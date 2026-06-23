#![allow(unused_imports)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::cast_precision_loss)]
use crate::CodeAnalyzer;
use crate::filters::{CompiledRule, apply_filter, load_filter_table, maybe_inject_no_stat};
use crate::logging::LogEvent;
use crate::shell::resolve_shell;
use crate::validation::{validate_path, validate_path_in_dir};
use crate::{
    EDIT_FAILURE_MAP_CAP, EDIT_STALE_THRESHOLD, ExecCommandParams, STDIN_MAX_BYTES, ShellOutput,
};
use aptu_coder_core::analyze;
use aptu_coder_core::cache::{AnalysisCache, CacheTier, CallGraphCache, CallGraphCacheKey};
use aptu_coder_core::formatter::{
    format_file_details_paginated, format_file_details_summary, format_focused_paginated,
    format_module_info, format_structure_paginated, format_summary,
};
use aptu_coder_core::formatter_defuse::format_focused_paginated_defuse;
use aptu_coder_core::pagination::{
    CursorData, DEFAULT_PAGE_SIZE, PaginationMode, decode_cursor, encode_cursor, paginate_slice,
};
use aptu_coder_core::parser::ParserError;
use aptu_coder_core::traversal::{
    WalkEntry, changed_files_from_git_ref, filter_entries_by_git_ref, walk_directory,
};
use aptu_coder_core::types::{
    AnalysisMode, AnalyzeDirectoryParams, AnalyzeFileParams, AnalyzeModuleParams,
    AnalyzeSymbolParams, EditOverwriteOutput, EditOverwriteParams, EditReplaceOutput,
    EditReplaceParams, SymbolMatchMode,
};
use aptu_coder_core::{cache, completion, graph, traversal, types};
use rmcp::handler::server::tool::{ToolRouter, schema_for_type};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, CancelledNotificationParam, CompleteRequestParams, CompleteResult,
    CompletionInfo, Content, ErrorData, Implementation, InitializeRequestParams, InitializeResult,
    LoggingLevel, LoggingMessageNotificationParam, Meta, Notification, ProgressNotificationParam,
    ProgressToken, ServerCapabilities, ServerNotification, SetLevelRequestParams,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{Peer, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as TokioMutex, RwLock, mpsc, watch};
use tracing::{instrument, warn};
use tracing_subscriber::filter::LevelFilter;

pub(crate) const SIZE_LIMIT: usize = 5_000;
pub(crate) const DEFAULT_DRAIN_TIMEOUT_MS: u64 = 500;

#[must_use]
pub fn summary_cursor_conflict(summary: Option<bool>, cursor: Option<&str>) -> bool {
    summary == Some(true) && cursor.is_some()
}

/// Session and client metadata recorded as span attributes on every tool call.
pub struct ClientMetadata {
    pub session_id: Option<String>,
    pub client_name: Option<String>,
    pub client_version: Option<String>,
}

/// Extract W3C Trace Context from MCP request _meta field and set as parent span context.
///
/// Attempts to extract traceparent and tracestate from the request's _meta field.
/// If successful, calls `set_parent` on the current tracing span so the OTel layer
/// re-parents it to the caller's trace. This must be called after the `#[instrument]`
/// span has been entered (i.e., inside the function body) for `set_parent` to take effect.
/// If extraction fails or _meta is absent, silently proceeds with root context (no panic).
pub fn extract_and_set_trace_context(
    meta: Option<&rmcp::model::Meta>,
    client_meta: ClientMetadata,
) {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let span = tracing::Span::current();

    // Record session and client attributes
    if let Some(sid) = client_meta.session_id {
        span.record("mcp.session.id", &sid);
    }
    if let Some(cn) = client_meta.client_name {
        span.record("client.name", &cn);
    }
    if let Some(cv) = client_meta.client_version {
        span.record("client.version", &cv);
    }

    // Extract agent-session-id from _meta if present (opportunistic; silent no-op if absent)
    if let Some(asi_str) = meta.and_then(|m| m.0.get("agent-session-id").and_then(|v| v.as_str())) {
        span.record("mcp.client.session.id", asi_str);
    }

    let Some(meta) = meta else { return };

    let mut propagation_map = std::collections::HashMap::new();

    // Extract traceparent if present
    if let Some(traceparent) = meta.0.get("traceparent")
        && let Some(tp_str) = traceparent.as_str()
    {
        propagation_map.insert("traceparent".to_string(), tp_str.to_string());
    }

    // Extract tracestate if present
    if let Some(tracestate) = meta.0.get("tracestate")
        && let Some(ts_str) = tracestate.as_str()
    {
        propagation_map.insert("tracestate".to_string(), ts_str.to_string());
    }

    // Only attempt extraction if we have at least traceparent
    if propagation_map.is_empty() {
        return;
    }

    // Extract context via the globally registered propagator (TraceContextPropagator by default)
    let parent_cx = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&ExtractMap(&propagation_map))
    });

    // Re-parent the current tracing span (already entered via #[instrument]) to the
    // extracted OTel context. set_parent is a no-op if the OTel layer is not installed.
    let _ = span.set_parent(parent_cx);
}

/// Helper struct for W3C Trace Context extraction from HashMap
struct ExtractMap<'a>(&'a std::collections::HashMap<String, String>);

impl<'a> opentelemetry::propagation::Extractor for ExtractMap<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorMeta {
    error_category: &'static str,
    is_retryable: bool,
    suggested_action: &'static str,
}

#[must_use]
pub(crate) fn error_meta(
    category: &'static str,
    is_retryable: bool,
    suggested_action: &'static str,
) -> serde_json::Value {
    serde_json::to_value(ErrorMeta {
        error_category: category,
        is_retryable,
        suggested_action,
    })
    .unwrap_or_default()
}

#[must_use]
pub(crate) fn err_to_tool_result(e: ErrorData) -> CallToolResult {
    let mut result =
        CallToolResult::error(vec![Content::text(e.message)]).with_meta(Some(no_cache_meta()));
    if let Some(data) = e.data {
        result.structured_content = Some(data);
    }
    result
}

pub(crate) fn err_to_tool_result_from_pagination(
    e: aptu_coder_core::pagination::PaginationError,
) -> CallToolResult {
    let msg = format!("Pagination error: {}", e);
    CallToolResult::error(vec![Content::text(msg)]).with_meta(Some(no_cache_meta()))
}

