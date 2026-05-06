// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Rust MCP server for code structure analysis using tree-sitter.
//!
//! This crate exposes four MCP tools for multiple programming languages:
//!
//! - **`analyze_directory`**: Directory tree with file counts and structure
//! - **`analyze_file`**: Semantic extraction (functions, classes, imports)
//! - **`analyze_symbol`**: Call graph analysis (callers and callees)
//! - **`analyze_module`**: Lightweight function and import index
//!
//! Key entry points:
//! - [`analyze::analyze_directory`]: Analyze entire directory tree
//! - [`analyze::analyze_file`]: Analyze single file
//!
//! Languages supported: Rust, Go, Java, Python, TypeScript, TSX, Fortran, JavaScript, C/C++, C#.

pub mod logging;
pub mod metrics;

pub use aptu_coder_core::analyze;
use aptu_coder_core::{cache, completion, graph, traversal, types};

pub(crate) const EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    ".git",
    "__pycache__",
    "target",
    "dist",
    "build",
    ".venv",
];

use aptu_coder_core::cache::AnalysisCache;
use aptu_coder_core::formatter::{
    format_file_details_paginated, format_file_details_summary, format_focused_paginated,
    format_module_info, format_structure_paginated, format_summary,
};
use aptu_coder_core::formatter_defuse::format_focused_paginated_defuse;
use aptu_coder_core::pagination::{
    CursorData, DEFAULT_PAGE_SIZE, PaginationMode, decode_cursor, encode_cursor, paginate_slice,
};
use aptu_coder_core::traversal::{
    WalkEntry, changed_files_from_git_ref, filter_entries_by_git_ref, walk_directory,
};
use aptu_coder_core::types::{
    AnalysisMode, AnalyzeDirectoryParams, AnalyzeFileParams, AnalyzeModuleParams,
    AnalyzeSymbolParams, EditInsertOutput, EditInsertParams, EditOverwriteOutput,
    EditOverwriteParams, EditRenameOutput, EditRenameParams, EditReplaceOutput, EditReplaceParams,
    SymbolMatchMode,
};
use aptu_coder_core::{edit_insert_at_symbol, edit_rename_in_file};
use logging::LogEvent;
use rmcp::handler::server::tool::{ToolRouter, schema_for_type};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, CancelledNotificationParam, CompleteRequestParams, CompleteResult,
    CompletionInfo, Content, ErrorData, Implementation, InitializeResult, LoggingLevel,
    LoggingMessageNotificationParam, Meta, Notification, NumberOrString, ProgressNotificationParam,
    ProgressToken, ServerCapabilities, ServerNotification, SetLevelRequestParams,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{Peer, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tracing::{instrument, warn};
use tracing_subscriber::filter::LevelFilter;

static GLOBAL_SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

const SIZE_LIMIT: usize = 50_000;

/// Returns `true` when `summary=true` and a `cursor` are both provided, which is an invalid
/// combination since summary mode and pagination are mutually exclusive.
#[must_use]
pub fn summary_cursor_conflict(summary: Option<bool>, cursor: Option<&str>) -> bool {
    summary == Some(true) && cursor.is_some()
}

#[must_use]
fn error_meta(
    category: &'static str,
    is_retryable: bool,
    suggested_action: &'static str,
) -> serde_json::Value {
    serde_json::json!({
        "errorCategory": category,
        "isRetryable": is_retryable,
        "suggestedAction": suggested_action,
    })
}

#[must_use]
fn err_to_tool_result(e: ErrorData) -> CallToolResult {
    CallToolResult::error(vec![Content::text(e.message)])
}

fn err_to_tool_result_from_pagination(
    e: aptu_coder_core::pagination::PaginationError,
) -> CallToolResult {
    let msg = format!("Pagination error: {}", e);
    CallToolResult::error(vec![Content::text(msg)])
}

fn no_cache_meta() -> Meta {
    let mut m = serde_json::Map::new();
    m.insert(
        "cache_hint".to_string(),
        serde_json::Value::String("no-cache".to_string()),
    );
    Meta(m)
}

/// Validates that a path is within the current working directory.
/// For `require_exists=true`, the path must exist and be canonicalizable.
/// For `require_exists=false`, the parent directory must exist and be canonicalizable.
fn validate_path(path: &str, require_exists: bool) -> Result<std::path::PathBuf, ErrorData> {
    // Canonicalize the allowed root (CWD) to resolve symlinks
    let allowed_root = std::fs::canonicalize(std::env::current_dir().map_err(|_| {
        ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is outside the allowed root".to_string(),
            Some(error_meta(
                "validation",
                false,
                "ensure the working directory is accessible",
            )),
        )
    })?)
    .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());

    let canonical_path = if require_exists {
        std::fs::canonicalize(path).map_err(|e| {
            let msg = match e.kind() {
                std::io::ErrorKind::NotFound => format!("path not found: {path}"),
                std::io::ErrorKind::PermissionDenied => format!("permission denied: {path}"),
                _ => "path is outside the allowed root".to_string(),
            };
            ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                msg,
                Some(error_meta(
                    "validation",
                    false,
                    "provide a valid path within the working directory",
                )),
            )
        })?
    } else {
        // For non-existent files (edit_overwrite), walk up the path until we find an existing ancestor
        let p = std::path::Path::new(path);
        let mut ancestor = p.to_path_buf();
        let mut suffix = std::path::PathBuf::new();

        loop {
            if ancestor.exists() {
                break;
            }
            if let Some(parent) = ancestor.parent() {
                if let Some(file_name) = ancestor.file_name() {
                    suffix = std::path::PathBuf::from(file_name).join(&suffix);
                }
                ancestor = parent.to_path_buf();
            } else {
                // No existing ancestor found — use allowed_root as anchor
                ancestor = allowed_root.clone();
                break;
            }
        }

        let canonical_base =
            std::fs::canonicalize(&ancestor).unwrap_or_else(|_| allowed_root.clone());
        canonical_base.join(&suffix)
    };

    if !canonical_path.starts_with(&allowed_root) {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is outside the allowed root".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a path within the current working directory",
            )),
        ));
    }

    Ok(canonical_path)
}