pub(crate) fn no_cache_meta() -> Meta {
    let mut m = serde_json::Map::new();
    m.insert(
        "cache_hint".to_string(),
        serde_json::Value::String("no-cache".to_string()),
    );
    Meta(m)
}

/// Helper function for paginating focus chains (callers or callees).
/// Returns (items, re-encoded_cursor_option).
pub(crate) fn paginate_focus_chains(
    chains: &[graph::InternalCallChain],
    mode: PaginationMode,
    offset: usize,
    page_size: usize,
) -> Result<(Vec<graph::InternalCallChain>, Option<String>), ErrorData> {
    let paginated = paginate_slice(chains, offset, page_size, mode).map_err(|e| {
        ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            e.to_string(),
            Some(error_meta("transient", true, "retry the request")),
        )
    })?;

    if paginated.next_cursor.is_none() && offset == 0 {
        return Ok((paginated.items, None));
    }

    let next = if let Some(raw_cursor) = paginated.next_cursor {
        let decoded = decode_cursor(&raw_cursor).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                e.to_string(),
                Some(error_meta("validation", false, "invalid cursor format")),
            )
        })?;
        Some(
            encode_cursor(&CursorData {
                mode,
                offset: decoded.offset,
            })
            .map_err(|e| {
                ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    e.to_string(),
                    Some(error_meta("validation", false, "invalid cursor format")),
                )
            })?,
        )
    } else {
        None
    };

    Ok((paginated.items, next))
}

/// MCP server handler that wires the four analysis tools to the rmcp transport.
#[derive(Clone)]
pub(crate) struct FocusedAnalysisParams {
    pub(crate) path: std::path::PathBuf,
    pub(crate) symbol: String,
    pub(crate) match_mode: SymbolMatchMode,
    pub(crate) follow_depth: u32,
    pub(crate) max_depth: Option<u32>,
    pub(crate) use_summary: bool,
    pub(crate) impl_only: Option<bool>,
    pub(crate) def_use: bool,
    pub(crate) parse_timeout_micros: Option<u64>,
}

pub(crate) struct ExecutionResult {
    pub(crate) exit_code: Option<i32>,
    pub(crate) output_truncated: bool,
    pub(crate) output_collection_error: Option<String>,
    pub(crate) timed_out: bool,
}

impl CodeAnalyzer {
    #[instrument(skip(self))]
    pub(crate) async fn emit_progress(
        &self,
        peer: Option<Peer<RoleServer>>,
        token: &ProgressToken,
        progress: f64,
        total: f64,
        message: String,
    ) {
        if let Some(peer) = peer {
            let notification = ServerNotification::ProgressNotification(Notification::new(
                ProgressNotificationParam {
                    progress_token: token.clone(),
                    progress,
                    total: Some(total),
                    message: Some(message),
                },
            ));
            if let Err(e) = peer.send_notification(notification).await {
                warn!("Failed to send progress notification: {}", e);
            }
        }
    }

    /// Emit a "received" metric event for the given tool name.
    /// Increments the session call sequence, locks the session ID, and sends
    /// the metric event via the channel. Returns the (seq, sid) pair for use
    /// by the caller in exit metrics, preserving per-call seq uniqueness.
    pub(crate) async fn emit_received_metric(&self, tool: &'static str) -> (u32, Option<String>) {
        // Relaxed: per-session monotonic counter; unique allocation is all that is
        // needed. No cross-thread happens-before required. Contrast:
        // GLOBAL_SESSION_COUNTER uses SeqCst for cross-session uniqueness.
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();
        self.metrics_tx.send(crate::metrics::MetricEvent {
            tool,
            result: "received",
            session_id: sid.clone(),
            seq: Some(seq),
            duration_ms: 0,
            ..Default::default()
        });
        (seq, sid)
    }

    /// Private helper: Extract analysis logic for overview mode (`analyze_directory`).
    /// Returns the complete analysis output and a cache_hit bool after spawning and monitoring progress.
    /// Cancels the blocking task when `ct` is triggered; returns an error on cancellation.
    #[allow(clippy::too_many_lines)] // long but cohesive analysis loop; extracting sub-functions would obscure the control flow
    #[allow(clippy::cast_precision_loss)] // progress percentage display; precision loss acceptable for usize counts
    #[instrument(skip(self, params, ct))]
    pub(crate) async fn handle_overview_mode(
        &self,
        params: &AnalyzeDirectoryParams,
        ct: tokio_util::sync::CancellationToken,
        progress_token: Option<ProgressToken>,
    ) -> Result<(std::sync::Arc<analyze::AnalysisOutput>, CacheTier), ErrorData> {
        let path = Path::new(&params.path);
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_clone = counter.clone();
        let path_owned = path.to_path_buf();
        let max_depth = params.max_depth;
        let ct_clone = ct.clone();

        // Bounded walk: pass max_depth directly so the walker stops at the right depth.
        let all_entries = walk_directory(path, params.max_depth).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Failed to walk directory: {e}"),
                Some(error_meta(
                    "resource",
                    false,
                    "check path permissions and availability",
                )),
            )
        })?;

        // Canonicalize max_depth: Some(0) is semantically identical to None (unlimited).
        let canonical_max_depth = max_depth.and_then(|d| if d == 0 { None } else { Some(d) });

        // Build cache key from all_entries (before depth filtering).
        // git_ref is included in the key so filtered and unfiltered results have distinct entries.
        let git_ref_val = params.git_ref.as_deref().filter(|s| !s.is_empty());
        let cache_key = cache::DirectoryCacheKey::from_entries(
            &all_entries,
            canonical_max_depth,
            AnalysisMode::Overview,
            git_ref_val,
        );

        // Check L1 cache
        if let Some(cached) = self.cache.get_directory(&cache_key) {
            tracing::debug!(cache_hit = true, message = "returning cached result");
            return Ok((cached, CacheTier::L1Memory));
        }

        // Compute disk cache key from canonical relative paths + mtime + params
        let root = std::path::Path::new(&params.path);
        let disk_key = {
            let mut hasher = blake3::Hasher::new();
            let mut sorted_entries: Vec<_> = all_entries.iter().collect();
            sorted_entries.sort_by(|a, b| a.path.cmp(&b.path));
            for entry in &sorted_entries {
                let rel = entry.path.strip_prefix(root).unwrap_or(&entry.path);
                hasher.update(rel.as_os_str().to_string_lossy().as_bytes());
                let mtime_secs = entry
                    .mtime
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                hasher.update(&mtime_secs.to_le_bytes());
            }
            if let Some(depth) = canonical_max_depth {
                hasher.update(depth.to_string().as_bytes());
            }
            if let Some(ref git_ref) = params.git_ref {
                hasher.update(git_ref.as_bytes());
            }
            hasher.finalize()
        };

        // Check L2 cache
        if let Some(cached) = self
            .disk_cache
            .get::<analyze::AnalysisOutput>("analyze_directory", &disk_key)
        {
            let arc = std::sync::Arc::new(cached);
            self.cache.put_directory(cache_key.clone(), arc.clone());
            return Ok((arc, CacheTier::L2Disk));
        }

        // Apply git_ref filter when requested (non-empty string only).
        let all_entries = if let Some(ref git_ref) = params.git_ref
            && !git_ref.is_empty()
        {
            let changed = changed_files_from_git_ref(path, git_ref).map_err(|e| {
                ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    format!("git_ref filter failed: {e}"),
                    Some(error_meta(
                        "resource",
                        false,
                        "ensure git is installed and path is inside a git repository",
                    )),
                )
            })?;
            filter_entries_by_git_ref(all_entries, &changed, path)
        } else {
            all_entries
        };

        // Compute subtree counts from the full entry set before filtering.
        let subtree_counts = if max_depth.is_some_and(|d| d > 0) {
            Some(traversal::subtree_counts_from_entries(path, &all_entries))
        } else {
            None
        };

        // Filter to depth-bounded subset for analysis.
        let entries: Vec<traversal::WalkEntry> = if let Some(depth) = max_depth
            && depth > 0
        {
            all_entries
                .into_iter()
                .filter(|e| e.depth <= depth as usize)
                .collect()
        } else {
            all_entries
        };

        // Get total file count for progress reporting
        let total_files = entries.iter().filter(|e| !e.is_dir).count();

        // Spawn blocking analysis with progress tracking
        let handle = tokio::task::spawn_blocking(move || {
            analyze::analyze_directory_with_progress(&path_owned, entries, counter_clone, ct_clone)
        });

        // Gate progress on client-supplied token; skip all machinery when absent.
        if let Some(ref token) = progress_token {
            let (tx, mut rx) = watch::channel(0usize);
            let peer = self.peer.lock().await.clone();
            let mut last_progress = 0usize;
            let mut cancelled = false;

            // Spawn a notifier that watches the counter and sends on the watch channel.
            let counter_notify = counter.clone();
            let tx_notify = tx.clone();
            let ct_notify = ct.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    if ct_notify.is_cancelled() {
                        break;
                    }
                    let current = counter_notify.load(std::sync::atomic::Ordering::Relaxed);
                    if tx_notify.send(current).is_err() {
                        break; // receiver dropped
                    }
                }
            });

            loop {
                tokio::select! {
                    _ = ct.cancelled() => {
                        cancelled = true;
                        break;
                    }
                    changed = rx.changed() => {
                        match changed {
                            Ok(()) => {
                                let current = *rx.borrow();
                                if current != last_progress && total_files > 0 {
                                    self.emit_progress(
                                        peer.clone(),
                                        token,
                                        current as f64,
                                        total_files as f64,
                                        format!("Analyzing {current}/{total_files} files"),
                                    )
                                    .await;
                                    last_progress = current;
                                }
                            }
                            Err(_) => {
                                // Sender dropped: analysis complete or notifier exited.
                                break;
                            }
                        }
                    }
                }
                if handle.is_finished() {
                    break;
                }
            }

            // Emit final 100% progress only if not cancelled
            if !cancelled && total_files > 0 {
                self.emit_progress(
                    peer.clone(),
                    token,
                    total_files as f64,
                    total_files as f64,
                    format!("Completed analyzing {total_files} files"),
                )
                .await;
            }
        }

        match handle.await {
            Ok(Ok(mut output)) => {
                output.subtree_counts = subtree_counts;
                let arc_output = std::sync::Arc::new(output);
                self.cache.put_directory(cache_key, arc_output.clone());
                // Spawn L2 write-behind; drain failure counter after write completes.
                {
                    let dc = self.disk_cache.clone();
                    let k = disk_key;
                    let v = arc_output.as_ref().clone();
                    let handle = tokio::task::spawn_blocking(move || {
                        dc.put("analyze_directory", &k, &v);
                        dc.drain_write_failures()
                    });
                    let metrics_tx = self.metrics_tx.clone();
                    let sid = self.session_id.lock().await.clone();
                    tokio::spawn(async move {
                        if let Ok(failures) = handle.await
                            && failures > 0
                        {
                            tracing::warn!(
                                tool = "analyze_directory",
                                failures,
                                "L2 disk cache write failed"
                            );
                            metrics_tx.send(
                                crate::metrics::MetricEventBuilder::new(
                                    "analyze_directory",
                                    "ok",
                                    0,
                                )
                                .session_id(sid)
                                .cache_write_failure(Some(true))
                                .build(),
                            );
                        }
                    });
                }
                Ok((arc_output, CacheTier::Miss))
            }
            Ok(Err(analyze::AnalyzeError::Cancelled)) => Err(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                "Analysis cancelled".to_string(),
                Some(error_meta("transient", true, "analysis was cancelled")),
            )),
            Ok(Err(e)) => Err(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Error analyzing directory: {e}"),
                Some(error_meta(
                    "resource",
                    false,
                    "check path and file permissions",
                )),
            )),
            Err(e) => Err(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Task join error: {e}"),
                Some(error_meta("transient", true, "retry the request")),
            )),
        }
    }

    /// Private helper: Extract analysis logic for file details mode (`analyze_file`).
    /// Returns the cached or newly analyzed file output along with a CacheTier.
    #[instrument(skip(self, params))]
    pub(crate) async fn handle_file_details_mode(
        &self,
        params: &AnalyzeFileParams,
    ) -> Result<(std::sync::Arc<analyze::FileAnalysisOutput>, CacheTier), ErrorData> {
        // Build cache key from file metadata
        let cache_key = std::fs::metadata(&params.path).ok().and_then(|meta| {
            meta.modified().ok().map(|mtime| cache::CacheKey {
                path: std::path::PathBuf::from(&params.path),
                modified: mtime,
                mode: AnalysisMode::FileDetails,
            })
        });

        // Check L1 cache first
        if let Some(ref key) = cache_key
            && let Some(cached) = self.cache.get(key)
        {
            tracing::debug!(cache_hit = true, message = "returning cached result");
            return Ok((cached, CacheTier::L1Memory));
        }

        // Compute disk cache key from file content
        let file_bytes = std::fs::read(&params.path).unwrap_or_default();
        let disk_key = blake3::hash(&file_bytes);

        // Check L2 cache
        if let Some(cached) = self
            .disk_cache
            .get::<analyze::FileAnalysisOutput>("analyze_file", &disk_key)
        {
            let arc = std::sync::Arc::new(cached);
            if let Some(ref key) = cache_key {
                self.cache.put(key.clone(), arc.clone());
            }
            return Ok((arc, CacheTier::L2Disk));
        }

        // Cache miss or no cache key, analyze and optionally store
        match analyze::analyze_file(&params.path, None) {
            Ok(output) => {
                let arc_output = std::sync::Arc::new(output);
                if let Some(key) = cache_key {
                    self.cache.put(key, arc_output.clone());
                }
                // Spawn L2 write-behind; drain failure counter after write completes.
                {
                    let dc = self.disk_cache.clone();
                    let k = disk_key;
                    let v = arc_output.as_ref().clone();
                    let handle = tokio::task::spawn_blocking(move || {
                        dc.put("analyze_file", &k, &v);
                        dc.drain_write_failures()
                    });
                    let metrics_tx = self.metrics_tx.clone();
                    let sid = self.session_id.lock().await.clone();
                    tokio::spawn(async move {
                        if let Ok(failures) = handle.await
                            && failures > 0
                        {
                            tracing::warn!(
                                tool = "analyze_file",
                                failures,
                                "L2 disk cache write failed"
                            );
                            metrics_tx.send(
                                crate::metrics::MetricEventBuilder::new("analyze_file", "ok", 0)
                                    .session_id(sid)
                                    .cache_write_failure(Some(true))
                                    .build(),
                            );
                        }
                    });
                }
                Ok((arc_output, CacheTier::Miss))
            }
            Err(e) => match &e {
                analyze::AnalyzeError::Parser(ParserError::UnsupportedLanguage(_)) => {
                    // Graceful fallback: reuse the file_bytes already read above for the
                    // cache key rather than re-reading the file (avoids a second I/O and
                    // the silent-empty-string risk of unwrap_or_default on a second read).
                    let source = String::from_utf8_lossy(&file_bytes);
                    let line_count = source.lines().count();
                    let ext = std::path::Path::new(&params.path)
                        .extension()
                        .and_then(|x| x.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let preview = source.lines().take(50).collect::<Vec<_>>().join("\n");
                    let formatted = format!(
                        "File: {path}\n[Unsupported extension: semantic analysis not available]\n\n{preview}",
                        path = params.path,
                    );
                    let output = analyze::FileAnalysisOutput::new(
                        formatted,
                        aptu_coder_core::types::SemanticAnalysis::default(),
                        line_count,
                        None,
                    );
                    let _ = ext;
                    let mut output = output;
                    output.unsupported = Some(true);
                    Ok((std::sync::Arc::new(output), CacheTier::Miss))
                }
                _ => Err(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    format!("Error analyzing file: {e}"),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )),
            },
        }
    }

    // Validate impl_only: only valid for directories that contain Rust source files.
    pub(crate) fn validate_impl_only(entries: &[WalkEntry]) -> Result<(), ErrorData> {
        let has_rust = entries.iter().any(|e| {
            !e.is_dir
                && e.path
                    .extension()
                    .and_then(|x: &std::ffi::OsStr| x.to_str())
                    == Some("rs")
        });

        if !has_rust {
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "impl_only=true requires Rust source files. No .rs files found in the given path. Use analyze_symbol without impl_only for cross-language analysis.".to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "remove impl_only or point to a directory containing .rs files",
                )),
            ));
        }
        Ok(())
    }

    /// Validate that `import_lookup=true` is accompanied by a non-empty symbol (the module path).
    pub(crate) fn validate_import_lookup(
        import_lookup: Option<bool>,
        symbol: &str,
    ) -> Result<(), ErrorData> {
        if import_lookup == Some(true) && symbol.is_empty() {
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "import_lookup=true requires symbol to contain the module path to search for"
                    .to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "set symbol to the module path when using import_lookup=true",
                )),
            ));
        }
        Ok(())
    }

    // Poll progress until analysis task completes.
    #[allow(clippy::cast_precision_loss, clippy::too_many_arguments)] // progress percentage display; precision loss acceptable for usize counts
    pub(crate) async fn poll_progress_until_done(
        &self,
        analysis_params: &FocusedAnalysisParams,
        counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        ct: tokio_util::sync::CancellationToken,
        entries: std::sync::Arc<Vec<WalkEntry>>,
        total_files: usize,
        symbol_display: &str,
        progress_token: Option<ProgressToken>,
    ) -> Result<analyze::FocusedAnalysisOutput, ErrorData> {
        let counter_clone = counter.clone();
        let ct_clone = ct.clone();
        let entries_clone = std::sync::Arc::clone(&entries);
        let path_owned = analysis_params.path.clone();
        let symbol_owned = analysis_params.symbol.clone();
        let match_mode_owned = analysis_params.match_mode.clone();
        let follow_depth = analysis_params.follow_depth;
        let max_depth = analysis_params.max_depth;
        let use_summary = analysis_params.use_summary;
        let impl_only = analysis_params.impl_only;
        let def_use = analysis_params.def_use;
        let parse_timeout_micros = analysis_params.parse_timeout_micros;
        let handle = tokio::task::spawn_blocking(move || {
            let params = analyze::FocusedAnalysisConfig {
                focus: symbol_owned,
                match_mode: match_mode_owned,
                follow_depth,
                max_depth,
                ast_recursion_limit: None,
                use_summary,
                impl_only,
                def_use,
                parse_timeout_micros,
            };
            analyze::analyze_focused_with_progress_with_entries(
                &path_owned,
                &params,
                &counter_clone,
                &ct_clone,
                &entries_clone,
            )
        });

        // Gate progress on client-supplied token; skip all machinery when absent.
        if let Some(ref token) = progress_token {
            let (tx, mut rx) = watch::channel(0usize);
            let peer = self.peer.lock().await.clone();
            let mut last_progress = 0usize;
            let mut cancelled = false;

            // Spawn a notifier that watches the counter and sends on the watch channel.
            let counter_notify = counter.clone();
            let tx_notify = tx.clone();
            let ct_notify = ct.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    if ct_notify.is_cancelled() {
                        break;
                    }
                    let current = counter_notify.load(std::sync::atomic::Ordering::Relaxed);
                    if tx_notify.send(current).is_err() {
                        break; // receiver dropped
                    }
                }
            });

            loop {
                tokio::select! {
                    _ = ct.cancelled() => {
                        cancelled = true;
                        break;
                    }
                    changed = rx.changed() => {
                        match changed {
                            Ok(()) => {
                                let current = *rx.borrow();
                                if current != last_progress && total_files > 0 {
                                    self.emit_progress(
                                        peer.clone(),
                                        token,
                                        current as f64,
                                        total_files as f64,
                                        format!(
                                            "Analyzing {current}/{total_files} files for symbol '{symbol_display}'"
                                        ),
                                    )
                                    .await;
                                    last_progress = current;
                                }
                            }
                            Err(_) => {
                                // Sender dropped: analysis complete or notifier exited.
                                break;
                            }
                        }
                    }
                }
                if handle.is_finished() {
                    break;
                }
            }

            if !cancelled && total_files > 0 {
                self.emit_progress(
                    peer.clone(),
                    token,
                    total_files as f64,
                    total_files as f64,
                    format!(
                        "Completed analyzing {total_files} files for symbol '{symbol_display}'"
                    ),
                )
                .await;
            }
        }

        match handle.await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(analyze::AnalyzeError::Cancelled)) => Err(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                "Analysis cancelled".to_string(),
                Some(error_meta("transient", true, "analysis was cancelled")),
            )),
            Ok(Err(e)) => Err(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Error analyzing symbol: {e}"),
                Some(error_meta("resource", false, "check symbol name and file")),
            )),
            Err(e) => Err(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Task join error: {e}"),
                Some(error_meta("transient", true, "retry the request")),
            )),
        }
    }

    // Run focused analysis with auto-summary retry on SIZE_LIMIT overflow.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_focused_with_auto_summary(
        &self,
        params: &AnalyzeSymbolParams,
        analysis_params: &FocusedAnalysisParams,
        counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        ct: tokio_util::sync::CancellationToken,
        entries: std::sync::Arc<Vec<WalkEntry>>,
        total_files: usize,
        progress_token: Option<ProgressToken>,
    ) -> Result<analyze::FocusedAnalysisOutput, ErrorData> {
        let use_summary_for_task = params.output_control.summary == Some(true);

        let analysis_params_initial = FocusedAnalysisParams {
            use_summary: use_summary_for_task,
            ..analysis_params.clone()
        };

        let mut output = self
            .poll_progress_until_done(
                &analysis_params_initial,
                counter.clone(),
                ct.clone(),
                entries.clone(),
                total_files,
                &params.symbol,
                progress_token.clone(),
            )
            .await?;

        if params.output_control.summary.is_none() && output.formatted.len() > SIZE_LIMIT {
            tracing::debug!(
                auto_summary = true,
                message = "output exceeded size limit, retrying with summary"
            );
            let counter2 = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let analysis_params_retry = FocusedAnalysisParams {
                use_summary: true,
                ..analysis_params.clone()
            };
            let summary_result = self
                .poll_progress_until_done(
                    &analysis_params_retry,
                    counter2,
                    ct,
                    entries,
                    total_files,
                    &params.symbol,
                    progress_token,
                )
                .await;

            if let Ok(summary_output) = summary_result {
                output.formatted = summary_output.formatted;
            } else {
                let estimated_tokens = output.formatted.len() / 4;
                let message = format!(
                    "Output exceeds 50K chars ({} chars, ~{} tokens). Use summary=true or narrow your scope.",
                    output.formatted.len(),
                    estimated_tokens
                );
                return Err(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    message,
                    Some(error_meta(
                        "validation",
                        false,
                        "use summary=true or narrow scope",
                    )),
                ));
            }
        } else if output.formatted.len() > SIZE_LIMIT
            && params.output_control.summary == Some(false)
        {
            let estimated_tokens = output.formatted.len() / 4;
            let message = format!(
                "Output exceeds 50K chars ({} chars, ~{} tokens). Use one of:\n\
                 - summary=true to get compact summary\n\
                 - Narrow your scope (smaller directory, specific file)",
                output.formatted.len(),
                estimated_tokens
            );
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                message,
                Some(error_meta(
                    "validation",
                    false,
                    "use summary=true or narrow scope",
                )),
            ));
        }

        Ok(output)
    }

    /// Private helper: Extract analysis logic for focused mode (`analyze_symbol`).
    /// Returns `(CacheTier, FocusedAnalysisOutput)` -- tier is `L1Memory` on cache hit,
    /// `Miss` on cache miss. Cancels the blocking task when `ct` is triggered.
    #[instrument(skip(self, params, ct))]
    pub(crate) async fn handle_focused_mode(
        &self,
        params: &AnalyzeSymbolParams,
        ct: tokio_util::sync::CancellationToken,
        progress_token: Option<ProgressToken>,
    ) -> Result<(CacheTier, analyze::FocusedAnalysisOutput), ErrorData> {
        let path = Path::new(&params.path);
        let raw_entries = match walk_directory(path, params.max_depth) {
            Ok(e) => e,
            Err(e) => {
                return Err(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    format!("Failed to walk directory: {e}"),
                    Some(error_meta(
                        "resource",
                        false,
                        "check path permissions and availability",
                    )),
                ));
            }
        };
        // Apply git_ref filter when requested (non-empty string only).
        let filtered_entries = if let Some(ref git_ref) = params.git_ref
            && !git_ref.is_empty()
        {
            let changed = changed_files_from_git_ref(path, git_ref).map_err(|e| {
                ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    format!("git_ref filter failed: {e}"),
                    Some(error_meta(
                        "resource",
                        false,
                        "ensure git is installed and path is inside a git repository",
                    )),
                )
            })?;
            filter_entries_by_git_ref(raw_entries, &changed, path)
        } else {
            raw_entries
        };
        let entries = std::sync::Arc::new(filtered_entries);

        if params.impl_only == Some(true) {
            Self::validate_impl_only(&entries)?;
        }

        // Build cache key for this call-graph request.
        let cache_key = CallGraphCacheKey::from_entries(
            path,
            &entries,
            params.git_ref.as_deref(),
            params.follow_depth.unwrap_or(1),
            &params.match_mode.clone().unwrap_or_default(),
            params.impl_only.unwrap_or(false),
            None,
        );

        // Check L1 cache first.
        if let Some(cached) = self.call_graph_cache.get(&cache_key) {
            return Ok((CacheTier::L1Memory, (*cached).clone()));
        }

        // Compute L2 disk cache key by streaming CallGraphCacheKey fields through blake3.
        // Same pattern as analyze_directory (lib.rs:591-617): root_path + git_ref +
        // follow_depth + match_mode + impl_only + per-file mtimes.
        let disk_key = {
            let mut hasher = blake3::Hasher::new();
            hasher.update(path.as_os_str().to_string_lossy().as_bytes());
            if let Some(ref git_ref) = params.git_ref {
                hasher.update(git_ref.as_bytes());
            }
            hasher.update(&params.follow_depth.unwrap_or(1).to_le_bytes());
            let match_mode_str =
                match serde_json::to_string(&params.match_mode.clone().unwrap_or_default()) {
                    Ok(s) => s,
                    Err(e) => {
                        // Serialization of a unit-like enum should never fail; if it does,
                        // an empty string would produce a non-unique cache key, so warn loudly.
                        tracing::warn!(
                            error = %e,
                            "analyze_symbol: failed to serialize match_mode for disk cache key; \
                             falling back to empty string (cache key may collide)"
                        );
                        String::new()
                    }
                };
            hasher.update(match_mode_str.as_bytes());
            hasher.update(&[u8::from(params.impl_only.unwrap_or(false))]);
            // Stream sorted per-file (path, mtime_nanos) pairs for freshness.
            let mut sorted_entries: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();
            sorted_entries.sort_by(|a, b| a.path.cmp(&b.path));
            for entry in &sorted_entries {
                // `path` is always a canonical absolute path (validated upstream by
                // validate_path before handle_focused_mode is called), so strip_prefix
                // succeeds for every entry under it. The unwrap_or fallback retains the
                // full absolute path, which is still unique and safe for hashing.
                let rel = entry.path.strip_prefix(path).unwrap_or(&entry.path);
                hasher.update(rel.as_os_str().to_string_lossy().as_bytes());
                let mtime_nanos = entry
                    .mtime
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                hasher.update(&mtime_nanos.to_le_bytes());
            }
            hasher.finalize()
        };

        // Check L2 disk cache.
        if let Some(cached) = self
            .disk_cache
            .get::<analyze::FocusedAnalysisOutput>("analyze_symbol", &disk_key)
        {
            let arc = std::sync::Arc::new(cached.clone());
            self.call_graph_cache.put(cache_key, arc);
            return Ok((CacheTier::L2Disk, cached));
        }

        let total_files = entries.iter().filter(|e| !e.is_dir).count();
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let analysis_params = FocusedAnalysisParams {
            path: path.to_path_buf(),
            symbol: params.symbol.clone(),
            match_mode: params.match_mode.clone().unwrap_or_default(),
            follow_depth: params.follow_depth.unwrap_or(1),
            max_depth: params.max_depth,
            use_summary: false,
            impl_only: params.impl_only,
            def_use: params.def_use.unwrap_or(false),
            parse_timeout_micros: None,
        };

        let mut output = self
            .run_focused_with_auto_summary(
                params,
                &analysis_params,
                counter,
                ct,
                entries,
                total_files,
                progress_token,
            )
            .await?;

        if params.impl_only == Some(true) {
            let filter_line = format!(
                "FILTER: impl_only=true ({} of {} callers shown)\n",
                output.impl_trait_caller_count, output.unfiltered_caller_count
            );
            output.formatted = format!("{}{}", filter_line, output.formatted);

            if output.impl_trait_caller_count == 0 {
                output.formatted.push_str(
                    "\nNOTE: No impl-trait callers found. The symbol may be a plain function or struct, not a trait method. Remove impl_only to see all callers.\n"
                );
            }
        }

        // Store in L1 cache for subsequent calls.
        self.call_graph_cache
            .put(cache_key, std::sync::Arc::new(output.clone()));

        // Spawn L2 write-behind; drain failure counter after write completes.
        {
            let dc = self.disk_cache.clone();
            let k = disk_key;
            let v = output.clone();
            let handle = tokio::task::spawn_blocking(move || {
                dc.put("analyze_symbol", &k, &v);
                dc.drain_write_failures()
            });
            let metrics_tx = self.metrics_tx.clone();
            let sid = self.session_id.lock().await.clone();
            tokio::spawn(async move {
                if let Ok(failures) = handle.await
                    && failures > 0
                {
                    tracing::warn!(
                        tool = "analyze_symbol",
                        failures,
                        "L2 disk cache write failed"
                    );
                    metrics_tx.send(
                        crate::metrics::MetricEventBuilder::new("analyze_symbol", "ok", 0)
                            .session_id(sid)
                            .cache_write_failure(Some(true))
                            .build(),
                    );
                }
            });
        }

        Ok((CacheTier::Miss, output))
    }
}