/// Helper function for paginating focus chains (callers or callees).
/// Returns (items, re-encoded_cursor_option).
fn paginate_focus_chains(
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
///
/// Holds shared state: tool router, analysis cache, peer connection, log-level filter,
/// log event channel, metrics sender, and per-session sequence tracking.
#[derive(Clone)]
pub struct CodeAnalyzer {
    // Accessed by rmcp macro-generated tool dispatch, but this field still triggers
    // `dead_code` in this crate, so keep the targeted suppression.
    #[allow(dead_code)]
    pub(crate) tool_router: ToolRouter<Self>,
    cache: AnalysisCache,
    peer: Arc<TokioMutex<Option<Peer<RoleServer>>>>,
    log_level_filter: Arc<Mutex<LevelFilter>>,
    event_rx: Arc<TokioMutex<Option<mpsc::UnboundedReceiver<LogEvent>>>>,
    metrics_tx: crate::metrics::MetricsSender,
    session_call_seq: Arc<std::sync::atomic::AtomicU32>,
    session_id: Arc<TokioMutex<Option<String>>>,
}

#[tool_router]
impl CodeAnalyzer {
    #[must_use]
    pub fn list_tools() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(
        peer: Arc<TokioMutex<Option<Peer<RoleServer>>>>,
        log_level_filter: Arc<Mutex<LevelFilter>>,
        event_rx: mpsc::UnboundedReceiver<LogEvent>,
        metrics_tx: crate::metrics::MetricsSender,
    ) -> Self {
        let file_cap: usize = std::env::var("CODE_ANALYZE_FILE_CACHE_CAPACITY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);
        CodeAnalyzer {
            tool_router: Self::tool_router(),
            cache: AnalysisCache::new(file_cap),
            peer,
            log_level_filter,
            event_rx: Arc::new(TokioMutex::new(Some(event_rx))),
            metrics_tx,
            session_call_seq: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            session_id: Arc::new(TokioMutex::new(None)),
        }
    }

    #[instrument(skip(self))]
    async fn emit_progress(
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

    /// Private helper: Extract analysis logic for overview mode (`analyze_directory`).
    /// Returns the complete analysis output and a cache_hit bool after spawning and monitoring progress.
    /// Cancels the blocking task when `ct` is triggered; returns an error on cancellation.
    #[allow(clippy::too_many_lines)] // long but cohesive analysis loop; extracting sub-functions would obscure the control flow
    #[allow(clippy::cast_precision_loss)] // progress percentage display; precision loss acceptable for usize counts
    #[instrument(skip(self, params, ct))]
    async fn handle_overview_mode(
        &self,
        params: &AnalyzeDirectoryParams,
        ct: tokio_util::sync::CancellationToken,
    ) -> Result<(std::sync::Arc<analyze::AnalysisOutput>, bool), ErrorData> {
        let path = Path::new(&params.path);
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_clone = counter.clone();
        let path_owned = path.to_path_buf();
        let max_depth = params.max_depth;
        let ct_clone = ct.clone();

        // Single unbounded walk; filter in-memory to respect max_depth for analysis.
        let all_entries = walk_directory(path, None).map_err(|e| {
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

        // Check cache
        if let Some(cached) = self.cache.get_directory(&cache_key) {
            return Ok((cached, true));
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

        // Poll and emit progress every 100ms
        let token = ProgressToken(NumberOrString::String(
            format!(
                "analyze-overview-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            )
            .into(),
        ));
        let peer = self.peer.lock().await.clone();
        let mut last_progress = 0usize;
        let mut cancelled = false;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if ct.is_cancelled() {
                cancelled = true;
                break;
            }
            let current = counter.load(std::sync::atomic::Ordering::Relaxed);
            if current != last_progress && total_files > 0 {
                self.emit_progress(
                    peer.clone(),
                    &token,
                    current as f64,
                    total_files as f64,
                    format!("Analyzing {current}/{total_files} files"),
                )
                .await;
                last_progress = current;
            }
            if handle.is_finished() {
                break;
            }
        }

        // Emit final 100% progress only if not cancelled
        if !cancelled && total_files > 0 {
            self.emit_progress(
                peer.clone(),
                &token,
                total_files as f64,
                total_files as f64,
                format!("Completed analyzing {total_files} files"),
            )
            .await;
        }

        match handle.await {
            Ok(Ok(mut output)) => {
                output.subtree_counts = subtree_counts;
                let arc_output = std::sync::Arc::new(output);
                self.cache.put_directory(cache_key, arc_output.clone());
                Ok((arc_output, false))
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
    /// Returns the cached or newly analyzed file output along with a cache_hit bool.
    #[instrument(skip(self, params))]
    async fn handle_file_details_mode(
        &self,
        params: &AnalyzeFileParams,
    ) -> Result<(std::sync::Arc<analyze::FileAnalysisOutput>, bool), ErrorData> {
        // Build cache key from file metadata
        let cache_key = std::fs::metadata(&params.path).ok().and_then(|meta| {
            meta.modified().ok().map(|mtime| cache::CacheKey {
                path: std::path::PathBuf::from(&params.path),
                modified: mtime,
                mode: AnalysisMode::FileDetails,
            })
        });

        // Check cache first
        if let Some(ref key) = cache_key
            && let Some(cached) = self.cache.get(key)
        {
            return Ok((cached, true));
        }

        // Cache miss or no cache key, analyze and optionally store
        match analyze::analyze_file(&params.path, params.ast_recursion_limit) {
            Ok(output) => {
                let arc_output = std::sync::Arc::new(output);
                if let Some(key) = cache_key {
                    self.cache.put(key, arc_output.clone());
                }
                Ok((arc_output, false))
            }
            Err(e) => Err(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Error analyzing file: {e}"),
                Some(error_meta(
                    "resource",
                    false,
                    "check file path and permissions",
                )),
            )),
        }
    }

    // Validate impl_only: only valid for directories that contain Rust source files.
    fn validate_impl_only(entries: &[WalkEntry]) -> Result<(), ErrorData> {
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
    fn validate_import_lookup(import_lookup: Option<bool>, symbol: &str) -> Result<(), ErrorData> {
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
    #[allow(clippy::cast_precision_loss)] // progress percentage display; precision loss acceptable for usize counts
    async fn poll_progress_until_done(
        &self,
        analysis_params: &FocusedAnalysisParams,
        counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        ct: tokio_util::sync::CancellationToken,
        entries: std::sync::Arc<Vec<WalkEntry>>,
        total_files: usize,
        symbol_display: &str,
    ) -> Result<analyze::FocusedAnalysisOutput, ErrorData> {
        let counter_clone = counter.clone();
        let ct_clone = ct.clone();
        let entries_clone = std::sync::Arc::clone(&entries);
        let path_owned = analysis_params.path.clone();
        let symbol_owned = analysis_params.symbol.clone();
        let match_mode_owned = analysis_params.match_mode.clone();
        let follow_depth = analysis_params.follow_depth;
        let max_depth = analysis_params.max_depth;
        let ast_recursion_limit = analysis_params.ast_recursion_limit;
        let use_summary = analysis_params.use_summary;
        let impl_only = analysis_params.impl_only;
        let def_use = analysis_params.def_use;
        let handle = tokio::task::spawn_blocking(move || {
            let params = analyze::FocusedAnalysisConfig {
                focus: symbol_owned,
                match_mode: match_mode_owned,
                follow_depth,
                max_depth,
                ast_recursion_limit,
                use_summary,
                impl_only,
                def_use,
            };
            analyze::analyze_focused_with_progress_with_entries(
                &path_owned,
                &params,
                &counter_clone,
                &ct_clone,
                &entries_clone,
            )
        });

        let token = ProgressToken(NumberOrString::String(
            format!(
                "analyze-symbol-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            )
            .into(),
        ));
        let peer = self.peer.lock().await.clone();
        let mut last_progress = 0usize;
        let mut cancelled = false;

        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if ct.is_cancelled() {
                cancelled = true;
                break;
            }
            let current = counter.load(std::sync::atomic::Ordering::Relaxed);
            if current != last_progress && total_files > 0 {
                self.emit_progress(
                    peer.clone(),
                    &token,
                    current as f64,
                    total_files as f64,
                    format!(
                        "Analyzing {current}/{total_files} files for symbol '{symbol_display}'"
                    ),
                )
                .await;
                last_progress = current;
            }
            if handle.is_finished() {
                break;
            }
        }

        if !cancelled && total_files > 0 {
            self.emit_progress(
                peer.clone(),
                &token,
                total_files as f64,
                total_files as f64,
                format!("Completed analyzing {total_files} files for symbol '{symbol_display}'"),
            )
            .await;
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
    async fn run_focused_with_auto_summary(
        &self,
        params: &AnalyzeSymbolParams,
        analysis_params: &FocusedAnalysisParams,
        counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        ct: tokio_util::sync::CancellationToken,
        entries: std::sync::Arc<Vec<WalkEntry>>,
        total_files: usize,
    ) -> Result<analyze::FocusedAnalysisOutput, ErrorData> {
        let use_summary_for_task = params.output_control.force != Some(true)
            && params.output_control.summary == Some(true);

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
            )
            .await?;

        if params.output_control.summary.is_none()
            && params.output_control.force != Some(true)
            && output.formatted.len() > SIZE_LIMIT
        {
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
                )
                .await;

            if let Ok(summary_output) = summary_result {
                output.formatted = summary_output.formatted;
            } else {
                let estimated_tokens = output.formatted.len() / 4;
                let message = format!(
                    "Output exceeds 50K chars ({} chars, ~{} tokens). Use summary=true or force=true.",
                    output.formatted.len(),
                    estimated_tokens
                );
                return Err(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    message,
                    Some(error_meta(
                        "validation",
                        false,
                        "use summary=true or force=true",
                    )),
                ));
            }
        } else if output.formatted.len() > SIZE_LIMIT
            && params.output_control.force != Some(true)
            && params.output_control.summary == Some(false)
        {
            let estimated_tokens = output.formatted.len() / 4;
            let message = format!(
                "Output exceeds 50K chars ({} chars, ~{} tokens). Use one of:\n\
                 - force=true to return full output\n\
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
                    "use force=true, summary=true, or narrow scope",
                )),
            ));
        }

        Ok(output)
    }

    /// Private helper: Extract analysis logic for focused mode (`analyze_symbol`).
    /// Returns the complete focused analysis output after spawning and monitoring progress.
    /// Cancels the blocking task when `ct` is triggered; returns an error on cancellation.
    #[instrument(skip(self, params, ct))]
    async fn handle_focused_mode(
        &self,
        params: &AnalyzeSymbolParams,
        ct: tokio_util::sync::CancellationToken,
    ) -> Result<analyze::FocusedAnalysisOutput, ErrorData> {
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

        let total_files = entries.iter().filter(|e| !e.is_dir).count();
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let analysis_params = FocusedAnalysisParams {
            path: path.to_path_buf(),
            symbol: params.symbol.clone(),
            match_mode: params.match_mode.clone().unwrap_or_default(),
            follow_depth: params.follow_depth.unwrap_or(1),
            max_depth: params.max_depth,
            ast_recursion_limit: params.ast_recursion_limit,
            use_summary: false,
            impl_only: params.impl_only,
            def_use: params.def_use.unwrap_or(false),
        };

        let mut output = self
            .run_focused_with_auto_summary(
                params,
                &analysis_params,
                counter,
                ct,
                entries,
                total_files,
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

        Ok(output)
    }

    #[instrument(skip(self, context))]
    #[tool(
        name = "analyze_directory",
        description = "Tree-view of directory with LOC, function/class counts, test markers. Respects .gitignore. Returns per-file stats plus next_cursor for pagination. Fails if summary=true and cursor. For 1000+ files, use max_depth=2-3 and summary=true. git_ref restricts to files changed since a branch/tag/commit. Empty directories return zero counts. Example queries: Analyze the src/ directory to understand module structure; What files are in the tests/ directory and how large are they?",
        output_schema = schema_for_type::<analyze::AnalysisOutput>(),
        annotations(
            title = "Analyze Directory",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn analyze_directory(
        &self,
        params: Parameters<AnalyzeDirectoryParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        let ct = context.ct.clone();
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let max_depth_val = params.max_depth;
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        // Call handler for analysis and progress tracking
        let (arc_output, dir_cache_hit) = match self.handle_overview_mode(&params, ct).await {
            Ok(v) => v,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        // Extract the value from Arc for modification. On a cache hit the Arc is shared,
        // so try_unwrap may fail; fall back to cloning the underlying value in that case.
        let mut output = match std::sync::Arc::try_unwrap(arc_output) {
            Ok(owned) => owned,
            Err(arc) => (*arc).clone(),
        };

        // summary=true (explicit) and cursor are mutually exclusive.
        // Auto-summarization (summary=None + large output) must NOT block cursor pagination.
        if summary_cursor_conflict(
            params.output_control.summary,
            params.pagination.cursor.as_deref(),
        ) {
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "summary=true is incompatible with a pagination cursor; use one or the other"
                    .to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "remove cursor or set summary=false",
                )),
            )));
        }

        // Apply summary/output size limiting logic
        let use_summary = if params.output_control.force == Some(true) {
            false
        } else if params.output_control.summary == Some(true) {
            true
        } else if params.output_control.summary == Some(false) {
            false
        } else {
            output.formatted.len() > SIZE_LIMIT
        };

        if use_summary {
            output.formatted = format_summary(
                &output.entries,
                &output.files,
                params.max_depth,
                output.subtree_counts.as_deref(),
            );
        }

        // Decode pagination cursor if provided
        let page_size = params.pagination.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        let offset = if let Some(ref cursor_str) = params.pagination.cursor {
            let cursor_data = match decode_cursor(cursor_str).map_err(|e| {
                ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    e.to_string(),
                    Some(error_meta("validation", false, "invalid cursor format")),
                )
            }) {
                Ok(v) => v,
                Err(e) => return Ok(err_to_tool_result(e)),
            };
            cursor_data.offset
        } else {
            0
        };

        // Apply pagination to files
        let paginated =
            match paginate_slice(&output.files, offset, page_size, PaginationMode::Default) {
                Ok(v) => v,
                Err(e) => {
                    return Ok(err_to_tool_result(ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        e.to_string(),
                        Some(error_meta("transient", true, "retry the request")),
                    )));
                }
            };

        let verbose = params.output_control.verbose.unwrap_or(false);
        if !use_summary {
            output.formatted = format_structure_paginated(
                &paginated.items,
                paginated.total,
                params.max_depth,
                Some(Path::new(&params.path)),
                verbose,
            );
        }

        // Update next_cursor in output after pagination (unless using summary mode)
        if use_summary {
            output.next_cursor = None;
        } else {
            output.next_cursor.clone_from(&paginated.next_cursor);
        }

        // Build final text output with pagination cursor if present (unless using summary mode)
        let mut final_text = output.formatted.clone();
        if !use_summary && let Some(cursor) = paginated.next_cursor {
            final_text.push('\n');
            final_text.push_str("NEXT_CURSOR: ");
            final_text.push_str(&cursor);
        }

        let mut result = CallToolResult::success(vec![Content::text(final_text.clone())])
            .with_meta(Some(no_cache_meta()));
        let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(crate::metrics::MetricEvent {
            ts: crate::metrics::unix_ms(),
            tool: "analyze_directory",
            duration_ms: dur,
            output_chars: final_text.len(),
            param_path_depth: crate::metrics::path_component_count(&param_path),
            max_depth: max_depth_val,
            result: "ok",
            error_type: None,
            session_id: sid,
            seq: Some(seq),
            cache_hit: Some(dir_cache_hit),
        });
        Ok(result)
    }

    #[instrument(skip(self, _context))]
    #[tool(
        name = "analyze_file",
        description = "Functions, types, classes, and imports from a single source file. Returns functions (name, signature, line range), classes (methods, fields, inheritance), imports; paginate with cursor/page_size. Use fields=[\"functions\",\"classes\",\"imports\"] to limit output sections. Fails if directory path supplied; use analyze_directory instead. Fails if summary=true and cursor. git_ref not supported for single-file analysis. Use analyze_module for lightweight function/import index (~75% smaller). Supported: Rust, Go, Java, Python, TypeScript, TSX, Fortran, JavaScript, C/C++, C#. Example queries: What functions are defined in src/lib.rs?; Show me the classes and their methods in src/analyzer.py.",
        output_schema = schema_for_type::<analyze::FileAnalysisOutput>(),
        annotations(
            title = "Analyze File",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn analyze_file(
        &self,
        params: Parameters<AnalyzeFileParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        // Check if path is a directory (not allowed for analyze_file)
        if std::path::Path::new(&params.path).is_dir() {
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                format!(
                    "'{}' is a directory; use analyze_directory instead",
                    params.path
                ),
                Some(error_meta(
                    "validation",
                    false,
                    "pass a file path, not a directory",
                )),
            )));
        }

        // summary=true and cursor are mutually exclusive
        if summary_cursor_conflict(
            params.output_control.summary,
            params.pagination.cursor.as_deref(),
        ) {
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "summary=true is incompatible with a pagination cursor; use one or the other"
                    .to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "remove cursor or set summary=false",
                )),
            )));
        }

        // Call handler for analysis and caching
        let (arc_output, file_cache_hit) = match self.handle_file_details_mode(&params).await {
            Ok(v) => v,
            Err(e) => return Ok(err_to_tool_result(e)),
        };

        // Clone only the two fields that may be mutated per-request (formatted and
        // next_cursor). The heavy SemanticAnalysis data is shared via Arc and never
        // modified, so we borrow it directly from the cached pointer.
        let mut formatted = arc_output.formatted.clone();
        let line_count = arc_output.line_count;

        // Apply summary/output size limiting logic
        let use_summary = if params.output_control.force == Some(true) {
            false
        } else if params.output_control.summary == Some(true) {
            true
        } else if params.output_control.summary == Some(false) {
            false
        } else {
            formatted.len() > SIZE_LIMIT
        };

        if use_summary {
            formatted = format_file_details_summary(&arc_output.semantic, &params.path, line_count);
        } else if formatted.len() > SIZE_LIMIT && params.output_control.force != Some(true) {
            let estimated_tokens = formatted.len() / 4;
            let message = format!(
                "Output exceeds 50K chars ({} chars, ~{} tokens). Use one of:\n\
                 - force=true to return full output\n\
                 - Use fields to limit output to specific sections (functions, classes, or imports)\n\
                 - Use summary=true for a compact overview",
                formatted.len(),
                estimated_tokens
            );
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                message,
                Some(error_meta(
                    "validation",
                    false,
                    "use force=true, fields, or summary=true",
                )),
            )));
        }

        // Decode pagination cursor if provided (analyze_file)
        let page_size = params.pagination.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        let offset = if let Some(ref cursor_str) = params.pagination.cursor {
            let cursor_data = match decode_cursor(cursor_str).map_err(|e| {
                ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    e.to_string(),
                    Some(error_meta("validation", false, "invalid cursor format")),
                )
            }) {
                Ok(v) => v,
                Err(e) => return Ok(err_to_tool_result(e)),
            };
            cursor_data.offset
        } else {
            0
        };

        // Filter to top-level functions only (exclude methods) before pagination
        let top_level_fns: Vec<crate::types::FunctionInfo> = arc_output
            .semantic
            .functions
            .iter()
            .filter(|func| {
                !arc_output
                    .semantic
                    .classes
                    .iter()
                    .any(|class| func.line >= class.line && func.end_line <= class.end_line)
            })
            .cloned()
            .collect();

        // Paginate top-level functions only
        let paginated =
            match paginate_slice(&top_level_fns, offset, page_size, PaginationMode::Default) {
                Ok(v) => v,
                Err(e) => {
                    return Ok(err_to_tool_result(ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        e.to_string(),
                        Some(error_meta("transient", true, "retry the request")),
                    )));
                }
            };

        // Regenerate formatted output using the paginated formatter (handles verbose and pagination correctly)
        let verbose = params.output_control.verbose.unwrap_or(false);
        if !use_summary {
            // fields: serde rejects unknown enum variants at deserialization; no runtime validation required
            formatted = format_file_details_paginated(
                &paginated.items,
                paginated.total,
                &arc_output.semantic,
                &params.path,
                line_count,
                offset,
                verbose,
                params.fields.as_deref(),
            );
        }

        // Capture next_cursor from pagination result (unless using summary mode)
        let next_cursor = if use_summary {
            None
        } else {
            paginated.next_cursor.clone()
        };

        // Build final text output with pagination cursor if present (unless using summary mode)
        let mut final_text = formatted.clone();
        if !use_summary && let Some(ref cursor) = next_cursor {
            final_text.push('\n');
            final_text.push_str("NEXT_CURSOR: ");
            final_text.push_str(cursor);
        }

        // Build the response output, sharing SemanticAnalysis from the Arc to avoid cloning it.
        let response_output = analyze::FileAnalysisOutput::new(
            formatted,
            arc_output.semantic.clone(),
            line_count,
            next_cursor,
        );

        let mut result = CallToolResult::success(vec![Content::text(final_text.clone())])
            .with_meta(Some(no_cache_meta()));
        let structured = serde_json::to_value(&response_output).unwrap_or(Value::Null);
        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(crate::metrics::MetricEvent {
            ts: crate::metrics::unix_ms(),
            tool: "analyze_file",
            duration_ms: dur,
            output_chars: final_text.len(),
            param_path_depth: crate::metrics::path_component_count(&param_path),
            max_depth: None,
            result: "ok",
            error_type: None,
            session_id: sid,
            seq: Some(seq),
            cache_hit: Some(file_cache_hit),
        });
        Ok(result)
    }

    #[instrument(skip(self, context))]
    #[tool(
        name = "analyze_symbol",
        description = "Call graph for a named symbol across all files in a directory. Returns callers and callees. Modes: call graph (default), import_lookup (files importing a module path), def_use (write/read sites). Fails if file path supplied; fails if impl_only=true on non-Rust directory; fails if import_lookup=true with empty symbol; fails if summary=true and cursor. match_mode controls name matching (exact/insensitive/prefix/contains). git_ref restricts to changed files. Example queries: Find all callers of parse_config; Find all files that import std::collections.",
        output_schema = schema_for_type::<analyze::FocusedAnalysisOutput>(),
        annotations(
            title = "Analyze Symbol",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn analyze_symbol(
        &self,
        params: Parameters<AnalyzeSymbolParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        let ct = context.ct.clone();
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let max_depth_val = params.follow_depth;
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        // Check if path is a file (not allowed for analyze_symbol)
        if std::path::Path::new(&params.path).is_file() {
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                format!(
                    "'{}' is a file; analyze_symbol requires a directory path",
                    params.path
                ),
                Some(error_meta(
                    "validation",
                    false,
                    "pass a directory path, not a file",
                )),
            )));
        }

        // summary=true and cursor are mutually exclusive
        if summary_cursor_conflict(
            params.output_control.summary,
            params.pagination.cursor.as_deref(),
        ) {
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "summary=true is incompatible with a pagination cursor; use one or the other"
                    .to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "remove cursor or set summary=false",
                )),
            )));
        }

        // import_lookup=true is mutually exclusive with a non-empty symbol.
        if let Err(e) = Self::validate_import_lookup(params.import_lookup, &params.symbol) {
            return Ok(err_to_tool_result(e));
        }

        // import_lookup mode: scan for files importing `params.symbol` as a module path.
        if params.import_lookup == Some(true) {
            let path_owned = PathBuf::from(&params.path);
            let symbol = params.symbol.clone();
            let git_ref = params.git_ref.clone();
            let max_depth = params.max_depth;
            let ast_recursion_limit = params.ast_recursion_limit;

            let handle = tokio::task::spawn_blocking(move || {
                let path = path_owned.as_path();
                let raw_entries = match walk_directory(path, max_depth) {
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
                let entries = if let Some(ref git_ref_val) = git_ref
                    && !git_ref_val.is_empty()
                {
                    let changed = match changed_files_from_git_ref(path, git_ref_val) {
                        Ok(c) => c,
                        Err(e) => {
                            return Err(ErrorData::new(
                                rmcp::model::ErrorCode::INVALID_PARAMS,
                                format!("git_ref filter failed: {e}"),
                                Some(error_meta(
                                    "resource",
                                    false,
                                    "ensure git is installed and path is inside a git repository",
                                )),
                            ));
                        }
                    };
                    filter_entries_by_git_ref(raw_entries, &changed, path)
                } else {
                    raw_entries
                };
                let output = match analyze::analyze_import_lookup(
                    path,
                    &symbol,
                    &entries,
                    ast_recursion_limit,
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        return Err(ErrorData::new(
                            rmcp::model::ErrorCode::INTERNAL_ERROR,
                            format!("import_lookup failed: {e}"),
                            Some(error_meta(
                                "resource",
                                false,
                                "check path and file permissions",
                            )),
                        ));
                    }
                };
                Ok(output)
            });

            let output = match handle.await {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => return Ok(err_to_tool_result(e)),
                Err(e) => {
                    return Ok(err_to_tool_result(ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        format!("spawn_blocking failed: {e}"),
                        Some(error_meta("resource", false, "internal error")),
                    )));
                }
            };

            let final_text = output.formatted.clone();
            let mut result = CallToolResult::success(vec![Content::text(final_text.clone())])
                .with_meta(Some(no_cache_meta()));
            let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
            result.structured_content = Some(structured);
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(crate::metrics::MetricEvent {
                ts: crate::metrics::unix_ms(),
                tool: "analyze_symbol",
                duration_ms: dur,
                output_chars: final_text.len(),
                param_path_depth: crate::metrics::path_component_count(&param_path),
                max_depth: max_depth_val,
                result: "ok",
                error_type: None,
                session_id: sid,
                seq: Some(seq),
                cache_hit: Some(false),
            });
            return Ok(result);
        }

        // Call handler for analysis and progress tracking
        let mut output = match self.handle_focused_mode(&params, ct).await {
            Ok(v) => v,
            Err(e) => return Ok(err_to_tool_result(e)),
        };

        // Decode pagination cursor if provided (analyze_symbol)
        let page_size = params.pagination.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        let offset = if let Some(ref cursor_str) = params.pagination.cursor {
            let cursor_data = match decode_cursor(cursor_str).map_err(|e| {
                ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    e.to_string(),
                    Some(error_meta("validation", false, "invalid cursor format")),
                )
            }) {
                Ok(v) => v,
                Err(e) => return Ok(err_to_tool_result(e)),
            };
            cursor_data.offset
        } else {
            0
        };

        // SymbolFocus pagination: decode cursor mode to determine callers vs callees
        let cursor_mode = if let Some(ref cursor_str) = params.pagination.cursor {
            decode_cursor(cursor_str)
                .map(|c| c.mode)
                .unwrap_or(PaginationMode::Callers)
        } else {
            PaginationMode::Callers
        };

        let mut use_summary = params.output_control.summary == Some(true);
        if params.output_control.force == Some(true) {
            use_summary = false;
        }
        let verbose = params.output_control.verbose.unwrap_or(false);

        let mut callee_cursor = match cursor_mode {
            PaginationMode::Callers => {
                let (paginated_items, paginated_next) = match paginate_focus_chains(
                    &output.prod_chains,
                    PaginationMode::Callers,
                    offset,
                    page_size,
                ) {
                    Ok(v) => v,
                    Err(e) => return Ok(err_to_tool_result(e)),
                };

                if !use_summary
                    && (paginated_next.is_some()
                        || offset > 0
                        || !verbose
                        || !output.outgoing_chains.is_empty())
                {
                    let base_path = Path::new(&params.path);
                    output.formatted = format_focused_paginated(
                        &paginated_items,
                        output.prod_chains.len(),
                        PaginationMode::Callers,
                        &params.symbol,
                        &output.prod_chains,
                        &output.test_chains,
                        &output.outgoing_chains,
                        output.def_count,
                        offset,
                        Some(base_path),
                        verbose,
                    );
                    paginated_next
                } else {
                    None
                }
            }
            PaginationMode::Callees => {
                let (paginated_items, paginated_next) = match paginate_focus_chains(
                    &output.outgoing_chains,
                    PaginationMode::Callees,
                    offset,
                    page_size,
                ) {
                    Ok(v) => v,
                    Err(e) => return Ok(err_to_tool_result(e)),
                };

                if paginated_next.is_some() || offset > 0 || !verbose {
                    let base_path = Path::new(&params.path);
                    output.formatted = format_focused_paginated(
                        &paginated_items,
                        output.outgoing_chains.len(),
                        PaginationMode::Callees,
                        &params.symbol,
                        &output.prod_chains,
                        &output.test_chains,
                        &output.outgoing_chains,
                        output.def_count,
                        offset,
                        Some(base_path),
                        verbose,
                    );
                    paginated_next
                } else {
                    None
                }
            }
            PaginationMode::Default => {
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "invalid cursor: unknown pagination mode".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "use a cursor returned by a previous analyze_symbol call",
                    )),
                )));
            }
            PaginationMode::DefUse => {
                let total_sites = output.def_use_sites.len();
                let (paginated_sites, paginated_next) = match paginate_slice(
                    &output.def_use_sites,
                    offset,
                    page_size,
                    PaginationMode::DefUse,
                ) {
                    Ok(r) => (r.items, r.next_cursor),
                    Err(e) => return Ok(err_to_tool_result_from_pagination(e)),
                };

                // Always regenerate formatted output for DefUse mode so the
                // first page (offset=0, verbose=true) is not skipped.
                if !use_summary {
                    let base_path = Path::new(&params.path);
                    output.formatted = format_focused_paginated_defuse(
                        &paginated_sites,
                        total_sites,
                        &params.symbol,
                        offset,
                        Some(base_path),
                        verbose,
                    );
                }

                // Slice output.def_use_sites to the current page window so
                // structuredContent only contains the paginated subset.
                output.def_use_sites = paginated_sites;

                paginated_next
            }
        };

        // When callers are exhausted and callees exist, bootstrap callee pagination
        // by emitting a {mode:callees, offset:0} cursor. This makes PaginationMode::Callees
        // reachable; without it the branch was dead code. Suppressed in summary mode
        // because summary and pagination are mutually exclusive.
        if callee_cursor.is_none()
            && cursor_mode == PaginationMode::Callers
            && !output.outgoing_chains.is_empty()
            && !use_summary
            && let Ok(cursor) = encode_cursor(&CursorData {
                mode: PaginationMode::Callees,
                offset: 0,
            })
        {
            callee_cursor = Some(cursor);
        }

        // When callees are exhausted and def_use_sites exist, bootstrap defuse cursor
        // by emitting a {mode:defuse, offset:0} cursor. This makes PaginationMode::DefUse
        // reachable. Suppressed in summary mode because summary and pagination are mutually exclusive.
        // Also bootstrap directly from Callers mode when there are no outgoing chains
        // (e.g. SymbolNotFound path or symbols with no callees) so def-use pagination
        // is reachable even without a Callees phase.
        if callee_cursor.is_none()
            && matches!(
                cursor_mode,
                PaginationMode::Callees | PaginationMode::Callers
            )
            && !output.def_use_sites.is_empty()
            && !use_summary
            && let Ok(cursor) = encode_cursor(&CursorData {
                mode: PaginationMode::DefUse,
                offset: 0,
            })
        {
            // Only bootstrap from Callers when callees are empty (otherwise
            // the Callees bootstrap above takes priority).
            if cursor_mode == PaginationMode::Callees || output.outgoing_chains.is_empty() {
                callee_cursor = Some(cursor);
            }
        }

        // Update next_cursor in output
        output.next_cursor.clone_from(&callee_cursor);

        // Build final text output with pagination cursor if present
        let mut final_text = output.formatted.clone();
        if let Some(cursor) = callee_cursor {
            final_text.push('\n');
            final_text.push_str("NEXT_CURSOR: ");
            final_text.push_str(&cursor);
        }

        let mut result = CallToolResult::success(vec![Content::text(final_text.clone())])
            .with_meta(Some(no_cache_meta()));
        // Only include def_use_sites in structuredContent when in DefUse mode.
        // In Callers/Callees modes, clearing the vec prevents large def-use
        // payloads from leaking into paginated non-def-use responses.
        if cursor_mode != PaginationMode::DefUse {
            output.def_use_sites = Vec::new();
        }
        let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(crate::metrics::MetricEvent {
            ts: crate::metrics::unix_ms(),
            tool: "analyze_symbol",
            duration_ms: dur,
            output_chars: final_text.len(),
            param_path_depth: crate::metrics::path_component_count(&param_path),
            max_depth: max_depth_val,
            result: "ok",
            error_type: None,
            session_id: sid,
            seq: Some(seq),
            cache_hit: Some(false),
        });
        Ok(result)
    }

    #[instrument(skip(self, _context))]
    #[tool(
        name = "analyze_module",
        description = "Function and import index for a single source file with minimal token cost: name, line_count, language, function names with line numbers, import list only (~75% smaller than analyze_file). Fails if directory path supplied. Pagination, summary, force, verbose, git_ref not supported. Use analyze_file when you need signatures, types, or class details. Supported: Rust, Go, Java, Python, TypeScript, TSX, Fortran, JavaScript, C/C++, C#. Example queries: What functions are defined in src/analyze.rs?",
        output_schema = schema_for_type::<types::ModuleInfo>(),
        annotations(
            title = "Analyze Module",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn analyze_module(
        &self,
        params: Parameters<AnalyzeModuleParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        // Issue 340: Guard against directory paths
        if std::fs::metadata(&params.path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(crate::metrics::MetricEvent {
                ts: crate::metrics::unix_ms(),
                tool: "analyze_module",
                duration_ms: dur,
                output_chars: 0,
                param_path_depth: crate::metrics::path_component_count(&param_path),
                max_depth: None,
                result: "error",
                error_type: Some("invalid_params".to_string()),
                session_id: sid.clone(),
                seq: Some(seq),
                cache_hit: None,
            });
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                format!(
                    "'{}' is a directory. Use analyze_directory to analyze a directory, or pass a specific file path to analyze_module.",
                    params.path
                ),
                Some(error_meta(
                    "validation",
                    false,
                    "use analyze_directory for directories",
                )),
            )));
        }

        // Check file cache using mtime-keyed CacheKey (same pattern as handle_file_details_mode).
        let module_cache_key = std::fs::metadata(&params.path).ok().and_then(|meta| {
            meta.modified().ok().map(|mtime| cache::CacheKey {
                path: std::path::PathBuf::from(&params.path),
                modified: mtime,
                mode: AnalysisMode::FileDetails,
            })
        });
        let (module_info, module_cache_hit) = if let Some(ref key) = module_cache_key
            && let Some(cached_file) = self.cache.get(key)
        {
            // Reconstruct ModuleInfo from the cached FileAnalysisOutput.
            // Path and language are derived from params.path since FileAnalysisOutput
            // does not store them.
            let file_path = std::path::Path::new(&params.path);
            let name = file_path
                .file_name()
                .and_then(|n: &std::ffi::OsStr| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            let language = file_path
                .extension()
                .and_then(|e| e.to_str())
                .and_then(aptu_coder_core::lang::language_for_extension)
                .unwrap_or("unknown")
                .to_string();
            let mut mi = types::ModuleInfo::default();
            mi.name = name;
            mi.line_count = cached_file.line_count;
            mi.language = language;
            mi.functions = cached_file
                .semantic
                .functions
                .iter()
                .map(|f| {
                    let mut mfi = types::ModuleFunctionInfo::default();
                    mfi.name = f.name.clone();
                    mfi.line = f.line;
                    mfi
                })
                .collect();
            mi.imports = cached_file
                .semantic
                .imports
                .iter()
                .map(|i| {
                    let mut mii = types::ModuleImportInfo::default();
                    mii.module = i.module.clone();
                    mii.items = i.items.clone();
                    mii
                })
                .collect();
            (mi, true)
        } else {
            // Cache miss: call analyze_file (returns FileAnalysisOutput) so we can populate
            // the file cache for future calls. Then reconstruct ModuleInfo from the result,
            // mirroring the cache-hit path above.
            let file_output = match analyze::analyze_file(&params.path, None) {
                Ok(v) => v,
                Err(e) => {
                    let error_data = match &e {
                        analyze::AnalyzeError::Io(io_err) => match io_err.kind() {
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
                                ErrorData::new(
                                    rmcp::model::ErrorCode::INVALID_PARAMS,
                                    format!("Failed to analyze module: {e}"),
                                    Some(error_meta(
                                        "validation",
                                        false,
                                        "ensure file exists, is readable, and has a supported extension",
                                    )),
                                )
                            }
                            _ => ErrorData::new(
                                rmcp::model::ErrorCode::INTERNAL_ERROR,
                                format!("Failed to analyze module: {e}"),
                                Some(error_meta("internal", false, "report this as a bug")),
                            ),
                        },
                        analyze::AnalyzeError::UnsupportedLanguage(_)
                        | analyze::AnalyzeError::InvalidRange { .. }
                        | analyze::AnalyzeError::NotAFile(_) => ErrorData::new(
                            rmcp::model::ErrorCode::INVALID_PARAMS,
                            format!("Failed to analyze module: {e}"),
                            Some(error_meta(
                                "validation",
                                false,
                                "ensure the path is a supported source file",
                            )),
                        ),
                        _ => ErrorData::new(
                            rmcp::model::ErrorCode::INTERNAL_ERROR,
                            format!("Failed to analyze module: {e}"),
                            Some(error_meta("internal", false, "report this as a bug")),
                        ),
                    };
                    return Ok(err_to_tool_result(error_data));
                }
            };
            let arc_output = std::sync::Arc::new(file_output);
            if let Some(key) = module_cache_key.clone() {
                self.cache.put(key, arc_output.clone());
            }
            let file_path = std::path::Path::new(&params.path);
            let name = file_path
                .file_name()
                .and_then(|n: &std::ffi::OsStr| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            let language = file_path
                .extension()
                .and_then(|e| e.to_str())
                .and_then(aptu_coder_core::lang::language_for_extension)
                .unwrap_or("unknown")
                .to_string();
            let mut mi = types::ModuleInfo::default();
            mi.name = name;
            mi.line_count = arc_output.line_count;
            mi.language = language;
            mi.functions = arc_output
                .semantic
                .functions
                .iter()
                .map(|f| {
                    let mut mfi = types::ModuleFunctionInfo::default();
                    mfi.name = f.name.clone();
                    mfi.line = f.line;
                    mfi
                })
                .collect();
            mi.imports = arc_output
                .semantic
                .imports
                .iter()
                .map(|i| {
                    let mut mii = types::ModuleImportInfo::default();
                    mii.module = i.module.clone();
                    mii.items = i.items.clone();
                    mii
                })
                .collect();
            (mi, false)
        };

        let text = format_module_info(&module_info);
        let mut result = CallToolResult::success(vec![Content::text(text.clone())])
            .with_meta(Some(no_cache_meta()));
        let structured = match serde_json::to_value(&module_info).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("serialization failed: {e}"),
                Some(error_meta("internal", false, "report this as a bug")),
            )
        }) {
            Ok(v) => v,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(crate::metrics::MetricEvent {
            ts: crate::metrics::unix_ms(),
            tool: "analyze_module",
            duration_ms: dur,
            output_chars: text.len(),
            param_path_depth: crate::metrics::path_component_count(&param_path),
            max_depth: None,
            result: "ok",
            error_type: None,
            session_id: sid,
            seq: Some(seq),
            cache_hit: Some(module_cache_hit),
        });
        Ok(result)
    }

    #[instrument(skip(self, _context))]
    #[tool(
        name = "analyze_raw",
        description = "Raw UTF-8 file content with line numbers; no AST parsing. Returns path, total_lines, start_line, end_line, content. Use start_line/end_line (1-indexed, inclusive) to read a range; defaults to full file. Fails if directory path supplied; fails on binary or non-UTF-8 files. Use analyze_file or analyze_module for AST-structured output. Example queries: Show lines 100-150 of src/lib.rs.",
        output_schema = schema_for_type::<types::AnalyzeRawOutput>(),
        annotations(
            title = "Analyze Raw",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn analyze_raw(
        &self,
        params: Parameters<types::AnalyzeRawParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        // Guard against directory paths
        if std::fs::metadata(&params.path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(crate::metrics::MetricEvent {
                ts: crate::metrics::unix_ms(),
                tool: "analyze_raw",
                duration_ms: dur,
                output_chars: 0,
                param_path_depth: crate::metrics::path_component_count(&param_path),
                max_depth: None,
                result: "error",
                error_type: Some("invalid_params".to_string()),
                session_id: sid.clone(),
                seq: Some(seq),
                cache_hit: None,
            });
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "path is a directory; use analyze_directory instead".to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "pass a file path, not a directory",
                )),
            )));
        }

        let path = std::path::PathBuf::from(&params.path);
        let handle = tokio::task::spawn_blocking(move || {
            aptu_coder_core::analyze_raw_range(&path, params.start_line, params.end_line)
        });

        let output = match handle.await {
            Ok(Ok(v)) => v,
            Ok(Err(aptu_coder_core::AnalyzeError::InvalidRange { start, end, total })) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "analyze_raw",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    format!("invalid range: start ({start}) > end ({end}); file has {total} lines"),
                    Some(error_meta(
                        "validation",
                        false,
                        "ensure start_line <= end_line",
                    )),
                )));
            }
            Ok(Err(aptu_coder_core::AnalyzeError::NotAFile(_))) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "analyze_raw",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "path is not a file".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "provide a file path, not a directory",
                    )),
                )));
            }
            Ok(Err(e)) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "analyze_raw",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )));
            }
            Err(e) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "analyze_raw",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )));
            }
        };

        let text = format!(
            "File: {} (total: {} lines, showing: {}-{})\n\n{}",
            output.path, output.total_lines, output.start_line, output.end_line, output.content
        );
        let mut result = CallToolResult::success(vec![Content::text(text.clone())])
            .with_meta(Some(no_cache_meta()));
        let structured = match serde_json::to_value(&output).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("serialization failed: {e}"),
                Some(error_meta("internal", false, "report this as a bug")),
            )
        }) {
            Ok(v) => v,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(crate::metrics::MetricEvent {
            ts: crate::metrics::unix_ms(),
            tool: "analyze_raw",
            duration_ms: dur,
            output_chars: text.len(),
            param_path_depth: crate::metrics::path_component_count(&param_path),
            max_depth: None,
            result: "ok",
            error_type: None,
            session_id: sid,
            seq: Some(seq),
            cache_hit: None,
        });
        Ok(result)
    }

    #[instrument(skip(self, _context))]
    #[tool(
        name = "edit_overwrite",
        description = "Creates or overwrites a file with UTF-8 content; creates parent directories if needed. Returns path, bytes_written. Fails if directory path supplied. AST-unaware (no language constraint). Use edit_replace for targeted single-block edits; edit_rename/edit_insert for AST-targeted changes. Example queries: Overwrite src/config.rs with updated content.",
        output_schema = schema_for_type::<EditOverwriteOutput>(),
        annotations(
            title = "Edit Overwrite",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn edit_overwrite(
        &self,
        params: Parameters<EditOverwriteParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let _validated_path = match validate_path(&params.path, false) {
            Ok(p) => p,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        // Guard against directory paths
        if std::fs::metadata(&params.path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(crate::metrics::MetricEvent {
                ts: crate::metrics::unix_ms(),
                tool: "edit_overwrite",
                duration_ms: dur,
                output_chars: 0,
                param_path_depth: crate::metrics::path_component_count(&param_path),
                max_depth: None,
                result: "error",
                error_type: Some("invalid_params".to_string()),
                session_id: sid.clone(),
                seq: Some(seq),
                cache_hit: None,
            });
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "path is a directory; cannot write to a directory".to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "provide a file path, not a directory",
                )),
            )));
        }

        let path = std::path::PathBuf::from(&params.path);
        let content = params.content.clone();
        let handle = tokio::task::spawn_blocking(move || {
            aptu_coder_core::edit_overwrite_content(&path, &content)
        });

        let output = match handle.await {
            Ok(Ok(v)) => v,
            Ok(Err(aptu_coder_core::EditError::NotAFile(_))) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_overwrite",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "path is a directory".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "provide a file path, not a directory",
                    )),
                )));
            }
            Ok(Err(e)) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_overwrite",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )));
            }
            Err(e) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_overwrite",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )));
            }
        };

        let text = format!("Wrote {} bytes to {}", output.bytes_written, output.path);
        let mut result = CallToolResult::success(vec![Content::text(text.clone())])
            .with_meta(Some(no_cache_meta()));
        let structured = match serde_json::to_value(&output).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("serialization failed: {e}"),
                Some(error_meta("internal", false, "report this as a bug")),
            )
        }) {
            Ok(v) => v,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        result.structured_content = Some(structured);
        self.cache
            .invalidate_file(&std::path::PathBuf::from(&param_path));
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(crate::metrics::MetricEvent {
            ts: crate::metrics::unix_ms(),
            tool: "edit_overwrite",
            duration_ms: dur,
            output_chars: text.len(),
            param_path_depth: crate::metrics::path_component_count(&param_path),
            max_depth: None,
            result: "ok",
            error_type: None,
            session_id: sid,
            seq: Some(seq),
            cache_hit: None,
        });
        Ok(result)
    }

    #[instrument(skip(self, _context))]
    #[tool(
        name = "edit_replace",
        description = "Replaces a unique exact text block; old_text must match character-for-character and appear exactly once. Returns path, bytes_before, bytes_after. Fails if zero matches; fails if multiple matches (extend old_text to be more specific). Whitespace-sensitive exact match. Use edit_overwrite to replace the whole file; edit_rename/edit_insert for AST-targeted changes. Example queries: Update the function signature in lib.rs.",
        output_schema = schema_for_type::<EditReplaceOutput>(),
        annotations(
            title = "Edit Replace",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn edit_replace(
        &self,
        params: Parameters<EditReplaceParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        // Guard against directory paths
        if std::fs::metadata(&params.path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(crate::metrics::MetricEvent {
                ts: crate::metrics::unix_ms(),
                tool: "edit_replace",
                duration_ms: dur,
                output_chars: 0,
                param_path_depth: crate::metrics::path_component_count(&param_path),
                max_depth: None,
                result: "error",
                error_type: Some("invalid_params".to_string()),
                session_id: sid.clone(),
                seq: Some(seq),
                cache_hit: None,
            });
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "path is a directory; cannot edit a directory".to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "provide a file path, not a directory",
                )),
            )));
        }

        let path = std::path::PathBuf::from(&params.path);
        let old_text = params.old_text.clone();
        let new_text = params.new_text.clone();
        let handle = tokio::task::spawn_blocking(move || {
            aptu_coder_core::edit_replace_block(&path, &old_text, &new_text)
        });

        let output = match handle.await {
            Ok(Ok(v)) => v,
            Ok(Err(aptu_coder_core::EditError::NotFound { path: _ })) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_replace",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "old_text not found in file — verify the text matches exactly, including whitespace and newlines".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "check that old_text appears in the file",
                    )),
                )));
            }
            Ok(Err(aptu_coder_core::EditError::Ambiguous { count, path: _ })) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_replace",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    format!(
                        "old_text appears {count} times in file — make old_text longer and more specific to uniquely identify the block"
                    ),
                    Some(error_meta(
                        "validation",
                        false,
                        "include more context in old_text to make it unique",
                    )),
                )));
            }
            Ok(Err(aptu_coder_core::EditError::NotAFile(_))) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_replace",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "path is a directory".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "provide a file path, not a directory",
                    )),
                )));
            }
            Ok(Err(e)) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_replace",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )));
            }
            Err(e) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_replace",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )));
            }
        };

        let text = format!(
            "Edited {}: {} bytes -> {} bytes",
            output.path, output.bytes_before, output.bytes_after
        );
        let mut result = CallToolResult::success(vec![Content::text(text.clone())])
            .with_meta(Some(no_cache_meta()));
        let structured = match serde_json::to_value(&output).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("serialization failed: {e}"),
                Some(error_meta("internal", false, "report this as a bug")),
            )
        }) {
            Ok(v) => v,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        result.structured_content = Some(structured);
        self.cache
            .invalidate_file(&std::path::PathBuf::from(&param_path));
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(crate::metrics::MetricEvent {
            ts: crate::metrics::unix_ms(),
            tool: "edit_replace",
            duration_ms: dur,
            output_chars: text.len(),
            param_path_depth: crate::metrics::path_component_count(&param_path),
            max_depth: None,
            result: "ok",
            error_type: None,
            session_id: sid,
            seq: Some(seq),
            cache_hit: None,
        });
        Ok(result)
    }

    #[instrument(skip(self, _context))]
    #[tool(
        name = "edit_rename",
        description = "AST-aware rename within a single file. Matches syntactic identifiers only; occurrences in string literals and comments are excluded. Returns path, old_name, new_name, occurrences_renamed. Fails if old_name not found; fails if kind parameter supplied (reserved, not yet supported). Supported: Rust, Go, Java, Python, TypeScript, TSX, Fortran, JavaScript, C/C++, C#. Example queries: Rename function parse_config to load_config in src/config.rs.",
        output_schema = schema_for_type::<EditRenameOutput>(),
        annotations(
            title = "Edit Rename",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn edit_rename(
        &self,
        params: Parameters<EditRenameParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        // Guard against directory paths
        if std::fs::metadata(&params.path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(crate::metrics::MetricEvent {
                ts: crate::metrics::unix_ms(),
                tool: "edit_rename",
                duration_ms: dur,
                output_chars: 0,
                param_path_depth: crate::metrics::path_component_count(&param_path),
                max_depth: None,
                result: "error",
                error_type: Some("invalid_params".to_string()),
                session_id: sid.clone(),
                seq: Some(seq),
                cache_hit: None,
            });
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "edit_rename operates on a single file — provide a file path, not a directory"
                    .to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "provide a file path, not a directory",
                )),
            )));
        }

        let path = std::path::PathBuf::from(&params.path);
        let old_name = params.old_name.clone();
        let new_name = params.new_name.clone();
        let kind = params.kind.clone();
        let handle = tokio::task::spawn_blocking(move || {
            edit_rename_in_file(&path, &old_name, &new_name, kind.as_deref())
        });

        let output = match handle.await {
            Ok(Ok(v)) => v,
            Ok(Err(aptu_coder_core::EditError::SymbolNotFound { .. })) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_rename",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "symbol not found in file".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "verify the symbol name and file path",
                    )),
                )));
            }
            Ok(Err(aptu_coder_core::EditError::AmbiguousKind { .. })) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_rename",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "symbol name is ambiguous".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "verify the symbol name is unique",
                    )),
                )));
            }
            Ok(Err(aptu_coder_core::EditError::UnsupportedLanguage(_))) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_rename",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "file language is not supported".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "check that the file has a supported language extension",
                    )),
                )));
            }
            Ok(Err(aptu_coder_core::EditError::KindFilterUnsupported)) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_rename",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "kind filtering is not supported with the current identifier query infrastructure"
                        .to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "omit the kind parameter",
                    )),
                )));
            }
            Ok(Err(aptu_coder_core::EditError::NotAFile(_))) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_rename",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "path is a directory".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "provide a file path, not a directory",
                    )),
                )));
            }
            Ok(Err(e)) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_rename",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )));
            }
            Err(e) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_rename",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )));
            }
        };

        let text = format!(
            "Renamed '{}' to '{}' in {} ({} occurrence(s))",
            output.old_name, output.new_name, output.path, output.occurrences_renamed
        );
        let mut result = CallToolResult::success(vec![Content::text(text.clone())])
            .with_meta(Some(no_cache_meta()));
        let structured = match serde_json::to_value(&output).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("serialization failed: {e}"),
                Some(error_meta("internal", false, "report this as a bug")),
            )
        }) {
            Ok(v) => v,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        result.structured_content = Some(structured);
        self.cache
            .invalidate_file(&std::path::PathBuf::from(&param_path));
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(crate::metrics::MetricEvent {
            ts: crate::metrics::unix_ms(),
            tool: "edit_rename",
            duration_ms: dur,
            output_chars: text.len(),
            param_path_depth: crate::metrics::path_component_count(&param_path),
            max_depth: None,
            result: "ok",
            error_type: None,
            session_id: sid,
            seq: Some(seq),
            cache_hit: None,
        });
        Ok(result)
    }

    #[instrument(skip(self, _context))]
    #[tool(
        name = "edit_insert",
        description = "Insert content immediately before or after a named identifier in a source file. Returns path, symbol_name, position, byte_offset. position is \"before\" or \"after\"; symbol_name must be an identifier (not a keyword or punctuation). Inserts content verbatim at that token's byte boundary; include leading/trailing newlines as needed. Uses first occurrence if symbol_name appears multiple times. Fails if symbol not found; fails if directory path supplied. Supported: Rust, Go, Java, Python, TypeScript, TSX, Fortran, JavaScript, C/C++, C#. Example queries: Insert a #[instrument] attribute before the handle_request function.",
        output_schema = schema_for_type::<EditInsertOutput>(),
        annotations(
            title = "Edit Insert",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn edit_insert(
        &self,
        params: Parameters<EditInsertParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        // Guard against directory paths
        if std::fs::metadata(&params.path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(crate::metrics::MetricEvent {
                ts: crate::metrics::unix_ms(),
                tool: "edit_insert",
                duration_ms: dur,
                output_chars: 0,
                param_path_depth: crate::metrics::path_component_count(&param_path),
                max_depth: None,
                result: "error",
                error_type: Some("invalid_params".to_string()),
                session_id: sid.clone(),
                seq: Some(seq),
                cache_hit: None,
            });
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "edit_insert operates on a single file — provide a file path, not a directory"
                    .to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "provide a file path, not a directory",
                )),
            )));
        }

        let path = std::path::PathBuf::from(&params.path);
        let symbol_name = params.symbol_name.clone();
        let position = params.position;
        let content = params.content.clone();
        let handle = tokio::task::spawn_blocking(move || {
            edit_insert_at_symbol(&path, &symbol_name, position, &content)
        });

        let output = match handle.await {
            Ok(Ok(v)) => v,
            Ok(Err(aptu_coder_core::EditError::SymbolNotFound { .. })) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_insert",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "symbol not found in file".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "verify the symbol name and file path",
                    )),
                )));
            }
            Ok(Err(aptu_coder_core::EditError::UnsupportedLanguage(_))) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_insert",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("invalid_params".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "file language is not supported".to_string(),
                    Some(error_meta(
                        "validation",
                        false,
                        "check that the file has a supported language extension",
                    )),
                )));
            }
            Ok(Err(e)) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_insert",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )));
            }
            Err(e) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "edit_insert",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(&param_path),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta(
                        "resource",
                        false,
                        "check file path and permissions",
                    )),
                )));
            }
        };

        let text = format!(
            "Inserted content {} '{}' in {} (at byte offset {})",
            output.position, output.symbol_name, output.path, output.byte_offset
        );
        let mut result = CallToolResult::success(vec![Content::text(text.clone())])
            .with_meta(Some(no_cache_meta()));
        let structured = match serde_json::to_value(&output).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("serialization failed: {e}"),
                Some(error_meta("internal", false, "report this as a bug")),
            )
        }) {
            Ok(v) => v,
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        result.structured_content = Some(structured);
        self.cache
            .invalidate_file(&std::path::PathBuf::from(&param_path));
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(crate::metrics::MetricEvent {
            ts: crate::metrics::unix_ms(),
            tool: "edit_insert",
            duration_ms: dur,
            output_chars: text.len(),
            param_path_depth: crate::metrics::path_component_count(&param_path),
            max_depth: None,
            result: "ok",
            error_type: None,
            session_id: sid,
            seq: Some(seq),
            cache_hit: None,
        });
        Ok(result)
    }

    #[tool(
        name = "exec_command",
        description = "Execute shell command via sh -c (or $SHELL if set). Returns stdout, stderr, exit_code, timed_out, output_truncated. Output capped at 2000 lines and 50 KB per stream; use timeout_secs to limit execution time. working_dir sets initial working directory; cd and absolute paths in command string bypass this restriction. Fails if working_dir does not exist, is not a directory, or is outside CWD. Example queries: Run the test suite and capture output.",
        output_schema = schema_for_type::<types::ShellOutput>(),
        annotations(
            title = "Exec Command",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn exec_command(
        &self,
        params: Parameters<types::ExecCommandParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let t_start = std::time::Instant::now();
        let params = params.0;

        // Validate working_dir if provided
        let working_dir_path = if let Some(ref wd) = params.working_dir {
            match validate_path(wd, true) {
                Ok(p) => {
                    // Verify it's a directory
                    if !std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false) {
                        return Ok(err_to_tool_result(ErrorData::new(
                            rmcp::model::ErrorCode::INVALID_PARAMS,
                            "working_dir must be a directory".to_string(),
                            Some(error_meta(
                                "validation",
                                false,
                                "provide a valid directory path",
                            )),
                        )));
                    }
                    Some(p)
                }
                Err(e) => {
                    return Ok(err_to_tool_result(e));
                }
            }
        } else {
            None
        };

        let param_path = params.working_dir.clone();
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        let command = params.command.clone();
        let timeout_secs = params.timeout_secs.unwrap_or(30);

        // Spawn the command using tokio::process::Command for proper async handling
        let mut cmd = tokio::process::Command::new(
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
        );
        cmd.arg("-c").arg(&command);

        if let Some(ref wd) = working_dir_path {
            cmd.current_dir(wd);
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "exec_command",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(
                        param_path.as_deref().unwrap_or(""),
                    ),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    format!("failed to spawn command: {e}"),
                    Some(error_meta(
                        "resource",
                        false,
                        "check command syntax and permissions",
                    )),
                )));
            }
        };

        // Wait for the command with timeout using tokio::select! to race wait against sleep
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);
        const MAX_BYTES: usize = 50 * 1024;
        let (stdout_str, stderr_str, exit_code, timed_out) = tokio::select! {
            result = async {
                use tokio::io::AsyncReadExt;
                let mut stdout = child.stdout.take();
                let mut stderr = child.stderr.take();

                let stdout_bytes = if let Some(ref mut s) = stdout {
                    let mut stdout_reader = s.take(MAX_BYTES as u64);
                    let mut stdout_buf = Vec::new();
                    let _ = stdout_reader.read_to_end(&mut stdout_buf).await;
                    stdout_buf
                } else {
                    Vec::new()
                };

                let stderr_bytes = if let Some(ref mut s) = stderr {
                    let mut stderr_reader = s.take(MAX_BYTES as u64);
                    let mut stderr_buf = Vec::new();
                    let _ = stderr_reader.read_to_end(&mut stderr_buf).await;
                    stderr_buf
                } else {
                    Vec::new()
                };

                let status = child.wait().await.ok();
                (stdout_bytes, stderr_bytes, status)
            } => {
                let stdout_str = String::from_utf8_lossy(&result.0).to_string();
                let stderr_str = String::from_utf8_lossy(&result.1).to_string();
                let exit_code = result.2.and_then(|s| s.code());
                (stdout_str, stderr_str, exit_code, false)
            }
            _ = tokio::time::sleep(timeout_duration) => {
                // Timeout occurred: kill the process and return empty output
                let _ = child.kill().await;
                let _ = child.wait().await;
                ("".to_string(), "".to_string(), None, true)
            }
        };

        // Truncate output if needed
        const MAX_LINES: usize = 2000;

        let (stdout, stdout_truncated) = truncate_output(&stdout_str, MAX_LINES, MAX_BYTES);
        let (stderr, stderr_truncated) = truncate_output(&stderr_str, MAX_LINES, MAX_BYTES);
        let output_truncated = stdout_truncated || stderr_truncated;

        let output =
            types::ShellOutput::new(stdout, stderr, exit_code, timed_out, output_truncated);

        let text = format!(
            "Command: {}\nExit code: {}\nTimed out: {}\nOutput truncated: {}\n\nStdout:\n{}\n\nStderr:\n{}",
            params.command,
            exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "null".to_string()),
            timed_out,
            output_truncated,
            output.stdout,
            output.stderr
        );

        let mut result = CallToolResult::success(vec![Content::text(text.clone())])
            .with_meta(Some(no_cache_meta()));

        let structured = match serde_json::to_value(&output).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("serialization failed: {e}"),
                Some(error_meta("internal", false, "report this as a bug")),
            )
        }) {
            Ok(v) => v,
            Err(e) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(crate::metrics::MetricEvent {
                    ts: crate::metrics::unix_ms(),
                    tool: "exec_command",
                    duration_ms: dur,
                    output_chars: 0,
                    param_path_depth: crate::metrics::path_component_count(
                        param_path.as_deref().unwrap_or(""),
                    ),
                    max_depth: None,
                    result: "error",
                    error_type: Some("internal_error".to_string()),
                    session_id: sid.clone(),
                    seq: Some(seq),
                    cache_hit: None,
                });
                return Ok(err_to_tool_result(e));
            }
        };

        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(crate::metrics::MetricEvent {
            ts: crate::metrics::unix_ms(),
            tool: "exec_command",
            duration_ms: dur,
            output_chars: text.len(),
            param_path_depth: crate::metrics::path_component_count(
                param_path.as_deref().unwrap_or(""),
            ),
            max_depth: None,
            result: "ok",
            error_type: None,
            session_id: sid,
            seq: Some(seq),
            cache_hit: None,
        });
        Ok(result)
    }
}