pub(crate) fn build_exec_command(
    command: &str,
    working_dir_path: Option<&std::path::PathBuf>,
    stdin_present: bool,
    resolved_path: Option<&str>,
) -> tokio::process::Command {
    let shell = resolve_shell();
    let mut cmd = tokio::process::Command::new(shell);

    // Unify command invocation: use -c on all platforms.
    // On macOS, the resolved PATH from the startup-captured login shell profile
    // is injected below, so -l is not needed per-command.
    cmd.arg("-c").arg(command);

    if let Some(wd) = working_dir_path {
        cmd.current_dir(wd);
    }

    // Inject resolved login shell PATH snapshot on all platforms.
    if let Some(path) = resolved_path {
        cmd.env("PATH", path);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if stdin_present {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }

    cmd
}

/// Strip a leading `cd <path> &&` prefix from a command string.
///
/// Returns `(stripped_command, Some(extracted_path))` when the command starts with
/// `cd <path> &&`. Returns `(cmd, None)` when no `cd ... &&` prefix is found.
///
/// Uses only `str` methods (no regex). Leading whitespace is trimmed before matching.
pub(crate) fn strip_cd_prefix(cmd: &str) -> (&str, Option<&str>) {
    let trimmed = cmd.trim_start();
    let Some(rest) = trimmed.strip_prefix("cd ") else {
        return (cmd, None);
    };
    // Find the && separator
    let Some((path_part, rest_part)) = rest.split_once("&&") else {
        return (cmd, None);
    };
    let path = path_part.trim();
    let stripped = rest_part.trim();
    (stripped, Some(path))
}

/// Result of a timed command execution.
pub(crate) async fn run_with_timeout(
    mut child: tokio::process::Child,
    tx: tokio::sync::mpsc::UnboundedSender<(bool, String)>,
    timeout_secs: Option<i64>,
    drain_timeout: std::time::Duration,
) -> ExecutionResult {
    use tokio::io::AsyncBufReadExt as _;
    use tokio_stream::StreamExt as TokioStreamExt;
    use tokio_stream::wrappers::LinesStream;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let drain_task = tokio::spawn(async move {
        let so_stream = stdout_pipe.map(|p| {
            LinesStream::new(tokio::io::BufReader::new(p).lines()).map(|l| l.map(|s| (false, s)))
        });
        let se_stream = stderr_pipe.map(|p| {
            LinesStream::new(tokio::io::BufReader::new(p).lines()).map(|l| l.map(|s| (true, s)))
        });

        match (so_stream, se_stream) {
            (Some(so), Some(se)) => {
                let mut merged = so.merge(se);
                while let Some(Ok((is_stderr, line))) = merged.next().await {
                    let _ = tx.send((is_stderr, line));
                }
            }
            (Some(so), None) => {
                let mut stream = so;
                while let Some(Ok((_, line))) = stream.next().await {
                    let _ = tx.send((false, line));
                }
            }
            (None, Some(se)) => {
                let mut stream = se;
                while let Some(Ok((_, line))) = stream.next().await {
                    let _ = tx.send((true, line));
                }
            }
            (None, None) => {}
        }
    });

    let drain_abort = drain_task.abort_handle();

    match timeout_secs {
        Some(secs) if secs > 0 => {
            // User timeout wraps only child.wait(); drain follows outside the timeout.
            let timeout_secs_u64 = u64::try_from(secs).unwrap_or(u64::MAX);
            let (exit_code, timed_out) = match tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs_u64),
                child.wait(),
            )
            .await
            {
                Ok(Ok(s)) => (s.code(), false),
                Ok(Err(_)) => (None, false),
                Err(_elapsed) => {
                    child.start_kill().ok();
                    // Reap the zombie so the OS does not accumulate a defunct child.
                    let _ = child.wait().await;
                    (None, true)
                }
            };

            // Drain remaining buffered output with drain_timeout grace (outside user timeout).
            let drain_truncated = if timed_out {
                drain_abort.abort();
                false
            } else {
                match tokio::time::timeout(drain_timeout, drain_task).await {
                    Ok(_) => false,
                    Err(_) => {
                        drain_abort.abort();
                        true
                    }
                }
            };

            let ocerr = if drain_truncated {
                Some("post-exit drain timeout: background process held pipes".to_string())
            } else {
                None
            };

            ExecutionResult {
                exit_code,
                output_truncated: drain_truncated,
                output_collection_error: ocerr,
                timed_out,
            }
        }
        _ => {
            // No user timeout: wait for child exit first, then drain buffered output
            // with a short grace period (drain_timeout) for background subprocesses.
            let exit_status = child.wait().await.ok();
            let drain_result = tokio::time::timeout(drain_timeout, drain_task).await;

            let drain_truncated = drain_result.is_err();
            if drain_truncated {
                drain_abort.abort();
            }
            let exit_code = exit_status.and_then(|s| s.code());
            let ocerr = if drain_truncated {
                Some("post-exit drain timeout: background process held pipes".to_string())
            } else {
                None
            };
            ExecutionResult {
                exit_code,
                output_truncated: drain_truncated,
                output_collection_error: ocerr,
                timed_out: false,
            }
        }
    }
}