/// Truncates output to a maximum number of lines and bytes.
/// Returns (truncated_output, was_truncated).
fn truncate_output(output: &str, max_lines: usize, max_bytes: usize) -> (String, bool) {
    let lines: Vec<&str> = output.lines().collect();

    let output_to_use = if lines.len() > max_lines {
        lines[..max_lines].join("\n")
    } else {
        output.to_string()
    };

    if output_to_use.len() > max_bytes {
        // Find the last valid UTF-8 char boundary at or before max_bytes
        let safe_end = (0..=max_bytes.min(output_to_use.len()))
            .rev()
            .find(|&i| output_to_use.is_char_boundary(i))
            .unwrap_or(0);
        (output_to_use[..safe_end].to_string(), true)
    } else {
        (output_to_use, lines.len() > max_lines)
    }
}

// Parameters for focused analysis task.
#[derive(Clone)]
struct FocusedAnalysisParams {
    path: std::path::PathBuf,
    symbol: String,
    match_mode: SymbolMatchMode,
    follow_depth: u32,
    max_depth: Option<u32>,
    ast_recursion_limit: Option<usize>,
    use_summary: bool,
    impl_only: Option<bool>,
    def_use: bool,
}

#[tool_handler]
impl ServerHandler for CodeAnalyzer {
    fn get_info(&self) -> InitializeResult {
        let excluded = crate::EXCLUDED_DIRS.join(", ");
        let instructions = format!(
            "Recommended workflow:\n\
            1. Start with analyze_directory(path=<repo_root>, max_depth=2, summary=true) to identify source package (largest by file count; exclude {excluded}).\n\
            2. Re-run analyze_directory(path=<source_package>, max_depth=2, summary=true) for module map. Include test directories (tests/, *_test.go, test_*.py, test_*.rs, *.spec.ts, *.spec.js).\n\
            3. For key files, prefer analyze_module for function/import index; use analyze_file for signatures and types.\n\
            4. Use analyze_symbol to trace call graphs.\n\
            Prefer summary=true on 1000+ files. Set max_depth=2; increase if packages too large. Paginate with cursor/page_size. For subagents: DISABLE_PROMPT_CACHING=1."
        );
        let capabilities = ServerCapabilities::builder()
            .enable_logging()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_completions()
            .build();
        let server_info = Implementation::new("aptu-coder", env!("CARGO_PKG_VERSION"))
            .with_title("Aptu Coder")
            .with_description("MCP server for code structure analysis using tree-sitter");
        InitializeResult::new(capabilities)
            .with_server_info(server_info)
            .with_instructions(&instructions)
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let mut peer_lock = self.peer.lock().await;
        *peer_lock = Some(context.peer.clone());
        drop(peer_lock);

        // Generate session_id in MILLIS-N format
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let counter = GLOBAL_SESSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let sid = format!("{millis}-{counter}");
        {
            let mut session_id_lock = self.session_id.lock().await;
            *session_id_lock = Some(sid);
        }
        self.session_call_seq
            .store(0, std::sync::atomic::Ordering::Relaxed);

        // Spawn consumer task to drain log events from channel with batching.
        let peer = self.peer.clone();
        let event_rx = self.event_rx.clone();

        tokio::spawn(async move {
            let rx = {
                let mut rx_lock = event_rx.lock().await;
                rx_lock.take()
            };

            if let Some(mut receiver) = rx {
                let mut buffer = Vec::with_capacity(64);
                loop {
                    // Drain up to 64 events from channel
                    receiver.recv_many(&mut buffer, 64).await;

                    if buffer.is_empty() {
                        // Channel closed, exit consumer task
                        break;
                    }

                    // Acquire peer lock once per batch
                    let peer_lock = peer.lock().await;
                    if let Some(peer) = peer_lock.as_ref() {
                        for log_event in buffer.drain(..) {
                            let notification = ServerNotification::LoggingMessageNotification(
                                Notification::new(LoggingMessageNotificationParam {
                                    level: log_event.level,
                                    logger: Some(log_event.logger),
                                    data: log_event.data,
                                }),
                            );
                            if let Err(e) = peer.send_notification(notification).await {
                                warn!("Failed to send logging notification: {}", e);
                            }
                        }
                    }
                }
            }
        });
    }

    #[instrument(skip(self, _context))]
    async fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) {
        tracing::info!(
            request_id = ?notification.request_id,
            reason = ?notification.reason,
            "Received cancellation notification"
        );
    }

    #[instrument(skip(self, _context))]
    async fn complete(
        &self,
        request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        // Dispatch on argument name: "path" or "symbol"
        let argument_name = &request.argument.name;
        let argument_value = &request.argument.value;

        let completions = match argument_name.as_str() {
            "path" => {
                // Path completions: use current directory as root
                let root = Path::new(".");
                completion::path_completions(root, argument_value)
            }
            "symbol" => {
                // Symbol completions: need the path argument from context
                let path_arg = request
                    .context
                    .as_ref()
                    .and_then(|ctx| ctx.get_argument("path"));

                match path_arg {
                    Some(path_str) => {
                        let path = Path::new(path_str);
                        completion::symbol_completions(&self.cache, path, argument_value)
                    }
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        };

        // Create CompletionInfo with has_more flag if >100 results
        let total_count = u32::try_from(completions.len()).unwrap_or(u32::MAX);
        let (values, has_more) = if completions.len() > 100 {
            (completions.into_iter().take(100).collect(), true)
        } else {
            (completions, false)
        };

        let completion_info =
            match CompletionInfo::with_pagination(values, Some(total_count), has_more) {
                Ok(info) => info,
                Err(_) => {
                    // Graceful degradation: return empty on error
                    CompletionInfo::with_all_values(Vec::new())
                        .unwrap_or_else(|_| CompletionInfo::new(Vec::new()).unwrap())
                }
            };

        Ok(CompleteResult::new(completion_info))
    }

    async fn set_level(
        &self,
        params: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let level_filter = match params.level {
            LoggingLevel::Debug => LevelFilter::DEBUG,
            LoggingLevel::Info | LoggingLevel::Notice => LevelFilter::INFO,
            LoggingLevel::Warning => LevelFilter::WARN,
            LoggingLevel::Error
            | LoggingLevel::Critical
            | LoggingLevel::Alert
            | LoggingLevel::Emergency => LevelFilter::ERROR,
        };

        let mut filter_lock = self
            .log_level_filter
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *filter_lock = level_filter;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_emit_progress_none_peer_is_noop() {
        let peer = Arc::new(TokioMutex::new(None));
        let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
        let analyzer = CodeAnalyzer::new(
            peer,
            log_level_filter,
            rx,
            crate::metrics::MetricsSender(metrics_tx),
        );
        let token = ProgressToken(NumberOrString::String("test".into()));
        // Should complete without panic
        analyzer
            .emit_progress(None, &token, 0.0, 10.0, "test".to_string())
            .await;
    }

    fn make_analyzer() -> CodeAnalyzer {
        let peer = Arc::new(TokioMutex::new(None));
        let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
        CodeAnalyzer::new(
            peer,
            log_level_filter,
            rx,
            crate::metrics::MetricsSender(metrics_tx),
        )
    }

    #[test]
    fn test_summary_cursor_conflict() {
        assert!(summary_cursor_conflict(Some(true), Some("cursor")));
        assert!(!summary_cursor_conflict(Some(true), None));
        assert!(!summary_cursor_conflict(None, Some("x")));
        assert!(!summary_cursor_conflict(None, None));
    }

    #[tokio::test]
    async fn test_validate_impl_only_non_rust_returns_invalid_params() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.py"), "def foo(): pass").unwrap();

        let analyzer = make_analyzer();
        // Call analyze_symbol with impl_only=true on a Python-only directory via the tool API.
        // We use handle_focused_mode which calls validate_impl_only internally.
        let entries: Vec<traversal::WalkEntry> =
            traversal::walk_directory(dir.path(), None).unwrap_or_default();
        let result = CodeAnalyzer::validate_impl_only(&entries);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        drop(analyzer); // ensure it compiles with analyzer in scope
    }

    #[tokio::test]
    async fn test_no_cache_meta_on_analyze_directory_result() {
        use aptu_coder_core::types::{
            AnalyzeDirectoryParams, OutputControlParams, PaginationParams,
        };
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let analyzer = make_analyzer();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": dir.path().to_str().unwrap(),
        }))
        .unwrap();
        let ct = tokio_util::sync::CancellationToken::new();
        let (arc_output, _cache_hit) = analyzer.handle_overview_mode(&params, ct).await.unwrap();
        // Verify the no_cache_meta shape by constructing it directly and checking the shape
        let meta = no_cache_meta();
        assert_eq!(
            meta.0.get("cache_hint").and_then(|v| v.as_str()),
            Some("no-cache"),
        );
        drop(arc_output);
    }

    #[test]
    fn test_complete_path_completions_returns_suggestions() {
        // Test the underlying completion function (same code path as complete()) directly
        // to avoid needing a constructed RequestContext<RoleServer>.
        // CARGO_MANIFEST_DIR is <workspace>/aptu-coder; parent is the workspace root,
        // which contains aptu-coder-core/ and aptu-coder/ matching the "aptu-" prefix.
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent().expect("manifest dir has parent");
        let suggestions = completion::path_completions(workspace_root, "aptu-");
        assert!(
            !suggestions.is_empty(),
            "expected completions for prefix 'aptu-' in workspace root"
        );
    }

    #[tokio::test]
    async fn test_handle_overview_mode_verbose_no_summary_block() {
        use aptu_coder_core::pagination::{PaginationMode, paginate_slice};
        use aptu_coder_core::types::{
            AnalyzeDirectoryParams, OutputControlParams, PaginationParams,
        };
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();

        let peer = Arc::new(TokioMutex::new(None));
        let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
        let analyzer = CodeAnalyzer::new(
            peer,
            log_level_filter,
            rx,
            crate::metrics::MetricsSender(metrics_tx),
        );

        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": tmp.path().to_str().unwrap(),
            "verbose": true,
        }))
        .unwrap();

        let ct = tokio_util::sync::CancellationToken::new();
        let (output, _cache_hit) = analyzer.handle_overview_mode(&params, ct).await.unwrap();

        // Replicate the handler's formatting path (the fix site)
        let use_summary = output.formatted.len() > SIZE_LIMIT; // summary=None, force=None, small output
        let paginated =
            paginate_slice(&output.files, 0, DEFAULT_PAGE_SIZE, PaginationMode::Default).unwrap();
        let verbose = true;
        let formatted = if !use_summary {
            format_structure_paginated(
                &paginated.items,
                paginated.total,
                params.max_depth,
                Some(std::path::Path::new(&params.path)),
                verbose,
            )
        } else {
            output.formatted.clone()
        };

        // After the fix: verbose=true must not emit the SUMMARY: block
        assert!(
            !formatted.contains("SUMMARY:"),
            "verbose=true must not emit SUMMARY: block; got: {}",
            &formatted[..formatted.len().min(300)]
        );
        assert!(
            formatted.contains("PAGINATED:"),
            "verbose=true must emit PAGINATED: header"
        );
        assert!(
            formatted.contains("FILES [LOC, FUNCTIONS, CLASSES]"),
            "verbose=true must emit FILES section header"
        );
    }

    // --- cache_hit integration tests ---

    #[tokio::test]
    async fn test_analyze_directory_cache_hit_metrics() {
        use aptu_coder_core::types::{
            AnalyzeDirectoryParams, OutputControlParams, PaginationParams,
        };
        use tempfile::TempDir;

        // Arrange: a temp dir with one file
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").unwrap();
        let analyzer = make_analyzer();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": dir.path().to_str().unwrap(),
        }))
        .unwrap();

        // Act: first call (cache miss)
        let ct1 = tokio_util::sync::CancellationToken::new();
        let (_out1, hit1) = analyzer.handle_overview_mode(&params, ct1).await.unwrap();

        // Act: second call (cache hit)
        let ct2 = tokio_util::sync::CancellationToken::new();
        let (_out2, hit2) = analyzer.handle_overview_mode(&params, ct2).await.unwrap();

        // Assert
        assert!(!hit1, "first call must be a cache miss");
        assert!(hit2, "second call must be a cache hit");
    }

    #[tokio::test]
    async fn test_analyze_module_cache_hit_metrics() {
        use std::io::Write as _;
        use tempfile::NamedTempFile;

        // Arrange: create a temp Rust file; prime the file cache via analyze_file handler
        let mut f = NamedTempFile::with_suffix(".rs").unwrap();
        writeln!(f, "fn bar() {{}}").unwrap();
        let path = f.path().to_str().unwrap().to_string();

        let analyzer = make_analyzer();

        // Prime the file cache by calling handle_file_details_mode once
        let mut file_params = aptu_coder_core::types::AnalyzeFileParams::default();
        file_params.path = path.clone();
        file_params.ast_recursion_limit = None;
        file_params.fields = None;
        file_params.pagination.cursor = None;
        file_params.pagination.page_size = None;
        file_params.output_control.summary = None;
        file_params.output_control.force = None;
        file_params.output_control.verbose = None;
        let (_cached, _) = analyzer
            .handle_file_details_mode(&file_params)
            .await
            .unwrap();

        // Act: now call analyze_module; the cache key is mtime-based so same file = hit
        let mut module_params = aptu_coder_core::types::AnalyzeModuleParams::default();
        module_params.path = path.clone();

        // Replicate the cache lookup the handler does (no public method; test via build path)
        let module_cache_key = std::fs::metadata(&path).ok().and_then(|meta| {
            meta.modified()
                .ok()
                .map(|mtime| aptu_coder_core::cache::CacheKey {
                    path: std::path::PathBuf::from(&path),
                    modified: mtime,
                    mode: aptu_coder_core::types::AnalysisMode::FileDetails,
                })
        });
        let cache_hit = module_cache_key
            .as_ref()
            .and_then(|k| analyzer.cache.get(k))
            .is_some();

        // Assert: the file cache must have been populated by the earlier handle_file_details_mode call
        assert!(
            cache_hit,
            "analyze_module should find the file in the shared file cache"
        );
        drop(module_params);
    }

    // --- import_lookup tests ---

    #[test]
    fn test_analyze_symbol_import_lookup_invalid_params() {
        // Arrange: empty symbol with import_lookup=true (violates the guard:
        // symbol must hold the module path when import_lookup=true).
        // Act: call the validate helper directly (same pattern as validate_impl_only).
        let result = CodeAnalyzer::validate_import_lookup(Some(true), "");

        // Assert: INVALID_PARAMS is returned.
        assert!(
            result.is_err(),
            "import_lookup=true with empty symbol must return Err"
        );
        let err = result.unwrap_err();
        assert_eq!(
            err.code,
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "expected INVALID_PARAMS; got {:?}",
            err.code
        );
    }

    #[tokio::test]
    async fn test_analyze_symbol_import_lookup_found() {
        use tempfile::TempDir;

        // Arrange: a Rust file that imports "std::collections"
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("main.rs"),
            "use std::collections::HashMap;\nfn main() {}\n",
        )
        .unwrap();

        let entries = traversal::walk_directory(dir.path(), None).unwrap();

        // Act: search for the module "std::collections"
        let output =
            analyze::analyze_import_lookup(dir.path(), "std::collections", &entries, None).unwrap();

        // Assert: one match found
        assert!(
            output.formatted.contains("MATCHES: 1"),
            "expected 1 match; got: {}",
            output.formatted
        );
        assert!(
            output.formatted.contains("main.rs"),
            "expected main.rs in output; got: {}",
            output.formatted
        );
    }

    #[tokio::test]
    async fn test_analyze_symbol_import_lookup_empty() {
        use tempfile::TempDir;

        // Arrange: a Rust file that does NOT import "no_such_module"
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

        let entries = traversal::walk_directory(dir.path(), None).unwrap();

        // Act
        let output =
            analyze::analyze_import_lookup(dir.path(), "no_such_module", &entries, None).unwrap();

        // Assert: zero matches
        assert!(
            output.formatted.contains("MATCHES: 0"),
            "expected 0 matches; got: {}",
            output.formatted
        );
    }

    // --- git_ref tests ---

    #[tokio::test]
    async fn test_analyze_directory_git_ref_non_git_repo() {
        use aptu_coder_core::traversal::changed_files_from_git_ref;
        use tempfile::TempDir;

        // Arrange: a temp dir that is NOT a git repository
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        // Act: attempt git_ref resolution in a non-git dir
        let result = changed_files_from_git_ref(dir.path(), "HEAD~1");

        // Assert: must return a GitError
        assert!(result.is_err(), "non-git dir must return an error");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("git"),
            "error must mention git; got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_analyze_directory_git_ref_filters_changed_files() {
        use aptu_coder_core::traversal::{changed_files_from_git_ref, filter_entries_by_git_ref};
        use std::collections::HashSet;
        use tempfile::TempDir;

        // Arrange: build a set of fake "changed" paths and a walk entry list
        let dir = TempDir::new().unwrap();
        let changed_file = dir.path().join("changed.rs");
        let unchanged_file = dir.path().join("unchanged.rs");
        std::fs::write(&changed_file, "fn changed() {}").unwrap();
        std::fs::write(&unchanged_file, "fn unchanged() {}").unwrap();

        let entries = traversal::walk_directory(dir.path(), None).unwrap();
        let total_files = entries.iter().filter(|e| !e.is_dir).count();
        assert_eq!(total_files, 2, "sanity: 2 files before filtering");

        // Simulate: only changed.rs is in the changed set
        let mut changed: HashSet<std::path::PathBuf> = HashSet::new();
        changed.insert(changed_file.clone());

        // Act: filter entries
        let filtered = filter_entries_by_git_ref(entries, &changed, dir.path());
        let filtered_files: Vec<_> = filtered.iter().filter(|e| !e.is_dir).collect();

        // Assert: only changed.rs remains
        assert_eq!(
            filtered_files.len(),
            1,
            "only 1 file must remain after git_ref filter"
        );
        assert_eq!(
            filtered_files[0].path, changed_file,
            "the remaining file must be the changed one"
        );

        // Verify changed_files_from_git_ref is at least callable (tested separately for non-git error)
        let _ = changed_files_from_git_ref;
    }

    #[tokio::test]
    async fn test_handle_overview_mode_git_ref_filters_via_handler() {
        use aptu_coder_core::types::{
            AnalyzeDirectoryParams, OutputControlParams, PaginationParams,
        };
        use std::process::Command;
        use tempfile::TempDir;

        // Arrange: create a real git repo with two commits.
        let dir = TempDir::new().unwrap();
        let repo = dir.path();

        // Init repo and configure minimal identity so git commit works.
        // Use no-hooks to avoid project-local commit hooks that enforce email allowlists.
        let git_no_hook = |repo_path: &std::path::Path, args: &[&str]| {
            let mut cmd = std::process::Command::new("git");
            cmd.args(["-c", "core.hooksPath=/dev/null"]);
            cmd.args(args);
            cmd.current_dir(repo_path);
            let out = cmd.output().unwrap();
            assert!(out.status.success(), "{out:?}");
        };
        git_no_hook(repo, &["init"]);
        git_no_hook(
            repo,
            &[
                "-c",
                "user.email=ci@example.com",
                "-c",
                "user.name=CI",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );

        // Commit file_a.rs in the first commit.
        std::fs::write(repo.join("file_a.rs"), "fn a() {}").unwrap();
        git_no_hook(repo, &["add", "file_a.rs"]);
        git_no_hook(
            repo,
            &[
                "-c",
                "user.email=ci@example.com",
                "-c",
                "user.name=CI",
                "commit",
                "-m",
                "add a",
            ],
        );

        // Add file_b.rs in a second commit (this is what HEAD changes relative to HEAD~1).
        std::fs::write(repo.join("file_b.rs"), "fn b() {}").unwrap();
        git_no_hook(repo, &["add", "file_b.rs"]);
        git_no_hook(
            repo,
            &[
                "-c",
                "user.email=ci@example.com",
                "-c",
                "user.name=CI",
                "commit",
                "-m",
                "add b",
            ],
        );

        // Act: call handle_overview_mode with git_ref=HEAD~1.
        // `git diff --name-only HEAD~1` compares working tree against HEAD~1, returning
        // only file_b.rs (added in the last commit, so present in working tree but not in HEAD~1).
        // Use the canonical path so walk entries match what `git rev-parse --show-toplevel` returns
        // (macOS /tmp is a symlink to /private/tmp; without canonicalization paths would differ).
        let canon_repo = std::fs::canonicalize(repo).unwrap();
        let analyzer = make_analyzer();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": canon_repo.to_str().unwrap(),
            "git_ref": "HEAD~1",
        }))
        .unwrap();
        let ct = tokio_util::sync::CancellationToken::new();
        let (arc_output, _cache_hit) = analyzer
            .handle_overview_mode(&params, ct)
            .await
            .expect("handle_overview_mode with git_ref must succeed");

        // Assert: only file_b.rs (changed since HEAD~1) appears; file_a.rs must be absent.
        let formatted = &arc_output.formatted;
        assert!(
            formatted.contains("file_b.rs"),
            "git_ref=HEAD~1 output must include file_b.rs; got:\n{formatted}"
        );
        assert!(
            !formatted.contains("file_a.rs"),
            "git_ref=HEAD~1 output must exclude file_a.rs; got:\n{formatted}"
        );
    }

    #[test]
    fn test_validate_path_rejects_absolute_path_outside_cwd() {
        // S4: Verify that absolute paths outside the current working directory are rejected.
        // This test directly calls validate_path with /etc/passwd, which should fail.
        let result = validate_path("/etc/passwd", true);
        assert!(
            result.is_err(),
            "validate_path should reject /etc/passwd (outside CWD)"
        );
        let err = result.unwrap_err();
        let err_msg = err.message.to_lowercase();
        assert!(
            err_msg.contains("outside") || err_msg.contains("not found"),
            "Error message should mention 'outside' or 'not found': {}",
            err.message
        );
    }

    #[test]
    fn test_validate_path_accepts_relative_path_in_cwd() {
        // Happy path: relative path within CWD should be accepted.
        // Use Cargo.toml which exists in the crate root.
        let result = validate_path("Cargo.toml", true);
        assert!(
            result.is_ok(),
            "validate_path should accept Cargo.toml (exists in CWD)"
        );
    }

    #[test]
    fn test_validate_path_creates_parent_for_nonexistent_file() {
        // Edge case: non-existent file with non-existent parent should still be accepted
        // if the ancestor chain leads back to CWD.
        let result = validate_path("nonexistent_dir/nonexistent_file.txt", false);
        assert!(
            result.is_ok(),
            "validate_path should accept non-existent file with non-existent parent (require_exists=false)"
        );
        let path = result.unwrap();
        let cwd = std::env::current_dir().expect("should get cwd");
        let canonical_cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
        assert!(
            path.starts_with(&canonical_cwd),
            "Resolved path should be within CWD: {:?} should start with {:?}",
            path,
            canonical_cwd
        );
    }
}