/// Executes a shell command and returns the output.
/// This is a free async function (not a method) to allow use in moka::future::Cache::get_with().
/// It spawns the command, collects output, and persists output to slot files.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_exec_impl(
    command: String,
    working_dir_path: Option<std::path::PathBuf>,
    stdin: Option<String>,
    seq: u32,
    resolved_path: Option<&str>,
    filter_table: &Arc<Vec<CompiledRule>>,
    timeout_secs: Option<i64>,
    drain_timeout: std::time::Duration,
) -> ShellOutput {
    let mut cmd = build_exec_command(
        &command,
        working_dir_path.as_ref(),
        stdin.is_some(),
        resolved_path,
    );

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ShellOutput::new(
                String::new(),
                format!("failed to spawn command: {e}"),
                format!("failed to spawn command: {e}"),
                None,
                false,
            );
        }
    };

    if let Some(stdin_content) = stdin
        && let Some(mut stdin_handle) = child.stdin.take()
    {
        use tokio::io::AsyncWriteExt as _;
        match stdin_handle.write_all(stdin_content.as_bytes()).await {
            Ok(()) => {
                drop(stdin_handle);
            }
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => {
                warn!("failed to write stdin: {e}");
            }
        }
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(bool, String)>();

    let exec_result = run_with_timeout(child, tx, timeout_secs, drain_timeout).await;
    let exit_code = exec_result.exit_code;
    let mut output_truncated = exec_result.output_truncated;
    let output_collection_error = exec_result.output_collection_error;
    let timed_out = exec_result.timed_out;

    rx.close();

    let mut lines: Vec<(bool, String)> = Vec::new();
    while let Some(item) = rx.recv().await {
        lines.push(item);
    }

    // Split tagged lines into stdout, stderr, interleaved post-facto (no locks needed).
    const MAX_BYTES: usize = 50 * 1024;
    let mut stdout_str = String::new();
    let mut stderr_str = String::new();
    let mut interleaved_str = String::new();
    let mut so_bytes = 0usize;
    let mut se_bytes = 0usize;
    let mut il_bytes = 0usize;
    for (is_stderr, line) in &lines {
        let entry = format!("{line}\n");
        if il_bytes < 2 * MAX_BYTES {
            il_bytes += entry.len();
            interleaved_str.push_str(&entry);
        }
        if *is_stderr {
            if se_bytes < MAX_BYTES {
                se_bytes += entry.len();
                stderr_str.push_str(&entry);
            }
        } else if so_bytes < MAX_BYTES {
            so_bytes += entry.len();
            stdout_str.push_str(&entry);
        }
    }

    let slot = seq % 8;
    let (stdout, stderr, stdout_path, stderr_path, byte_truncated) =
        handle_output_persist(stdout_str, stderr_str, slot);
    output_truncated = output_truncated || stdout_path.is_some() || byte_truncated;

    let mut output = ShellOutput::new(stdout, stderr, interleaved_str, exit_code, output_truncated);
    output.output_collection_error = output_collection_error;
    output.stdout_path = stdout_path;
    output.stderr_path = stderr_path;
    output.timed_out = timed_out;

    // Apply filter if exit_code == 0
    if exit_code == Some(0) {
        for compiled_rule in filter_table.iter() {
            if compiled_rule.pattern.is_match(&command) {
                let filtered_stdout = apply_filter(compiled_rule, &output.stdout);
                output.stdout = filtered_stdout;
                // Also filter interleaved: the response handler prefers interleaved when
                // non-empty (which it always is for commands that write to both streams),
                // so filtering only stdout would leave the LLM-visible output unfiltered.
                // apply_filter is called separately on each field; there is no double-filtering
                // because stdout and interleaved are independent strings assembled from the
                // same source lines -- updating one does not affect the other.
                output.interleaved = apply_filter(compiled_rule, &output.interleaved);
                output.filter_applied = compiled_rule
                    .rule
                    .description
                    .clone()
                    .or_else(|| Some(compiled_rule.rule.match_command.clone()));
                break;
            }
        }
    }

    output
}

/// Handles output persistence by writing to slot files only when output overflows the line limit.
/// Writes full stdout/stderr to:
///   {temp_dir}/aptu-coder-overflow/slot-{slot}/{stdout,stderr}
/// Returns (stdout_out, stderr_out, stdout_path, stderr_path).
/// On overflow: truncates to last 50 lines and sets paths to Some.
/// Under limit: returns output unchanged and paths as None (no I/O).
pub(crate) fn handle_output_persist(
    stdout: String,
    stderr: String,
    slot: u32,
) -> (String, String, Option<String>, Option<String>, bool) {
    const MAX_OUTPUT_LINES: usize = 2000;
    // Sized at p99.3 of observed exec_command output_chars (27k calls): 99.27% of calls are
    // under 20k chars; raising to 30k covers 99.67% while still capping pathological cases
    // (git pull on large repos, cargo test on large workspaces) that exceed 100k chars.
    const MAX_STDOUT_BYTES: usize = 30_000;
    const MAX_STDERR_BYTES: usize = 10_000;
    const OVERFLOW_PREVIEW_LINES: usize = 50;

    let stdout_lines: Vec<&str> = stdout.lines().collect();
    let stderr_lines: Vec<&str> = stderr.lines().collect();

    let mut byte_truncated = false;

    // Check for line overflow or byte overflow
    let line_overflow =
        stdout_lines.len() > MAX_OUTPUT_LINES || stderr_lines.len() > MAX_OUTPUT_LINES;
    let stdout_byte_overflow = stdout.len() > MAX_STDOUT_BYTES;
    let stderr_byte_overflow = stderr.len() > MAX_STDERR_BYTES;
    let byte_overflow = stdout_byte_overflow || stderr_byte_overflow;

    // No overflow: return as-is with no I/O.
    if !line_overflow && !byte_overflow {
        return (stdout, stderr, None, None, false);
    }

    // Overflow: write slot files and return last-N-lines preview.
    let base = std::env::temp_dir()
        .join("aptu-coder-overflow")
        .join(format!("slot-{slot}"));
    let _ = std::fs::create_dir_all(&base);

    let stdout_path = base.join("stdout");
    let stderr_path = base.join("stderr");

    let _ = std::fs::write(&stdout_path, stdout.as_bytes());
    let _ = std::fs::write(&stderr_path, stderr.as_bytes());

    let stdout_path_str = stdout_path.display().to_string();
    let stderr_path_str = stderr_path.display().to_string();

    // Truncate stdout if it exceeds byte limit
    let stdout_preview = if stdout_byte_overflow {
        byte_truncated = true;
        // Use char-boundary-safe tail truncation
        let tail_start = stdout.len().saturating_sub(MAX_STDOUT_BYTES);
        let safe_start = stdout[..tail_start].floor_char_boundary(tail_start);
        stdout[safe_start..].to_string()
    } else if stdout_lines.len() > MAX_OUTPUT_LINES {
        stdout_lines[stdout_lines.len().saturating_sub(OVERFLOW_PREVIEW_LINES)..].join("\n")
    } else {
        stdout
    };

    // Truncate stderr if it exceeds byte limit
    let stderr_preview = if stderr_byte_overflow {
        byte_truncated = true;
        // Use char-boundary-safe tail truncation
        let tail_start = stderr.len().saturating_sub(MAX_STDERR_BYTES);
        let safe_start = stderr[..tail_start].floor_char_boundary(tail_start);
        stderr[safe_start..].to_string()
    } else if stderr_lines.len() > MAX_OUTPUT_LINES {
        stderr_lines[stderr_lines.len().saturating_sub(OVERFLOW_PREVIEW_LINES)..].join("\n")
    } else {
        stderr
    };

    (
        stdout_preview,
        stderr_preview,
        Some(stdout_path_str),
        Some(stderr_path_str),
        byte_truncated,
    )
}

/// Truncates output to a maximum number of lines and bytes.
/// Returns (truncated_output, was_truncated).
pub(crate) fn disable_routes(router: &mut ToolRouter<CodeAnalyzer>, tools: &[&'static str]) {
    for tool in tools {
        router.disable_route(*tool);
    }
}
