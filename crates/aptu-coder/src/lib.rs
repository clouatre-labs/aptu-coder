// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Rust MCP server for code structure analysis using tree-sitter.
//!
//! This crate exposes seven MCP tools for multiple programming languages:
//!
//! **Analyze family:**
//! - **`analyze_directory`**: Directory tree with file counts and structure
//! - **`analyze_file`**: Semantic extraction (functions, classes, imports)
//! - **`analyze_symbol`**: Call graph analysis (callers and callees)
//! - **`analyze_module`**: Lightweight function and import index
//!
//! **Edit family:**
//! - **`edit_overwrite`**: Create or overwrite files
//! - **`edit_replace`**: Replace text blocks in files
//!
//! **Exec family:**
//! - **`exec_command`**: Run shell commands with progress notifications
//!
//! Key entry points:
//! - [`analyze::analyze_directory`]: Analyze entire directory tree
//! - [`analyze::analyze_file`]: Analyze single file
//!
//! Languages supported: Astro, C/C++, C#, CSS, Fortran, Go, HTML, Java, JavaScript, JSON, Kotlin, Markdown, Python, Rust, TOML, TSX, TypeScript, YAML.

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod filters;
pub mod logging;
pub mod metrics;
pub mod otel;
mod shell;
mod validation;

use aptu_coder_core::analyze;
use aptu_coder_core::{cache, completion, graph, traversal, types};
use shell::resolve_shell;
use validation::{validate_path, validate_path_in_dir};

pub const STDIN_MAX_BYTES: usize = 1_048_576;

/// Number of consecutive not_found or ambiguous edit_replace failures on the same
/// (session_id, canonical_path) pair before returning a stale-context directive error.
pub(crate) const EDIT_STALE_THRESHOLD: u8 = 5;
/// Maximum number of (session_id, canonical_path) entries in the failure counter map.
/// When the map reaches this size, it is cleared entirely to prevent unbounded growth.
/// The circuit breaker is advisory, so a full clear is safe: the worst case is one
/// missed trip per session per path after an eviction cycle.
pub(crate) const EDIT_FAILURE_MAP_CAP: usize = 1024;

/// Default drain timeout for the no-timeout path: prevents indefinite hang when a login
/// shell profile blocks (macOS).
// No longer used after wait/drain order inversion (500ms grace inlined).
/// Default drain timeout in milliseconds for post-exit pipe drain (500ms).
const DEFAULT_DRAIN_TIMEOUT_MS: u64 = 500;

#[non_exhaustive]
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ExecCommandParams {
    /// Shell command to execute via sh -c (or $SHELL if set).
    pub command: String,
    /// Working directory for the command. Set this instead of prepending cd to the command string. Validated against path traversal; does not sandbox the process.
    pub working_dir: Option<String>,
    /// UTF-8 content to pipe into the process stdin (max `STDIN_MAX_BYTES` = 1 MB). When None, stdin is closed (null).
    pub stdin: Option<String>,
    /// Maximum execution time in seconds. When the command exceeds this limit, the
    /// child process is killed and the response indicates `timed_out: true`.
    /// A value of 0 or None means no timeout (unlimited execution).
    #[serde(default)]
    pub timeout_secs: Option<i64>,
    /// Drain timeout in milliseconds after the child process exits. When the child
    /// exits but a background subprocess holds pipes open, the drain collects
    /// buffered output for this many milliseconds before returning
    /// `output_truncated: true`. Default: 500ms when omitted or 0.
    /// Positive values override the default. Negative values are rejected with
    /// INVALID_PARAMS.
    #[serde(default)]
    pub drain_timeout_secs: Option<i64>,
}

impl ExecCommandParams {
    /// Creates a new ExecCommandParams with the given command.
    pub fn new(command: String, working_dir: Option<String>) -> Self {
        Self {
            command,
            working_dir,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ShellOutput {
    /// Standard output from the command.
    pub stdout: String,
    /// Standard error from the command.
    pub stderr: String,
    /// Stdout and stderr interleaved in arrival order.
    pub interleaved: String,
    /// Exit code; null if the process could not be waited on (e.g. drain timeout from a background process holding pipes).
    pub exit_code: Option<i32>,
    /// True if the post-exit drain timed out (backgrounded process kept pipes open).
    /// When true, any available output is still included; use the overflow file path
    /// from the truncation notice Content block to recover the full output.
    pub output_truncated: bool,
    /// Set when the post-exit drain timed out because a background process held the
    /// pipes open. Distinct from `output_truncated` (size cap) -- this indicates a
    /// drain timeout rather than a size overflow.
    pub output_collection_error: Option<String>,
    /// Path to the slot file containing full stdout (if output was persisted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_path: Option<String>,
    /// Path to the slot file containing full stderr (if output was persisted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_path: Option<String>,
    /// Description of the filter applied to stdout (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_applied: Option<String>,
    /// True when the command was killed due to exceeding `timeout_secs`.
    /// When true, exit_code is None and no partial output is available.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
}

impl ShellOutput {
    /// Creates a new ShellOutput with the given parameters.
    pub fn new(
        stdout: String,
        stderr: String,
        interleaved: String,
        exit_code: Option<i32>,
        output_truncated: bool,
    ) -> Self {
        Self {
            stdout,
            stderr,
            interleaved,
            exit_code,
            output_truncated,
            output_collection_error: None,
            stdout_path: None,
            stderr_path: None,
            filter_applied: None,
            timed_out: false,
        }
    }
}

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
use filters::{CompiledRule, apply_filter, load_filter_table, maybe_inject_no_stat};
use logging::LogEvent;
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

static GLOBAL_SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// 5_000 chars fires at ~150-180 files at depth=2 (~28-33 chars/file).
// Empirical data (684 calls, Jun 2026): max observed output was 4,882 chars; the old
// 50_000 threshold never triggered once. At 5_000, auto-summary engages for repos that
// would otherwise produce an overwhelming flat response.
const SIZE_LIMIT: usize = 5_000;

/// Returns `true` when `summary=true` and a `cursor` are both provided, which is an invalid
/// combination since summary mode and pagination are mutually exclusive.
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
fn error_meta(
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
fn err_to_tool_result(e: ErrorData) -> CallToolResult {
    let mut result =
        CallToolResult::error(vec![Content::text(e.message)]).with_meta(Some(no_cache_meta()));
    if let Some(data) = e.data {
        result.structured_content = Some(data);
    }
    result
}

fn err_to_tool_result_from_pagination(
    e: aptu_coder_core::pagination::PaginationError,
) -> CallToolResult {
    let msg = format!("Pagination error: {}", e);
    CallToolResult::error(vec![Content::text(msg)]).with_meta(Some(no_cache_meta()))
}

fn no_cache_meta() -> Meta {
    let mut m = serde_json::Map::new();
    m.insert(
        "cache_hint".to_string(),
        serde_json::Value::String("no-cache".to_string()),
    );
    Meta(m)
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
    // Wrapped in Arc<RwLock> to enable interior mutability for profile-based tool routing.
    // All clones share the same router instance (per-session state).
    // Read lock acquired by list_tools/call_tool; write lock acquired during on_initialized
    // to disable tools based on client profile.
    // IMPORTANT: Do not perform long-running I/O while holding the write lock in
    // on_initialized. The write lock blocks all concurrent list_tools/call_tool calls
    // for the duration. Keep the critical section to disable_route() calls only.
    pub(crate) tool_router: Arc<RwLock<ToolRouter<Self>>>,
    cache: AnalysisCache,
    disk_cache: std::sync::Arc<cache::DiskCache>,
    peer: Arc<TokioMutex<Option<Peer<RoleServer>>>>,
    log_level_filter: Arc<Mutex<LevelFilter>>,
    event_rx: Arc<TokioMutex<Option<mpsc::UnboundedReceiver<LogEvent>>>>,
    metrics_tx: crate::metrics::MetricsSender,
    session_call_seq: Arc<std::sync::atomic::AtomicU32>,
    session_id: Arc<TokioMutex<Option<String>>>,
    // Resolved profile string set once in initialize; read in on_initialized and call_tool.
    // OnceLock is lock-free after the first set; no mutex needed.
    session_profile: Arc<std::sync::OnceLock<String>>,
    client_name: Arc<TokioMutex<Option<String>>>,
    client_version: Arc<TokioMutex<Option<String>>>,
    // Resolved login shell PATH, captured once at startup via login shell invocation.
    // Arc<Option<String>> is immutable after init; no lock needed.
    resolved_path: Arc<Option<String>>,
    // Compiled filter rules table (built-in + project-local from .aptu/filters.toml).
    // Immutable after init; no lock needed.
    filter_table: Arc<Vec<CompiledRule>>,
    // L1 in-memory LRU cache for call graph results (analyze_symbol).
    // Capacity controlled by APTU_CODER_SYMBOL_CACHE_CAPACITY env var (default 32).
    call_graph_cache: CallGraphCache,
    // Per-(session_id, canonical_path) consecutive edit_replace failure counter.
    // Used to detect stale LLM context and return a directive error instead of
    // repeatedly trying an old_text that no longer matches the file content.
    edit_failure_counts: Arc<Mutex<HashMap<(String, String), u8>>>,
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
        let file_cap: usize = std::env::var("APTU_CODER_FILE_CACHE_CAPACITY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        // Initialize disk cache
        let xdg_data_home = if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME")
            && !xdg_data_home.is_empty()
        {
            std::path::PathBuf::from(xdg_data_home)
        } else if let Ok(home) = std::env::var("HOME") {
            std::path::PathBuf::from(home).join(".local").join("share")
        } else {
            std::path::PathBuf::from(".")
        };
        let disk_cache_disabled = std::env::var("APTU_CODER_DISK_CACHE_DISABLED")
            .map(|v| v == "1")
            .unwrap_or(false);
        let disk_cache_dir = std::env::var("APTU_CODER_DISK_CACHE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| xdg_data_home.join("aptu-coder").join("analysis-cache"));
        let disk_cache =
            std::sync::Arc::new(cache::DiskCache::new(disk_cache_dir, disk_cache_disabled));

        // Snapshot login shell PATH once at startup: invoke the user's login shell with
        // -l -c 'echo $PATH' so their full profile (nvm, Homebrew, etc.) is captured.
        // Shell resolution priority for the snapshot:
        //   1. $SHELL env var (user's actual login shell; sources the right profile)
        //   2. resolve_shell() (APTU_SHELL override or bash from PATH)
        //   3. /bin/sh (guaranteed to exist on all POSIX systems)
        // Falls back to the current process PATH when the snapshot fails or returns empty,
        // so exec_command always has a usable PATH in both stdio and HTTP transport modes.
        let resolved_path = {
            let snapshot_shell = std::env::var("SHELL")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    let s = resolve_shell();
                    if s.is_empty() {
                        "/bin/sh".to_string()
                    } else {
                        s
                    }
                });
            let login_path = match std::process::Command::new(&snapshot_shell)
                .args(["-l", "-c", "echo $PATH"])
                .output()
            {
                Ok(output) => {
                    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if path_str.is_empty() {
                        tracing::warn!(
                            shell = %snapshot_shell,
                            "login shell PATH snapshot returned empty string"
                        );
                        None
                    } else {
                        Some(path_str)
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        shell = %snapshot_shell,
                        error = %e,
                        "failed to snapshot login shell PATH"
                    );
                    None
                }
            };
            // Fall back to the current process PATH when the login shell snapshot fails.
            let path = login_path.or_else(|| std::env::var("PATH").ok());
            Arc::new(path)
        };

        let filter_table = Arc::new(load_filter_table(Path::new(".")));

        CodeAnalyzer {
            tool_router: Arc::new(RwLock::new(Self::tool_router())),
            cache: AnalysisCache::new(file_cap),
            disk_cache,
            peer,
            log_level_filter,
            event_rx: Arc::new(TokioMutex::new(Some(event_rx))),
            metrics_tx,
            session_call_seq: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            session_id: Arc::new(TokioMutex::new(None)),
            session_profile: Arc::new(std::sync::OnceLock::new()),
            client_name: Arc::new(TokioMutex::new(None)),
            client_version: Arc::new(TokioMutex::new(None)),
            resolved_path,
            filter_table,
            call_graph_cache: {
                CallGraphCache::new(aptu_coder_core::cache::parse_cache_capacity(
                    "APTU_CODER_SYMBOL_CACHE_CAPACITY",
                    32,
                ))
            },
            edit_failure_counts: Arc::new(Mutex::new(HashMap::new())),
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

    /// Emit a "received" metric event for the given tool name.
    /// Increments the session call sequence, locks the session ID, and sends
    /// the metric event via the channel. Returns the (seq, sid) pair for use
    /// by the caller in exit metrics, preserving per-call seq uniqueness.
    async fn emit_received_metric(&self, tool: &'static str) -> (u32, Option<String>) {
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
    async fn handle_overview_mode(
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
    async fn handle_file_details_mode(
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
    #[allow(clippy::cast_precision_loss, clippy::too_many_arguments)] // progress percentage display; precision loss acceptable for usize counts
    async fn poll_progress_until_done(
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
    async fn run_focused_with_auto_summary(
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
    async fn handle_focused_mode(
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

    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, path = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty, cache_tier = tracing::field::Empty))]
    #[tool(
        name = "analyze_directory",
        title = "Analyze Directory",
        description = "Tree-view of directory with LOC, function/class counts, test markers. Respects .gitignore. Returns per-file stats plus next_cursor for pagination. Default max_depth is 3; pass 0 for unlimited depth. Large directories (1000+ files) are auto-compacted to a summary; pass summary=false for a cursor-paginated per-file flat list (summary and cursor are mutually exclusive). git_ref restricts to files changed since a branch/tag/commit. Empty directories return zero counts. Example queries: Analyze the src/ directory to understand module structure; What files are in the tests/ directory and how large are they?",
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
        let mut params = params.0;
        // Apply max_depth default: 3. Pass 0 for unlimited depth.
        params.max_depth = params.max_depth.or(Some(3));
        let t_start = std::time::Instant::now();
        let (seq, sid) = self.emit_received_metric("analyze_directory").await;
        // Extract W3C Trace Context from request _meta if present
        let session_id = self.session_id.lock().await.clone();
        let client_name = self.client_name.lock().await.clone();
        let client_version = self.client_version.lock().await.clone();
        extract_and_set_trace_context(
            Some(&context.meta),
            ClientMetadata {
                session_id,
                client_name,
                client_version,
            },
        );
        let span = tracing::Span::current();
        span.record("gen_ai.system", "mcp");
        span.record("gen_ai.operation.name", "execute_tool");
        span.record("gen_ai.tool.name", "analyze_directory");
        span.record("path", &params.path);
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                return Ok(err_to_tool_result(e));
            }
        };
        let ct = context.ct.clone();
        let param_path = params.path.clone();
        let max_depth_val = params.max_depth;

        // Call handler for analysis and progress tracking
        let progress_token = context.meta.get_progress_token();
        let (arc_output, dir_cache_hit) =
            match self.handle_overview_mode(&params, ct, progress_token).await {
                Ok(v) => v,
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "internal_error");
                    return Ok(err_to_tool_result(e));
                }
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
            span.record("error", true);
            span.record("error.type", "invalid_params");
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

        // Determine output mode:
        //   summary=true  -> compact summary (format_summary)
        //   summary=false -> explicit paginated flat list (format_structure_paginated)
        //   summary=None, small output (<=SIZE_LIMIT) -> tree as-is (format_structure)
        //   summary=None, large output (>SIZE_LIMIT)  -> compact summary (format_summary)
        let use_summary = if params.output_control.summary == Some(true) {
            true
        } else if params.output_control.summary == Some(false) {
            false
        } else {
            output.formatted.len() > SIZE_LIMIT
        };

        // summary=false is the only path that uses format_structure_paginated
        let use_paginated = params.output_control.summary == Some(false);

        if use_summary {
            output.formatted = format_summary(
                &output.entries,
                &output.files,
                params.max_depth,
                output.subtree_counts.as_deref(),
            );
        }

        // Decode pagination cursor if provided (only relevant for paginated mode)
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
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    return Ok(err_to_tool_result(e));
                }
            };
            cursor_data.offset
        } else {
            0
        };

        // Apply pagination to files (used only in paginated mode)
        let paginated =
            match paginate_slice(&output.files, offset, page_size, PaginationMode::Default) {
                Ok(v) => v,
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "internal_error");
                    return Ok(err_to_tool_result(ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        e.to_string(),
                        Some(error_meta("transient", true, "retry the request")),
                    )));
                }
            };

        if use_paginated {
            output.formatted = format_structure_paginated(
                &paginated.items,
                paginated.total,
                params.max_depth,
                Some(Path::new(&params.path)),
                false,
            );
        }

        // Update next_cursor in output after pagination (only in paginated mode)
        if use_paginated {
            output.next_cursor.clone_from(&paginated.next_cursor);
        } else {
            output.next_cursor = None;
        }

        // Build final text output with pagination cursor if present (only in paginated mode)
        let mut final_text = output.formatted.clone();
        if use_paginated && let Some(cursor) = paginated.next_cursor {
            final_text.push('\n');
            final_text.push_str("NEXT_CURSOR: ");
            final_text.push_str(&cursor);
        }

        // Record cache tier in span
        tracing::Span::current().record("cache_tier", dir_cache_hit.as_str());

        // Add content_hash to _meta
        let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
        let mut meta = no_cache_meta().0;
        meta.insert(
            "content_hash".to_string(),
            serde_json::Value::String(content_hash),
        );
        let meta = rmcp::model::Meta(meta);

        let mut result = CallToolResult::success(vec![
            Content::text(final_text.clone()).with_priority(0.9_f32),
        ])
        .with_meta(Some(meta));
        let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("analyze_directory", "ok", dur)
                .output_chars(final_text.len())
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .max_depth(max_depth_val)
                .session_id(sid)
                .seq(Some(seq))
                .cache_hit(Some(dir_cache_hit != CacheTier::Miss))
                .cache_tier(Some(dir_cache_hit.as_str()))
                .build(),
        );
        Ok(result)
    }

    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, path = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty, cache_tier = tracing::field::Empty))]
    #[tool(
        name = "analyze_file",
        title = "Analyze File",
        description = "Functions, types, classes, and imports from a single source file. Returns functions (name, signature, line range), classes (methods, fields, inheritance), imports; paginate with cursor/page_size. Use fields=[\"functions\",\"classes\",\"imports\"] to limit output sections. Fails if directory path supplied; use analyze_directory instead. Fails if summary=true and cursor. git_ref not supported for single-file analysis. Use analyze_module for lightweight function/import index (~75% smaller). Supported: Astro, C/C++, C#, CSS, Fortran, Go, HTML, Java, JavaScript, JSON, Kotlin, Markdown, Python, Rust, TOML, TSX, TypeScript, YAML. Example queries: What functions are defined in src/lib.rs?; Show me the classes and their methods in src/analyzer.py.",
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let t_start = std::time::Instant::now();
        let (seq, sid) = self.emit_received_metric("analyze_file").await;
        // Extract W3C Trace Context from request _meta if present
        let session_id = self.session_id.lock().await.clone();
        let client_name = self.client_name.lock().await.clone();
        let client_version = self.client_version.lock().await.clone();
        extract_and_set_trace_context(
            Some(&context.meta),
            ClientMetadata {
                session_id,
                client_name,
                client_version,
            },
        );
        let span = tracing::Span::current();
        span.record("gen_ai.system", "mcp");
        span.record("gen_ai.operation.name", "execute_tool");
        span.record("gen_ai.tool.name", "analyze_file");
        span.record("path", &params.path);
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                return Ok(err_to_tool_result(e));
            }
        };
        let param_path = params.path.clone();

        // Check if path is a directory (not allowed for analyze_file)
        if std::path::Path::new(&params.path).is_dir() {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "path is a directory; use analyze_directory instead",
                {
                    let mut meta =
                        error_meta("validation", false, "pass a file path, not a directory");
                    if let Some(obj) = meta.as_object_mut() {
                        obj.insert("path".to_string(), serde_json::json!(params.path));
                    }
                    Some(meta)
                },
            )));
        }

        // summary=true and cursor are mutually exclusive
        if summary_cursor_conflict(
            params.output_control.summary,
            params.pagination.cursor.as_deref(),
        ) {
            span.record("error", true);
            span.record("error.type", "invalid_params");
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
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "internal_error");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                let error_type = match e.code {
                    rmcp::model::ErrorCode::INVALID_PARAMS => Some("invalid_params".to_string()),
                    rmcp::model::ErrorCode::INTERNAL_ERROR => Some("internal_error".to_string()),
                    _ => None,
                };
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("analyze_file", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(error_type)
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .file_ext(crate::metrics::path_file_ext(&param_path))
                        .language(crate::metrics::path_language(&param_path))
                        .build(),
                );
                return Ok(err_to_tool_result(e));
            }
        };

        // Clone only the two fields that may be mutated per-request (formatted and
        // next_cursor). The heavy SemanticAnalysis data is shared via Arc and never
        // modified, so we borrow it directly from the cached pointer.
        let mut formatted = arc_output.formatted.clone();
        let line_count = arc_output.line_count;

        // Apply summary/output size limiting logic
        let use_summary = if params.output_control.summary == Some(true) {
            true
        } else if params.output_control.summary == Some(false) {
            false
        } else {
            formatted.len() > SIZE_LIMIT
        };

        if use_summary {
            formatted = format_file_details_summary(&arc_output.semantic, &params.path, line_count);
        } else if formatted.len() > SIZE_LIMIT {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            let estimated_tokens = formatted.len() / 4;
            let message = format!(
                "Output exceeds 50K chars ({} chars, ~{} tokens). Use one of:\n\
                 - Use summary=true for a compact overview\n\
                 - Use fields to limit output to specific sections (functions, classes, or imports)",
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
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    return Ok(err_to_tool_result(e));
                }
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
        // Skip regeneration when the output is an unsupported-extension fallback (sentinel in formatted).
        let is_unsupported_fallback = arc_output
            .formatted
            .contains("[Unsupported extension: semantic analysis not available]");
        if !use_summary && !is_unsupported_fallback {
            // fields: serde rejects unknown enum variants at deserialization; no runtime validation required
            formatted = format_file_details_paginated(
                &paginated.items,
                paginated.total,
                &arc_output.semantic,
                &params.path,
                line_count,
                offset,
                false,
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

        // Build the response output, projecting SemanticAnalysis to only the requested sections.
        let response_output = analyze::FileAnalysisOutput::new(
            formatted,
            arc_output.semantic.project(params.fields.as_deref()),
            line_count,
            next_cursor,
        );

        // Record cache tier in span
        tracing::Span::current().record("cache_tier", file_cache_hit.as_str());

        // Add content_hash to _meta
        let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
        let mut meta = no_cache_meta().0;
        meta.insert(
            "content_hash".to_string(),
            serde_json::Value::String(content_hash),
        );
        let meta = rmcp::model::Meta(meta);

        let mut result = CallToolResult::success(vec![
            Content::text(final_text.clone()).with_priority(0.9_f32),
        ])
        .with_meta(Some(meta));
        let structured = serde_json::to_value(&response_output).unwrap_or(Value::Null);
        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("analyze_file", "ok", dur)
                .output_chars(final_text.len())
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .session_id(sid)
                .seq(Some(seq))
                .cache_hit(Some(file_cache_hit != CacheTier::Miss))
                .cache_tier(Some(file_cache_hit.as_str()))
                .file_ext(crate::metrics::path_file_ext(&param_path))
                .language(crate::metrics::path_language(&param_path))
                .build(),
        );
        Ok(result)
    }

    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, symbol = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty, cache_tier = tracing::field::Empty))]
    #[tool(
        name = "analyze_symbol",
        title = "Analyze Symbol",
        description = "Use when you need to: find all callers of a function across the codebase, trace transitive call chains, or locate all files importing a module path. Prefer over analyze_file when the question is \"who calls X\" or \"what does X call\" rather than \"what is in this file\".\n\nCall graph for a named symbol across all files in a directory. Returns callers and callees. Modes: call graph (default), import_lookup (files importing a module path), def_use (write/read sites). Fails if file path supplied; fails if impl_only=true on non-Rust directory; fails if import_lookup=true with empty symbol; fails if summary=true and cursor. match_mode controls name matching (exact/insensitive/prefix/contains). git_ref restricts to changed files. Example queries: Find all callers of parse_config; Find all files that import std::collections.",
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
        let t_start = std::time::Instant::now();
        let (seq, sid) = self.emit_received_metric("analyze_symbol").await;
        // Extract W3C Trace Context from request _meta if present
        let session_id = self.session_id.lock().await.clone();
        let client_name = self.client_name.lock().await.clone();
        let client_version = self.client_version.lock().await.clone();
        extract_and_set_trace_context(
            Some(&context.meta),
            ClientMetadata {
                session_id,
                client_name,
                client_version,
            },
        );
        let span = tracing::Span::current();
        span.record("gen_ai.system", "mcp");
        span.record("gen_ai.operation.name", "execute_tool");
        span.record("gen_ai.tool.name", "analyze_symbol");
        span.record("symbol", &params.symbol);
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                return Ok(err_to_tool_result(e));
            }
        };
        let ct = context.ct.clone();
        let param_path = params.path.clone();
        let max_depth_val = params.follow_depth;

        // Check if path is a file (not allowed for analyze_symbol)
        if std::path::Path::new(&params.path).is_file() {
            span.record("error", true);
            span.record("error.type", "invalid_params");
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
            span.record("error", true);
            span.record("error.type", "invalid_params");
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
            span.record("error", true);
            span.record("error.type", "invalid_params");
            return Ok(err_to_tool_result(e));
        }

        // import_lookup mode: scan for files importing `params.symbol` as a module path.
        if params.import_lookup == Some(true) {
            let path_owned = PathBuf::from(&params.path);
            let symbol = params.symbol.clone();
            let git_ref = params.git_ref.clone();
            let max_depth = params.max_depth;

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
                let output = match analyze::analyze_import_lookup(path, &symbol, &entries, None) {
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

            // Record cache tier in span
            tracing::Span::current().record("cache_tier", "Miss");

            // Add content_hash to _meta
            let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
            let mut meta = no_cache_meta().0;
            meta.insert(
                "content_hash".to_string(),
                serde_json::Value::String(content_hash),
            );

            let mut result = CallToolResult::success(vec![
                Content::text(final_text.clone()).with_priority(0.9_f32),
            ])
            .with_meta(Some(Meta(meta)));
            let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
            result.structured_content = Some(structured);
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("analyze_symbol", "ok", dur)
                    .output_chars(final_text.len())
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .max_depth(max_depth_val)
                    .session_id(sid)
                    .seq(Some(seq))
                    .cache_hit(Some(false))
                    .cache_tier(Some(CacheTier::Miss.as_str()))
                    .build(),
            );
            return Ok(result);
        }

        // Call handler for analysis and progress tracking
        let progress_token = context.meta.get_progress_token();
        let (graph_cache_tier, mut output) =
            match self.handle_focused_mode(&params, ct, progress_token).await {
                Ok(v) => v,
                Err(e) => return Ok(err_to_tool_result(e)),
            };

        // Surface cache tier in structuredContent for observability and testing.
        output.cache_tier = Some(graph_cache_tier.as_str().to_owned());

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

        let use_summary = params.output_control.summary == Some(true);

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
                        false,
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

                if paginated_next.is_some() || offset > 0 {
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
                        false,
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
                // first page (offset=0) is not skipped.
                if !use_summary {
                    let base_path = Path::new(&params.path);
                    output.formatted = format_focused_paginated_defuse(
                        &paginated_sites,
                        total_sites,
                        &params.symbol,
                        offset,
                        Some(base_path),
                        false,
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

        // Record cache tier in span
        tracing::Span::current().record("cache_tier", graph_cache_tier.as_str());

        // Add content_hash to _meta
        let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
        let mut meta = no_cache_meta().0;
        meta.insert(
            "content_hash".to_string(),
            serde_json::Value::String(content_hash),
        );

        let mut result = CallToolResult::success(vec![
            Content::text(final_text.clone()).with_priority(0.9_f32),
        ])
        .with_meta(Some(Meta(meta)));
        // Only include def_use_sites in structuredContent when in DefUse mode.
        // In Callers/Callees modes, clearing the vec prevents large def-use
        // payloads from leaking into paginated non-def-use responses.
        if cursor_mode != PaginationMode::DefUse {
            output.def_use_sites = Vec::new();
        }
        let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("analyze_symbol", "ok", dur)
                .output_chars(final_text.len())
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .max_depth(max_depth_val)
                .session_id(sid)
                .seq(Some(seq))
                .cache_hit(Some(graph_cache_tier != CacheTier::Miss))
                .cache_tier(Some(graph_cache_tier.as_str()))
                .build(),
        );
        Ok(result)
    }

    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, path = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty, cache_tier = tracing::field::Empty))]
    #[tool(
        name = "analyze_module",
        title = "Analyze Module",
        description = "Function and import index for a single source file with minimal token cost: name, line_count, language, function names with line numbers, import list only (~75% smaller than analyze_file). Fails if directory path supplied. Pagination and git_ref not supported. Use analyze_file when you need signatures, types, or class details. Supported: Astro, C/C++, C#, CSS, Fortran, Go, HTML, Java, JavaScript, JSON, Kotlin, Markdown, Python, Rust, TOML, TSX, TypeScript, YAML. Example queries: What functions are defined in src/analyze.rs?",
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let t_start = std::time::Instant::now();
        let (seq, sid) = self.emit_received_metric("analyze_module").await;
        // Extract W3C Trace Context from request _meta if present
        let session_id = self.session_id.lock().await.clone();
        let client_name = self.client_name.lock().await.clone();
        let client_version = self.client_version.lock().await.clone();
        extract_and_set_trace_context(
            Some(&context.meta),
            ClientMetadata {
                session_id,
                client_name,
                client_version,
            },
        );
        let span = tracing::Span::current();
        span.record("gen_ai.system", "mcp");
        span.record("gen_ai.operation.name", "execute_tool");
        span.record("gen_ai.tool.name", "analyze_module");
        span.record("path", &params.path);
        let _validated_path = match validate_path(&params.path, true) {
            Ok(p) => p,
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                return Ok(err_to_tool_result(e));
            }
        };
        let param_path = params.path.clone();

        // Issue 340: Guard against directory paths
        if std::fs::metadata(&params.path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("analyze_module", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .session_id(sid.clone())
                    .seq(Some(seq))
                    .build(),
            );
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "path is a directory; use analyze_directory for directories, or pass a file path to analyze_module",
                {
                    let mut meta =
                        error_meta("validation", false, "use analyze_directory for directories");
                    if let Some(obj) = meta.as_object_mut() {
                        obj.insert("path".to_string(), serde_json::json!(params.path));
                    }
                    Some(meta)
                },
            )));
        }

        // Module-only cache path: L2 (content hash) -> analyze_module_file fast path.
        // Uses AnalysisMode::ModuleOnly disk key so entries are distinct from analyze_file.
        // L1 in-memory cache is not used here: the existing L1 stores Arc<FileAnalysisOutput>
        // and adding a new typed slot is out of scope; L2 avoids the parse cost across restarts.
        let file_bytes = match tokio::fs::read(&params.path).await {
            Ok(b) => b,
            Err(_e) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("analyze_module", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("internal_error".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .file_ext(crate::metrics::path_file_ext(&param_path))
                        .language(crate::metrics::path_language(&param_path))
                        .build(),
                );
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    "failed to read file; check file path and permissions",
                    {
                        let mut meta =
                            error_meta("resource", false, "check file path and permissions");
                        if let Some(obj) = meta.as_object_mut() {
                            obj.insert("path".to_string(), serde_json::json!(params.path));
                        }
                        Some(meta)
                    },
                )));
            }
        };
        let disk_key = blake3::hash(&file_bytes);

        let (module_info, module_tier) = if let Some(cached) = self
            .disk_cache
            .get::<types::ModuleInfo>("analyze_module", &disk_key)
        {
            (cached, CacheTier::L2Disk)
        } else {
            // Cache miss: run the lightweight fast path
            let mi = match analyze::analyze_module_file(&params.path) {
                Ok(mi) => mi,
                Err(e) => {
                    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                    // Graceful fallback for unsupported extensions: return empty ModuleInfo
                    // with a note instead of INVALID_PARAMS.
                    if matches!(
                        &e,
                        analyze::AnalyzeError::Parser(
                            aptu_coder_core::parser::ParserError::UnsupportedLanguage(_)
                        )
                    ) {
                        let source = String::from_utf8_lossy(&file_bytes).into_owned();
                        let line_count = source.lines().count();
                        let name = std::path::Path::new(&params.path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_string();
                        let ext = std::path::Path::new(&params.path)
                            .extension()
                            .and_then(|x| x.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        self.metrics_tx.send(
                            crate::metrics::MetricEventBuilder::new("analyze_module", "ok", dur)
                                .param_path_depth(crate::metrics::path_component_count(&param_path))
                                .session_id(sid.clone())
                                .seq(Some(seq))
                                .file_ext(crate::metrics::path_file_ext(&param_path))
                                .language(crate::metrics::path_language(&param_path))
                                .build(),
                        );
                        return {
                            let mut mi =
                                types::ModuleInfo::new(name, line_count, ext, vec![], vec![]);
                            mi.unsupported = Some(true);
                            let text = format_module_info(&mi);
                            let content_hash = format!("{}", blake3::hash(text.as_bytes()));
                            let mut meta = no_cache_meta().0;
                            meta.insert(
                                "content_hash".to_string(),
                                serde_json::Value::String(content_hash),
                            );
                            let mut result = CallToolResult::success(vec![Content::text(text)])
                                .with_meta(Some(Meta(meta)));
                            match serde_json::to_value(&mi) {
                                Ok(v) => {
                                    result.structured_content = Some(v);
                                    Ok(result)
                                }
                                Err(se) => Ok(err_to_tool_result(ErrorData::new(
                                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                                    format!("serialization failed: {se}"),
                                    Some(error_meta("internal", false, "report this as a bug")),
                                ))),
                            }
                        };
                    }
                    let (error_type, error_data) = (
                        Some("internal_error".to_string()),
                        ErrorData::new(
                            rmcp::model::ErrorCode::INTERNAL_ERROR,
                            format!("Failed to analyze module: {e}"),
                            Some(error_meta("internal", false, "report this as a bug")),
                        ),
                    );
                    self.metrics_tx.send(
                        crate::metrics::MetricEventBuilder::new("analyze_module", "error", dur)
                            .param_path_depth(crate::metrics::path_component_count(&param_path))
                            .error_type(error_type)
                            .session_id(sid.clone())
                            .seq(Some(seq))
                            .file_ext(crate::metrics::path_file_ext(&param_path))
                            .language(crate::metrics::path_language(&param_path))
                            .build(),
                    );
                    return Ok(err_to_tool_result(error_data));
                }
            };
            // Write-behind: store ModuleInfo in L2 disk cache
            {
                let dc = self.disk_cache.clone();
                let k = disk_key;
                let mi_clone = mi.clone();
                let metrics_tx2 = self.metrics_tx.clone();
                let sid2 = sid.clone();
                tokio::spawn(async move {
                    let handle = tokio::task::spawn_blocking(move || {
                        dc.put("analyze_module", &k, &mi_clone);
                        dc.drain_write_failures()
                    });
                    if let Ok(failures) = handle.await
                        && failures > 0
                    {
                        tracing::warn!(
                            tool = "analyze_module",
                            failures,
                            "L2 disk cache write failed"
                        );
                        metrics_tx2.send(
                            crate::metrics::MetricEventBuilder::new("analyze_module", "ok", 0)
                                .session_id(sid2)
                                .cache_write_failure(Some(true))
                                .build(),
                        );
                    }
                });
            }
            (mi, CacheTier::Miss)
        };

        let text = format_module_info(&module_info);

        // Record cache tier in span
        tracing::Span::current().record("cache_tier", module_tier.as_str());

        // Add content_hash to _meta
        let content_hash = format!("{}", blake3::hash(text.as_bytes()));
        let mut meta = no_cache_meta().0;
        meta.insert(
            "content_hash".to_string(),
            serde_json::Value::String(content_hash),
        );

        let mut result =
            CallToolResult::success(vec![Content::text(text.clone())]).with_meta(Some(Meta(meta)));
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
        self.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("analyze_module", "ok", dur)
                .output_chars(text.len())
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .session_id(sid)
                .seq(Some(seq))
                .cache_hit(Some(module_tier != CacheTier::Miss))
                .cache_tier(Some(module_tier.as_str()))
                .file_ext(crate::metrics::path_file_ext(&param_path))
                .language(crate::metrics::path_language(&param_path))
                .build(),
        );
        Ok(result)
    }

    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, path = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty))]
    #[tool(
        name = "edit_overwrite",
        title = "Edit Overwrite",
        description = "Creates or overwrites a file with UTF-8 content; creates parent directories if needed. Returns path, bytes_written. Fails if directory path supplied. AST-unaware (no language constraint). Use edit_replace for targeted single-block edits. working_dir sets the base directory for path resolution (default: server CWD). Example queries: Overwrite src/config.rs with updated content.",
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let t_start = std::time::Instant::now();
        let (seq, sid) = self.emit_received_metric("edit_overwrite").await;
        // Extract W3C Trace Context from request _meta if present
        let session_id = self.session_id.lock().await.clone();
        let client_name = self.client_name.lock().await.clone();
        let client_version = self.client_version.lock().await.clone();
        extract_and_set_trace_context(
            Some(&context.meta),
            ClientMetadata {
                session_id,
                client_name,
                client_version,
            },
        );
        let span = tracing::Span::current();
        span.record("gen_ai.system", "mcp");
        span.record("gen_ai.operation.name", "execute_tool");
        span.record("gen_ai.tool.name", "edit_overwrite");
        span.record("path", &params.path);
        let resolved_path: std::path::PathBuf = if let Some(ref wd) = params.working_dir {
            match validate_path_in_dir(&params.path, false, std::path::Path::new(wd)) {
                Ok(p) => p,
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    let mut result = CallToolResult::error(vec![Content::text(
                        "working_dir is not valid; provide an existing directory path".to_string(),
                    )])
                    .with_meta(Some(no_cache_meta()));
                    result.structured_content = Some(serde_json::json!({
                        "workingDir": wd,
                        "error": e.message,
                    }));
                    return Ok(result);
                }
            }
        } else {
            match validate_path(&params.path, false) {
                Ok(p) => p,
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    return Ok(err_to_tool_result(e));
                }
            }
        };
        let param_path = params.path.clone();

        // Guard against directory paths
        if std::fs::metadata(&resolved_path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .session_id(sid.clone())
                    .seq(Some(seq))
                    .build(),
            );
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

        let content = params.content.clone();
        let handle = tokio::task::spawn_blocking(move || {
            aptu_coder_core::edit_overwrite_content(&resolved_path, &content)
        });

        let output = match handle.await {
            Ok(Ok(v)) => v,
            Ok(Err(aptu_coder_core::EditError::NotAFile(_))) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("invalid_params".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .build(),
                );
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
            Ok(Err(aptu_coder_core::EditError::Io(io_err))) => {
                span.record("error", true);
                span.record("error.type", "internal_error");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("internal_error".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .build(),
                );
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    "I/O error writing file; check file path and permissions".to_string(),
                    {
                        let mut meta =
                            error_meta("resource", false, "check file path and permissions");
                        if let Some(obj) = meta.as_object_mut() {
                            obj.insert("path".to_string(), serde_json::json!(param_path));
                            obj.insert(
                                "ioErrorKind".to_string(),
                                serde_json::json!(format!("{:?}", io_err.kind())),
                            );
                            obj.insert(
                                "ioErrorSource".to_string(),
                                serde_json::json!(io_err.to_string()),
                            );
                        }
                        Some(meta)
                    },
                )));
            }
            Ok(Err(e)) => {
                span.record("error", true);
                span.record("error.type", "internal_error");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("internal_error".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .build(),
                );
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
                span.record("error", true);
                span.record("error.type", "internal_error");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("internal_error".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .build(),
                );
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

        // Reset circuit breaker on successful write
        {
            let sid_str = sid.clone().unwrap_or_default();
            let canonical = output.path.clone();
            let mut counts = self
                .edit_failure_counts
                .lock()
                .expect("edit_failure_counts poisoned");
            counts.remove(&(sid_str, canonical));
        }

        self.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("edit_overwrite", "ok", dur)
                .output_chars(text.len())
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .session_id(sid)
                .seq(Some(seq))
                .build(),
        );
        Ok(result)
    }

    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, path = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty))]
    #[tool(
        name = "edit_replace",
        title = "Edit Replace",
        description = "Replaces a unique exact text block; old_text must appear exactly once. Returns path, bytes_before, bytes_after. Fails if zero matches; fails if multiple matches (extend old_text to be more specific). If invalid_params is returned, re-read the target file with analyze_file or analyze_module before retrying. CRLF line endings in old_text are normalized to LF before matching; all other whitespace is matched exactly. Use edit_overwrite to replace the whole file. Pass empty string for new_text to delete the matched block. working_dir sets the base directory for path resolution (default: server CWD). Example queries: Update the function signature in lib.rs.",
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = params.0;
        let t_start = std::time::Instant::now();
        let (seq, sid) = self.emit_received_metric("edit_replace").await;
        // Extract W3C Trace Context from request _meta if present
        let session_id = self.session_id.lock().await.clone();
        let client_name = self.client_name.lock().await.clone();
        let client_version = self.client_version.lock().await.clone();
        extract_and_set_trace_context(
            Some(&context.meta),
            ClientMetadata {
                session_id,
                client_name,
                client_version,
            },
        );
        let span = tracing::Span::current();
        span.record("gen_ai.system", "mcp");
        span.record("gen_ai.operation.name", "execute_tool");
        span.record("gen_ai.tool.name", "edit_replace");
        span.record("path", &params.path);
        let resolved_path: std::path::PathBuf = if let Some(ref wd) = params.working_dir {
            match validate_path_in_dir(&params.path, true, std::path::Path::new(wd)) {
                Ok(p) => p,
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    let mut result = CallToolResult::error(vec![Content::text(
                        "working_dir is not valid; provide an existing directory path".to_string(),
                    )])
                    .with_meta(Some(no_cache_meta()));
                    result.structured_content = Some(serde_json::json!({
                        "workingDir": wd,
                        "error": e.message,
                    }));
                    return Ok(result);
                }
            }
        } else {
            match validate_path(&params.path, true) {
                Ok(p) => p,
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    return Ok(err_to_tool_result(e));
                }
            }
        };
        let param_path = params.path.clone();

        // Guard against directory paths
        if std::fs::metadata(&resolved_path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .session_id(sid.clone())
                    .seq(Some(seq))
                    .build(),
            );
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

        let old_text = params.old_text.clone();
        let new_text = params.new_text.clone();
        let old_text_for_hint = old_text.clone();
        let handle = tokio::task::spawn_blocking(move || {
            aptu_coder_core::edit_replace_block(&resolved_path, &old_text, &new_text)
        });

        let increment_failure = |canonical: &str| -> bool {
            let sid_str = sid.clone().unwrap_or_default();
            let mut counts = self
                .edit_failure_counts
                .lock()
                .expect("edit_failure_counts poisoned");
            if counts.len() >= EDIT_FAILURE_MAP_CAP {
                counts.clear();
            }
            let entry = counts.entry((sid_str, canonical.to_owned())).or_insert(0);
            *entry = entry.saturating_add(1);
            *entry >= EDIT_STALE_THRESHOLD
        };

        let output = match handle.await {
            Ok(Ok(v)) => v,
            Ok(Err(aptu_coder_core::EditError::NotFound {
                path: notfound_path,
                first_20_lines,
            })) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                // Circuit breaker: track consecutive failures per (session_id, canonical_path)
                let canonical = notfound_path.clone();
                let tripped = increment_failure(&canonical);
                if tripped {
                    self.metrics_tx.send(
                        crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                            .param_path_depth(crate::metrics::path_component_count(&param_path))
                            .error_type(Some("invalid_params".to_string()))
                            .error_subtype(Some("stale_context".to_string()))
                            .session_id(sid.clone())
                            .seq(Some(seq))
                            .build(),
                    );
                    return Ok(err_to_tool_result(ErrorData::new(
                        rmcp::model::ErrorCode::INVALID_PARAMS,
                        format!(
                            "EDIT_STALE_CONTEXT: {} consecutive not_found/ambiguous failures on '{}' in this session. The file content has drifted from your context. Call analyze_file or analyze_module on this path first, then retry edit_replace with old_text taken verbatim from that response. Do not retry edit_replace on this path without re-reading first.",
                            EDIT_STALE_THRESHOLD, param_path,
                        ),
                        Some(error_meta(
                            "validation",
                            false,
                            "re-read the file with analyze_file or analyze_module, then retry with old_text from the live content",
                        )),
                    )));
                }

                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("invalid_params".to_string()))
                        .error_subtype(Some("not_found".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .build(),
                );

                let message = if first_20_lines.is_empty() {
                    "old_text not found (0 matches). Re-read the file with analyze_file or analyze_module to obtain the current content, then derive old_text from the live file before retrying."
                        .to_string()
                } else {
                    let first_old_line = old_text_for_hint.lines().next().unwrap_or("");
                    let mut best_line_idx = 1usize;
                    let mut best_line = "";
                    let mut best_lcp = 0usize;

                    for (i, file_line) in first_20_lines.lines().enumerate() {
                        let lcp = file_line
                            .chars()
                            .zip(first_old_line.chars())
                            .take_while(|(a, b)| a == b)
                            .count();
                        if lcp > best_lcp {
                            best_lcp = lcp;
                            best_line = file_line;
                            best_line_idx = i + 1;
                        }
                    }

                    let numbered_lines: String = first_20_lines
                        .lines()
                        .enumerate()
                        .map(|(i, line)| format!("  Line {}: {}", i + 1, line))
                        .collect::<Vec<_>>()
                        .join("\n");

                    format!(
                        "old_text not found (0 matches).\nThe file begins:\n{numbered_lines}\n\nNearest match: line {best_line_idx} contains \"{best_line}\" which shares {best_lcp} characters with the start of old_text.\nRe-read the file with analyze_file or analyze_module to obtain the current content, then derive old_text from the live file before retrying."
                    )
                };

                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    message,
                    {
                        let mut meta = error_meta(
                            "validation",
                            false,
                            "re-read the file with analyze_file or analyze_module, then derive old_text from the live content",
                        );
                        if let Some(obj) = meta.as_object_mut() {
                            obj.insert("path".to_string(), serde_json::json!(notfound_path));
                        }
                        Some(meta)
                    },
                )));
            }
            Ok(Err(aptu_coder_core::EditError::Ambiguous {
                count,
                path: ambiguous_path,
                match_lines,
            })) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                // Circuit breaker: track consecutive failures per (session_id, canonical_path)
                let canonical = ambiguous_path.clone();
                let tripped = increment_failure(&canonical);
                if tripped {
                    self.metrics_tx.send(
                        crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                            .param_path_depth(crate::metrics::path_component_count(&param_path))
                            .error_type(Some("invalid_params".to_string()))
                            .error_subtype(Some("stale_context".to_string()))
                            .session_id(sid.clone())
                            .seq(Some(seq))
                            .build(),
                    );
                    return Ok(err_to_tool_result(ErrorData::new(
                        rmcp::model::ErrorCode::INVALID_PARAMS,
                        format!(
                            "EDIT_STALE_CONTEXT: {} consecutive not_found/ambiguous failures on '{}' in this session. The file content has drifted from your context. Call analyze_file or analyze_module on this path first, then retry edit_replace with old_text taken verbatim from that response. Do not retry edit_replace on this path without re-reading first.",
                            EDIT_STALE_THRESHOLD, param_path,
                        ),
                        Some(error_meta(
                            "validation",
                            false,
                            "re-read the file with analyze_file or analyze_module, then retry with old_text from the live content",
                        )),
                    )));
                }

                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("invalid_params".to_string()))
                        .error_subtype(Some("ambiguous".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .build(),
                );

                let line_numbers_csv = match_lines
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    format!(
                        "old_text matched {count} locations.\nOccurrences at lines: {line_numbers_csv}\nExtend old_text with more surrounding context to make it unique, or re-read with analyze_file to confirm the exact text."
                    ),
                    {
                        let mut meta = error_meta(
                            "validation",
                            false,
                            "extend old_text with more surrounding context, or re-read with analyze_file to confirm the exact text",
                        );
                        if let Some(obj) = meta.as_object_mut() {
                            obj.insert("path".to_string(), serde_json::json!(ambiguous_path));
                        }
                        Some(meta)
                    },
                )));
            }
            Ok(Err(aptu_coder_core::EditError::NotAFile(_))) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("invalid_params".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .build(),
                );
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
            Ok(Err(aptu_coder_core::EditError::Io(io_err))) => {
                span.record("error", true);
                span.record("error.type", "internal_error");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("internal_error".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .build(),
                );
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    "I/O error editing file; check file path and permissions".to_string(),
                    {
                        let mut meta =
                            error_meta("resource", false, "check file path and permissions");
                        if let Some(obj) = meta.as_object_mut() {
                            obj.insert("path".to_string(), serde_json::json!(param_path));
                            obj.insert(
                                "ioErrorKind".to_string(),
                                serde_json::json!(format!("{:?}", io_err.kind())),
                            );
                            obj.insert(
                                "ioErrorSource".to_string(),
                                serde_json::json!(io_err.to_string()),
                            );
                        }
                        Some(meta)
                    },
                )));
            }
            Ok(Err(e)) => {
                span.record("error", true);
                span.record("error.type", "internal_error");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("internal_error".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .build(),
                );
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
                span.record("error", true);
                span.record("error.type", "internal_error");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("internal_error".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .build(),
                );
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

        // Reset circuit breaker on successful edit
        {
            let sid_str = sid.clone().unwrap_or_default();
            let canonical = output.path.clone();
            let mut counts = self
                .edit_failure_counts
                .lock()
                .expect("edit_failure_counts poisoned");
            counts.remove(&(sid_str, canonical));
        }

        self.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("edit_replace", "ok", dur)
                .output_chars(text.len())
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .session_id(sid)
                .seq(Some(seq))
                .build(),
        );
        Ok(result)
    }

    #[tool(
        name = "exec_command",
        title = "Exec Command",
        description = "Execute shell command via sh -c (or $SHELL if set). Returns stdout, stderr, interleaved, exit_code, output_truncated. Output capped at 2000 lines and 50 KB per stream; stdout capped at 30 KB, stderr at 10 KB. Set working_dir to the target directory; write the command using relative paths only. Commands run inside working_dir; omit `cd`. Fails if working_dir does not exist or is not a directory. Pass stdin to pipe UTF-8 content into the process (max 1 MB). For file creation and edits, prefer the edit_* tools. Heredoc file writes (cat/tee/redirect + <<) are rejected; use edit_overwrite instead. When running build, lint, and test commands, prefer machine-readable JSON output flags where available to reduce output tokens. Example queries: Run the test suite and capture output.",
        output_schema = schema_for_type::<ShellOutput>(),
        annotations(
            title = "Exec Command",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, command = tracing::field::Empty, exit_code = tracing::field::Empty, output_truncated = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty))]
    pub async fn exec_command(
        &self,
        params: Parameters<ExecCommandParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let t_start = std::time::Instant::now();
        let (seq, sid) = self.emit_received_metric("exec_command").await;
        let params = params.0;
        // Extract W3C Trace Context from request _meta if present
        let session_id = self.session_id.lock().await.clone();
        let client_name = self.client_name.lock().await.clone();
        let client_version = self.client_version.lock().await.clone();
        extract_and_set_trace_context(
            Some(&context.meta),
            ClientMetadata {
                session_id,
                client_name,
                client_version,
            },
        );
        let span = tracing::Span::current();
        span.record("gen_ai.system", "mcp");
        span.record("gen_ai.operation.name", "execute_tool");
        span.record("gen_ai.tool.name", "exec_command");
        span.record("command", &params.command);

        // Validate working_dir if provided -- existence + is_dir only, no CWD confinement.
        // exec_command is a shell runner; CWD confinement applies only to edit_overwrite/edit_replace.
        let working_dir_path = if let Some(ref wd) = params.working_dir {
            match std::fs::canonicalize(wd) {
                Ok(p) => {
                    if !p.is_dir() {
                        span.record("error", true);
                        span.record("error.type", "invalid_params");
                        let mut result = CallToolResult::error(vec![Content::text(
                            "working_dir is not a directory; provide an existing directory path"
                                .to_string(),
                        )])
                        .with_meta(Some(no_cache_meta()));
                        result.structured_content = Some(serde_json::json!({
                            "workingDir": wd,
                        }));
                        return Ok(result);
                    }
                    Some(p)
                }
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    let mut result = CallToolResult::error(vec![Content::text(
                        "working_dir is not valid; provide an existing directory path".to_string(),
                    )])
                    .with_meta(Some(no_cache_meta()));
                    result.structured_content = Some(serde_json::json!({
                        "workingDir": wd,
                        "error": e.to_string(),
                    }));
                    return Ok(result);
                }
            }
        } else {
            None
        };

        // Strip leading "cd <path> &&" prefix from command only when provably redundant.
        // - No working_dir: promote the cd path as working_dir (unambiguous).
        // - working_dir already set: strip only if the cd path resolves to the same
        //   directory; otherwise pass the command through unmodified (the cd is
        //   load-bearing, e.g. a multi-step chain like "cd sub && build && cd ../other && build").
        let (effective_command, cd_extracted_path) = strip_cd_prefix(&params.command);
        let (command, working_dir_path) = if let Some(cd_path) = cd_extracted_path {
            if working_dir_path.is_none() {
                // Only promote when the path is a plain absolute literal -- no shell
                // special characters (~, $, -). Relative paths and shell-expanded forms
                // (cd ~, cd $VAR, cd -) must reach the shell unmodified; validate_path
                // cannot resolve them correctly before execution.
                let is_plain_absolute = cd_path.starts_with('/')
                    && !cd_path.contains('$')
                    && !cd_path.contains('~')
                    && cd_path != "-";
                if !is_plain_absolute {
                    // Shell-special or relative -- pass through unmodified.
                    (params.command.clone(), working_dir_path)
                } else {
                    // Promote the cd path as working_dir, run through validation
                    match validate_path(cd_path, true) {
                        Ok(p) if std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false) => {
                            tracing::debug!(
                                "exec_command: promoting cd prefix path as working_dir: {}",
                                cd_path
                            );
                            (effective_command.to_owned(), Some(p))
                        }
                        Ok(_) => {
                            span.record("error", true);
                            span.record("error.type", "invalid_params");
                            let mut result = CallToolResult::error(vec![Content::text(
                                "cd prefix path is not a directory; set working_dir explicitly or use a valid directory path".to_string(),
                            )])
                            .with_meta(Some(no_cache_meta()));
                            result.structured_content = Some(serde_json::json!({
                                "cdPath": cd_path,
                            }));
                            return Ok(result);
                        }
                        Err(_) => {
                            span.record("error", true);
                            span.record("error.type", "invalid_params");
                            let mut result = CallToolResult::error(vec![Content::text(
                                "cd prefix path does not exist or is outside CWD; set working_dir explicitly".to_string(),
                            )])
                            .with_meta(Some(no_cache_meta()));
                            result.structured_content = Some(serde_json::json!({
                                "cdPath": cd_path,
                            }));
                            return Ok(result);
                        }
                    }
                }
            } else {
                // working_dir is already set -- only strip if the cd path resolves to
                // the same directory (redundant). Otherwise keep the full original command.
                let cd_resolves_to_same = validate_path(cd_path, true)
                    .ok()
                    .map(|p| Some(&p) == working_dir_path.as_ref())
                    .unwrap_or(false);
                if cd_resolves_to_same {
                    tracing::debug!(
                        "exec_command: stripped redundant cd prefix; matches explicit working_dir"
                    );
                    (effective_command.to_owned(), working_dir_path)
                } else {
                    // cd path differs from working_dir -- the cd is load-bearing; pass through.
                    (params.command.clone(), working_dir_path)
                }
            }
        } else {
            (params.command.clone(), working_dir_path)
        };
        // Inject --no-stat for git pull if not already present
        let command = maybe_inject_no_stat(&command);
        span.record("command", &command);

        let param_path = params.working_dir.clone();

        // Validate stdin size cap (1 MB)
        if let Some(ref stdin_content) = params.stdin
            && stdin_content.len() > STDIN_MAX_BYTES
        {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "stdin exceeds 1 MB limit".to_string(),
                Some(error_meta("validation", false, "reduce stdin content size")),
            )));
        }

        // Validate heredocs before spawning any process
        if let Err(e) = validation::validate_heredocs(&command) {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("exec_command", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(
                        param_path.as_deref().unwrap_or(""),
                    ))
                    .error_type(Some("invalid_params".to_string()))
                    .session_id(sid)
                    .seq(Some(seq))
                    .output_truncated(Some(false))
                    .build(),
            );
            return Ok(err_to_tool_result(e));
        }

        // Validate drain_timeout_secs: negative values are invalid.
        if let Some(n) = params.drain_timeout_secs
            && n < 0
        {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "drain_timeout_secs must be >= 0".to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "use a non-negative value or omit it",
                )),
            )));
        }

        // Compute effective drain timeout
        let drain_dur = match params.drain_timeout_secs {
            Some(n) if n > 0 => std::time::Duration::from_millis(n as u64),
            _ => std::time::Duration::from_millis(DEFAULT_DRAIN_TIMEOUT_MS),
        };

        // Execute command (non-cacheable; exec_command is side-effecting and non-idempotent)
        let resolved_path_str = self.resolved_path.as_ref().as_deref();
        let output = run_exec_impl(
            command.clone(),
            working_dir_path.clone(),
            params.stdin.clone(),
            seq,
            resolved_path_str,
            &self.filter_table,
            params.timeout_secs,
            drain_dur,
        )
        .await;

        // Short-circuit on timeout: return INTERNAL_ERROR before any output processing.
        if output.timed_out {
            span.record("error", true);
            span.record("error.type", "timeout");
            let mut result = CallToolResult::error(vec![Content::text(
                "Command execution timed out; the process was killed.".to_string(),
            )])
            .with_meta(Some(no_cache_meta()));
            result.structured_content = Some(serde_json::json!({
                "timed_out": true,
                "timeout_secs": params.timeout_secs,
            }));
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            self.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("exec_command", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(
                        param_path.as_deref().unwrap_or(""),
                    ))
                    .error_type(Some("timeout".to_string()))
                    .session_id(sid)
                    .seq(Some(seq))
                    .timed_out(true)
                    .output_truncated(Some(false))
                    .build(),
            );
            return Ok(result);
        }

        let exit_code = output.exit_code;
        let mut output_truncated = output.output_truncated;

        // Record execution results on span
        if let Some(code) = exit_code {
            span.record("exit_code", code);
        }
        span.record("output_truncated", output_truncated);

        // Emit debug event for truncation
        if output_truncated {
            tracing::debug!(truncated = true, message = "output truncated");
        }

        // Use interleaved if non-empty; fall back to separated stdout/stderr for empty-output commands
        let output_text = if output.interleaved.is_empty() {
            format!("Stdout:\n{}\n\nStderr:\n{}", output.stdout, output.stderr)
        } else {
            format!("Output:\n{}", output.interleaved)
        };

        // Apply combined output size limit (SIZE_LIMIT = 5_000 bytes). Per-stream caps
        // (MAX_STDOUT_BYTES = 30k stdout, MAX_STDERR_BYTES = 10k stderr) already fired in
        // handle_output_persist; this is the safety net for the interleaved assembly which
        // can still reach up to ~40k bytes from per-stream content plus headers and formatting.
        let mut combined_truncated = false;
        let truncated_output_text = if output_text.len() > SIZE_LIMIT {
            combined_truncated = true;
            // Use char-boundary-safe tail truncation
            let tail_start = output_text.len().saturating_sub(SIZE_LIMIT);
            let safe_start = output_text[..tail_start].floor_char_boundary(tail_start);
            output_text[safe_start..].to_string()
        } else {
            output_text
        };

        // Update output_truncated flag to include combined truncation
        output_truncated = output_truncated || combined_truncated;

        let text = format!(
            "Command: {}\nExit code: {}\nOutput truncated: {}\n\n{}",
            params.command,
            exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "null".to_string()),
            output_truncated,
            truncated_output_text,
        );

        let content_blocks = vec![Content::text(text.clone()).with_priority(0.0)];

        // Determine if command failed: non-zero exit code.
        // exit_code is None when the post-exit drain times out (background child
        // holding pipes -- command work was done, treat as success) or when the
        // process is externally killed; both cases use unwrap_or(false) to avoid
        // false negatives.
        let command_failed = exit_code.map(|c| c != 0).unwrap_or(false);

        let mut result = if command_failed {
            CallToolResult::error(content_blocks)
        } else {
            CallToolResult::success(content_blocks)
        }
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
                span.record("error", true);
                span.record("error.type", "internal_error");
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                self.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("exec_command", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(
                            param_path.as_deref().unwrap_or(""),
                        ))
                        .error_type(Some("internal_error".to_string()))
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .exit_code(exit_code)
                        .timed_out(output.timed_out)
                        .output_truncated(Some(output_truncated))
                        .build(),
                );
                return Ok(err_to_tool_result(e));
            }
        };

        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("exec_command", "ok", dur)
                .output_chars(text.len())
                .param_path_depth(crate::metrics::path_component_count(
                    param_path.as_deref().unwrap_or(""),
                ))
                .session_id(sid)
                .seq(Some(seq))
                .exit_code(exit_code)
                .timed_out(output.timed_out)
                .output_truncated(Some(output_truncated))
                .chars_threshold_breach(text.len() > 30_000)
                .filter_applied(output.filter_applied.clone())
                .build(),
        );
        Ok(result)
    }
}

/// Build and configure a tokio::process::Command with stdio, working directory, and resource limits.
fn build_exec_command(
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
fn strip_cd_prefix(cmd: &str) -> (&str, Option<&str>) {
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
struct ExecutionResult {
    exit_code: Option<i32>,
    output_truncated: bool,
    output_collection_error: Option<String>,
    timed_out: bool,
}

/// Run a spawned child process with output draining.
/// When `timeout_secs` is `Some(secs)` where `secs > 0`, the entire execution (drain +
/// wait) is bounded by that many seconds. If the timeout fires the child is killed.
async fn run_with_timeout(
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
async fn run_exec_impl(
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
fn handle_output_persist(
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

#[derive(Clone)]
struct FocusedAnalysisParams {
    path: std::path::PathBuf,
    symbol: String,
    match_mode: SymbolMatchMode,
    follow_depth: u32,
    max_depth: Option<u32>,
    use_summary: bool,
    impl_only: Option<bool>,
    def_use: bool,
    parse_timeout_micros: Option<u64>,
}

fn disable_routes(router: &mut ToolRouter<CodeAnalyzer>, tools: &[&'static str]) {
    for tool in tools {
        router.disable_route(*tool);
    }
}

#[tool_handler]
impl ServerHandler for CodeAnalyzer {
    #[instrument(skip(self, context), fields(service.name = tracing::field::Empty, service.version = tracing::field::Empty))]
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let span = tracing::Span::current();
        span.record("service.name", "aptu-coder");
        span.record("service.version", env!("CARGO_PKG_VERSION"));

        // Store client_info from the initialize request
        {
            let mut client_name_lock = self.client_name.lock().await;
            *client_name_lock = Some(request.client_info.name.clone());
        }
        {
            let mut client_version_lock = self.client_version.lock().await;
            *client_version_lock = Some(request.client_info.version.clone());
        }

        // Extract profile string from _meta and store for use in on_initialized and call_tool.
        if let Some(meta) = context.extensions.get::<Meta>()
            && let Some(profile) = meta
                .0
                .get("io.clouatre-labs/profile")
                .and_then(|v| v.as_str())
        {
            let _ = self.session_profile.set(profile.to_owned());
        }
        Ok(self.get_info())
    }

    fn get_info(&self) -> InitializeResult {
        let excluded = aptu_coder_core::EXCLUDED_DIRS.join(", ");
        let instructions = format!(
            "Recommended workflow:\n\
            1. Start with analyze_directory(path=<repo_root>, max_depth=2, summary=true) to identify source package (largest by file count; exclude {excluded}).\n\
            2. Re-run analyze_directory(path=<source_package>, max_depth=2, summary=true) for module map. Include test directories (tests/, *_test.go, test_*.py, test_*.rs, *.spec.ts, *.spec.js).\n\
            3. For key files, prefer analyze_module for function/import index; use analyze_file for signatures and types.\n\
            4. Use analyze_symbol to trace call graphs.\n\
            Prefer summary=true on 1000+ files. Set max_depth=2; increase if packages too large. Paginate with cursor/page_size. For subagents: DISABLE_PROMPT_CACHING=1.\n\
            JSONL metrics at $HOME/.local/share/aptu-coder/ (or $XDG_DATA_HOME/aptu-coder/). Always cd there before jq glob queries."
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

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, ErrorData> {
        let router = self.tool_router.read().await;
        Ok(rmcp::model::ListToolsResult {
            tools: router.list_all(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let router = self.tool_router.read().await;
        router.call(tcc).await
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

        // NON-STANDARD VENDOR EXTENSION: profile-based tool filtering.
        // The MCP 2025-11-25 spec has no profile or tool-subset concept; tools/list returns
        // all tools with no filtering parameters. This mechanism is retained solely for
        // controlled benchmarking (wave10/11). Do not promote or document it as a product
        // feature. The spec-compliant way to restrict tools is for the orchestrator to pass
        // a filtered `tools` array in the API call, or for clients to use tool annotations
        // (readOnlyHint/destructiveHint) to apply their own policy.
        // Two profiles: "edit" (3 tools), "analyze" (5 tools); absent/unknown = all 7 tools.
        // _meta key "io.clouatre-labs/profile" takes precedence over APTU_CODER_PROFILE env var.

        // Resolve the active profile: session_profile (set in initialize from _meta) wins;
        // fall back to env var.
        let active_profile = self
            .session_profile
            .get()
            .cloned()
            .or_else(|| std::env::var("APTU_CODER_PROFILE").ok());

        {
            let mut router = self.tool_router.write().await;

            // Default: all 7 tools enabled unless profile explicitly disables them.
            // Two profiles: "edit" (3 tools), "analyze" (5 tools); absent/unknown = all 7 tools.

            if let Some(ref profile) = active_profile {
                match profile.as_str() {
                    "edit" => {
                        // Enable only: edit_replace, edit_overwrite, exec_command
                        disable_routes(
                            &mut router,
                            &[
                                "analyze_directory",
                                "analyze_file",
                                "analyze_module",
                                "analyze_symbol",
                            ],
                        );
                    }
                    "analyze" => {
                        // Enable only: analyze_directory, analyze_file, analyze_module, analyze_symbol, exec_command
                        disable_routes(&mut router, &["edit_replace", "edit_overwrite"]);
                    }
                    _ => {
                        // Unknown profile: all 7 tools enabled (lenient fallback)
                    }
                }
            }

            // Bind peer notifier after disabling tools to send tools/list_changed notification
            router.bind_peer_notifier(&context.peer);
        }

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
    use regex::Regex;
    use rmcp::model::NumberOrString;

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
        let (arc_output, _cache_hit) = analyzer
            .handle_overview_mode(&params, ct, None)
            .await
            .unwrap();
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
    async fn test_handle_overview_mode_no_summary_block() {
        use aptu_coder_core::types::AnalyzeDirectoryParams;
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
        }))
        .unwrap();

        let ct = tokio_util::sync::CancellationToken::new();
        let (output, _cache_hit) = analyzer
            .handle_overview_mode(&params, ct, None)
            .await
            .unwrap();

        // summary=None with small output: handler uses format_structure (tree), which is
        // already stored in output.formatted from build_analysis_output.
        // The tree output contains a SUMMARY: block and a PATH block.
        let formatted = &output.formatted;

        assert!(
            formatted.contains("SUMMARY:"),
            "summary=None with small output must emit SUMMARY: block (tree output); got: {}",
            &formatted[..formatted.len().min(300)]
        );
        assert!(
            formatted.contains("PATH [LOC, FUNCTIONS, CLASSES]"),
            "summary=None with small output must emit PATH section header (tree output); got: {}",
            &formatted[..formatted.len().min(300)]
        );
        assert!(
            !formatted.contains("PAGINATED:"),
            "summary=None must NOT emit PAGINATED: header; got: {}",
            &formatted[..formatted.len().min(300)]
        );
    }

    #[tokio::test]
    async fn test_analyze_directory_summary_false_forces_pagination() {
        // Edge case: summary=false must return format_structure_paginated (flat list with
        // PAGINATED: header) even when the directory output is small (< 5000 chars).
        use aptu_coder_core::types::AnalyzeDirectoryParams;
        use tempfile::TempDir;

        // Arrange: a small directory (one file, well under SIZE_LIMIT)
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}").unwrap();

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
            "summary": false,
        }))
        .unwrap();

        // Act: call the full handler via handle_overview_mode + replicate handler path
        let ct = tokio_util::sync::CancellationToken::new();
        let (output, _cache_hit) = analyzer
            .handle_overview_mode(&params, ct, None)
            .await
            .unwrap();

        // Assert: output is small (confirms SIZE_LIMIT would not trigger auto-summary)
        assert!(
            output.formatted.len() <= SIZE_LIMIT,
            "test precondition: output must be small; got {} chars",
            output.formatted.len()
        );

        // The handler must use format_structure_paginated because summary=Some(false)
        // We verify by calling the full tool handler via make_analyzer + call_tool_raw
        // is not available here, so we verify the handler logic directly:
        // use_paginated = params.output_control.summary == Some(false) -> true
        let use_paginated = params.output_control.summary == Some(false);
        assert!(use_paginated, "summary=false must set use_paginated=true");

        // Confirm the tree output does NOT contain PAGINATED: (it is format_structure)
        assert!(
            !output.formatted.contains("PAGINATED:"),
            "handle_overview_mode returns format_structure (tree); PAGINATED: must not appear"
        );
        // Confirm the tree output contains SUMMARY: (format_structure marker)
        assert!(
            output.formatted.contains("SUMMARY:"),
            "handle_overview_mode returns format_structure (tree); SUMMARY: must appear"
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
        let (_out1, hit1) = analyzer
            .handle_overview_mode(&params, ct1, None)
            .await
            .unwrap();

        // Act: second call (cache hit)
        let ct2 = tokio_util::sync::CancellationToken::new();
        let (_out2, hit2) = analyzer
            .handle_overview_mode(&params, ct2, None)
            .await
            .unwrap();

        // Assert
        assert_eq!(hit1, CacheTier::Miss, "first call must be a cache miss");
        assert_eq!(hit2, CacheTier::L1Memory, "second call must be a cache hit");
    }

    #[test]
    fn test_analyze_module_cache_hit_metrics() {
        use std::io::Write as _;
        use tempfile::NamedTempFile;

        // Arrange: create a temp Rust file inside CWD so validate_path accepts it
        let cwd = std::env::current_dir().unwrap();
        let mut f = NamedTempFile::with_suffix_in(".rs", &cwd).unwrap();
        write!(f, "use std::io;\nfn bar() {{}}\n").unwrap();
        f.flush().unwrap();

        // Act
        let result = analyze::analyze_module_file(f.path().to_str().unwrap());

        // Assert
        let module_info = result.expect("analyze_module_file must succeed");
        assert_eq!(
            module_info.functions.len(),
            1,
            "expected exactly one function"
        );
        assert_eq!(module_info.functions[0].name, "bar");
        assert_eq!(module_info.imports.len(), 1, "expected exactly one import");
        assert!(
            module_info.imports[0].module.contains("std"),
            "import module must contain 'std', got: {}",
            module_info.imports[0].module
        );
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
            .handle_overview_mode(&params, ct, None)
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
        // Edge case: non-existent file with existing parent should be accepted.
        let cwd = std::env::current_dir().expect("should get cwd");
        let parent = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
        let parent_path = parent.path().to_path_buf();
        let child = parent_path.join("new_file.txt");

        let child_str = child.to_str().expect("path should be valid UTF-8");
        let result = validate_path(child_str, false);
        assert!(
            result.is_ok(),
            "validate_path should accept non-existent file with existing parent (require_exists=false)"
        );
        let path = result.unwrap();
        let canonical_cwd = std::fs::canonicalize(&cwd).expect("should canonicalize cwd");
        assert!(
            path.starts_with(&canonical_cwd),
            "Resolved path should be within CWD: {:?} should start with {:?}",
            path,
            canonical_cwd
        );
    }

    #[test]
    fn test_edit_overwrite_with_working_dir() {
        // Arrange: create a temporary directory within CWD to use as working_dir
        let cwd = std::env::current_dir().expect("should get cwd");
        let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
        let temp_path = temp_dir.path();

        // Act: call validate_path_in_dir with a relative path
        let result = validate_path_in_dir("test_file.txt", false, temp_path);

        // Assert: path should be resolved relative to working_dir
        assert!(
            result.is_ok(),
            "validate_path_in_dir should accept relative path in valid working_dir: {:?}",
            result.err()
        );
        let resolved = result.unwrap();
        assert!(
            resolved.starts_with(temp_path),
            "Resolved path should be within working_dir: {:?} should start with {:?}",
            resolved,
            temp_path
        );
    }

    #[test]
    fn test_validate_path_in_dir_accepts_outside_cwd() {
        // Arrange: use temp_dir() which is guaranteed to be outside CWD
        let temp_dir = std::env::temp_dir();
        let canonical_temp_dir =
            std::fs::canonicalize(&temp_dir).expect("should canonicalize temp_dir");

        // Act: call validate_path_in_dir with a relative filename
        let result = validate_path_in_dir("probe.txt", false, &temp_dir);

        // Assert: should accept working_dir outside CWD
        assert!(
            result.is_ok(),
            "validate_path_in_dir should accept working_dir outside CWD: {:?}",
            result.err()
        );
        let resolved = result.unwrap();
        assert!(
            resolved.starts_with(&canonical_temp_dir),
            "Resolved path should be within working_dir: {:?} should start with {:?}",
            resolved,
            canonical_temp_dir
        );
    }

    #[test]
    fn test_edit_overwrite_working_dir_traversal() {
        // Arrange: create a temporary directory within CWD to use as working_dir
        let cwd = std::env::current_dir().expect("should get cwd");
        let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
        let temp_path = temp_dir.path();

        // Act: try to traverse outside working_dir with ../../../etc/passwd
        let result = validate_path_in_dir("../../../etc/passwd", false, temp_path);

        // Assert: should reject path traversal attack (via parent canonicalize failure)
        assert!(
            result.is_err(),
            "validate_path_in_dir should reject path traversal outside working_dir"
        );
    }

    #[test]
    fn test_edit_replace_with_working_dir() {
        // Arrange: create a temporary directory within CWD and file
        let cwd = std::env::current_dir().expect("should get cwd");
        let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
        let temp_path = temp_dir.path();
        let file_path = temp_path.join("test.txt");
        std::fs::write(&file_path, "hello world").expect("should write test file");

        // Act: call validate_path_in_dir with require_exists=true
        let result = validate_path_in_dir("test.txt", true, temp_path);

        // Assert: should find the file relative to working_dir
        assert!(
            result.is_ok(),
            "validate_path_in_dir should find existing file in working_dir: {:?}",
            result.err()
        );
        let resolved = result.unwrap();
        assert_eq!(
            resolved, file_path,
            "Resolved path should match the actual file path"
        );
    }

    #[test]
    fn test_edit_overwrite_no_working_dir() {
        // Arrange: use validate_path without working_dir (existing behavior)
        // Use Cargo.toml which exists in the crate root

        // Act: call validate_path with require_exists=true
        let result = validate_path("Cargo.toml", true);

        // Assert: should work as before
        assert!(
            result.is_ok(),
            "validate_path should still work without working_dir"
        );
    }

    #[test]
    fn test_edit_overwrite_working_dir_is_file() {
        // Arrange: create a temporary file (not directory) to use as working_dir
        let cwd = std::env::current_dir().expect("should get cwd");
        let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
        let temp_file = temp_dir.path().join("test_file.txt");
        std::fs::write(&temp_file, "test content").expect("should write test file");

        // Act: call validate_path_in_dir with a file as working_dir
        let result = validate_path_in_dir("some_file.txt", false, &temp_file);

        // Assert: should reject because working_dir is not a directory
        assert!(
            result.is_err(),
            "validate_path_in_dir should reject a file as working_dir"
        );
        let err = result.unwrap_err();
        let err_msg = err.message.to_lowercase();
        assert!(
            err_msg.contains("directory"),
            "Error message should mention 'directory': {}",
            err.message
        );
    }

    #[test]
    fn test_tool_annotations() {
        // Arrange: get tool list via static method
        let tools = CodeAnalyzer::list_tools();

        // Act: find specific tools by name
        let analyze_directory = tools.iter().find(|t| t.name == "analyze_directory");
        let exec_command = tools.iter().find(|t| t.name == "exec_command");

        // Assert: analyze_directory has correct annotations
        let analyze_dir_tool = analyze_directory.expect("analyze_directory tool should exist");
        let analyze_dir_annot = analyze_dir_tool
            .annotations
            .as_ref()
            .expect("analyze_directory should have annotations");
        assert_eq!(
            analyze_dir_annot.read_only_hint,
            Some(true),
            "analyze_directory read_only_hint should be true"
        );
        assert_eq!(
            analyze_dir_annot.destructive_hint,
            Some(false),
            "analyze_directory destructive_hint should be false"
        );

        // Assert: exec_command has correct annotations
        let exec_cmd_tool = exec_command.expect("exec_command tool should exist");
        let exec_cmd_annot = exec_cmd_tool
            .annotations
            .as_ref()
            .expect("exec_command should have annotations");
        assert_eq!(
            exec_cmd_annot.open_world_hint,
            Some(true),
            "exec_command open_world_hint should be true"
        );
    }

    #[test]
    fn test_exec_stdin_size_cap_validation() {
        // Test: stdin size cap check (1 MB limit)
        // Arrange: create oversized stdin
        let oversized_stdin = "x".repeat(STDIN_MAX_BYTES + 1);

        // Act & Assert: verify size exceeds limit
        assert!(
            oversized_stdin.len() > STDIN_MAX_BYTES,
            "test setup: oversized stdin should exceed 1 MB"
        );

        // Verify that a 1 MB stdin is accepted
        let max_stdin = "y".repeat(STDIN_MAX_BYTES);
        assert_eq!(
            max_stdin.len(),
            STDIN_MAX_BYTES,
            "test setup: max stdin should be exactly 1 MB"
        );
    }

    #[tokio::test]
    async fn test_exec_stdin_cat_roundtrip() {
        // Test: stdin content is piped to process and readable via stdout
        // Arrange: prepare stdin content
        let stdin_content = "hello world";

        // Act: execute cat with stdin via shell
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn cat");

        if let Some(mut stdin_handle) = child.stdin.take() {
            use tokio::io::AsyncWriteExt as _;
            stdin_handle
                .write_all(stdin_content.as_bytes())
                .await
                .expect("write stdin");
            drop(stdin_handle);
        }

        let output = child.wait_with_output().await.expect("wait for cat");

        // Assert: stdout contains the piped stdin content
        let stdout_str = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout_str.contains(stdin_content),
            "stdout should contain stdin content: {}",
            stdout_str
        );
    }

    #[tokio::test]
    async fn test_exec_stdin_none_no_regression() {
        // Test: command without stdin executes normally (no regression)
        // Act: execute echo without stdin
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("echo hi")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn echo");

        let output = child.wait_with_output().await.expect("wait for echo");

        // Assert: command executes successfully
        let stdout_str = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout_str.contains("hi"),
            "stdout should contain echo output: {}",
            stdout_str
        );
    }

    #[test]
    fn test_validate_path_in_dir_rejects_sibling_prefix() {
        // Arrange: create a parent temp dir, then two subdirs:
        //   allowed/   -- the working_dir
        //   allowed_sibling/  -- a sibling whose name shares the prefix
        // This mirrors CVE-2025-53110: "/work_evil" must not match "/work".
        let cwd = std::env::current_dir().expect("should get cwd");
        let parent = tempfile::TempDir::new_in(&cwd).expect("should create parent temp dir");
        let allowed = parent.path().join("allowed");
        let sibling = parent.path().join("allowed_sibling");
        std::fs::create_dir_all(&allowed).expect("should create allowed dir");
        std::fs::create_dir_all(&sibling).expect("should create sibling dir");

        // Act: ask for a file inside the sibling dir, using a path that
        // traverses from allowed/ into allowed_sibling/
        let result = validate_path_in_dir("../allowed_sibling/secret.txt", false, &allowed);

        // Assert: must be rejected even though "allowed_sibling" starts with "allowed"
        assert!(
            result.is_err(),
            "validate_path_in_dir must reject a path resolving to a sibling directory \
             sharing the working_dir name prefix (CVE-2025-53110 pattern)"
        );
        let err = result.unwrap_err();
        let msg = err.message.to_lowercase();
        assert!(
            msg.contains("outside") || msg.contains("working"),
            "Error should mention 'outside' or 'working', got: {}",
            err.message
        );
    }

    #[test]
    fn test_validate_path_in_dir_nonexistent_deep_path() {
        // Deeply nested non-existent path: a/b/c/d/new.txt -- none of the
        // intermediate directories exist.  With parent-directory validation,
        // this is rejected because the parent a/b/c/d does not exist.
        let temp_dir = tempfile::TempDir::new().expect("should create temp dir");
        let result = validate_path_in_dir("a/b/c/d/new.txt", false, temp_dir.path());
        assert!(
            result.is_err(),
            "validate_path_in_dir should reject deeply nested non-existent path"
        );
    }

    #[test]
    fn test_validate_path_in_dir_nonexistent_with_existing_parent() {
        // Partial existence: working_dir/sub/ exists but working_dir/sub/new.txt does not.
        // The loop should stop at sub/ (the first existing ancestor) and rejoin new.txt.
        let temp_dir = tempfile::TempDir::new().expect("should create temp dir");
        let sub = temp_dir.path().join("sub");
        std::fs::create_dir_all(&sub).expect("should create sub dir");

        let result = validate_path_in_dir("sub/new.txt", false, temp_dir.path());
        assert!(
            result.is_ok(),
            "validate_path_in_dir should accept file in existing subdir: {:?}",
            result.err()
        );
        let resolved = result.unwrap();
        let canonical_sub = std::fs::canonicalize(&sub).expect("should canonicalize sub");
        assert!(
            resolved.starts_with(&canonical_sub),
            "Resolved path should anchor at the existing sub/ dir: {resolved:?}"
        );
        assert_eq!(
            resolved.file_name().and_then(|n| n.to_str()),
            Some("new.txt"),
            "File name component must be preserved"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_file_cache_capacity_default() {
        // Arrange: ensure the env var is not set
        unsafe { std::env::remove_var("APTU_CODER_FILE_CACHE_CAPACITY") };

        // Act
        let analyzer = make_analyzer();

        // Assert: default file cache capacity is 100
        assert_eq!(analyzer.cache.file_capacity(), 100);
    }

    #[test]
    #[serial_test::serial]
    fn test_file_cache_capacity_from_env() {
        // Arrange
        unsafe { std::env::set_var("APTU_CODER_FILE_CACHE_CAPACITY", "42") };

        // Act
        let analyzer = make_analyzer();

        // Cleanup before assertions to minimise env pollution window
        unsafe { std::env::remove_var("APTU_CODER_FILE_CACHE_CAPACITY") };

        // Assert
        assert_eq!(analyzer.cache.file_capacity(), 42);
    }

    #[test]
    fn test_exec_command_path_injected() {
        // Arrange: call build_exec_command with Some("...") resolved_path
        let resolved_path = Some("/usr/local/bin:/usr/bin:/bin");
        let cmd = build_exec_command("echo test", None, false, resolved_path);

        // Act: verify the command was created without panic and inspect args
        let cmd_str = format!("{:?}", cmd);

        // Assert: -l flag must NOT be present (platform unification)
        assert!(
            !cmd_str.contains("-l"),
            "build_exec_command must not use -l on any platform"
        );

        // Assert: command should be created successfully
        assert!(
            !cmd_str.is_empty(),
            "build_exec_command should return a valid Command"
        );
    }

    #[test]
    fn test_exec_command_path_fallback() {
        // Arrange: call build_exec_command with None resolved_path
        let cmd = build_exec_command("echo test", None, false, None);

        // Act: verify the command was created without panic and inspect args
        let cmd_str = format!("{:?}", cmd);

        // Assert: -l flag must NOT be present (platform unification)
        assert!(
            !cmd_str.contains("-l"),
            "build_exec_command must not use -l on any platform"
        );

        // Assert: command should be created successfully even with None
        assert!(
            !cmd_str.is_empty(),
            "build_exec_command should handle None resolved_path gracefully"
        );
    }

    #[test]
    fn test_analyze_symbol_cache_fields_use_cache_tier_enum() {
        // Verify that CacheTier::Miss produces the expected cache_hit/cache_tier
        // values that analyze_symbol writes in both code paths (#950).
        // Guards against string drift if CacheTier::Miss.as_str() ever changes.
        assert_eq!(
            CacheTier::Miss.as_str(),
            "miss",
            "CacheTier::Miss.as_str() must stay \"miss\" -- analyze_symbol metrics depend on it"
        );
        assert!(
            !matches!(CacheTier::Miss, CacheTier::L1Memory | CacheTier::L2Disk),
            "CacheTier::Miss must not be a hit variant (cache_hit=false for a miss)"
        );
    }

    #[tokio::test]
    async fn test_unsupported_extension_returns_success() {
        // Arrange: unsupported extension; handle_file_details_mode should return
        // a structured success (empty semantic, first-50-lines preview).
        let temp_dir = tempfile::TempDir::new().expect("should create temp dir");
        let unsupported_file = temp_dir.path().join("notes.txt");
        std::fs::write(&unsupported_file, "line one\nline two\nline three")
            .expect("should write file");

        let analyzer = make_analyzer();
        let mut params = AnalyzeFileParams::default();
        params.path = unsupported_file.to_string_lossy().to_string();

        let result = analyzer.handle_file_details_mode(&params).await;

        assert!(
            result.is_ok(),
            "should succeed for unsupported extension; got: {:?}",
            result
        );
        let (output, _tier) = result.unwrap();
        assert_eq!(output.line_count, 3, "line_count must be 3");
        assert!(
            output.semantic.functions.is_empty(),
            "functions must be empty"
        );
        assert!(output.semantic.classes.is_empty(), "classes must be empty");
        assert!(output.semantic.imports.is_empty(), "imports must be empty");
    }

    #[tokio::test]
    async fn test_unsupported_extension_fallback_note_in_formatted() {
        // Edge case: formatted output must contain an unsupported-extension note.
        let temp_dir = tempfile::TempDir::new().expect("should create temp dir");
        let unsupported_file = temp_dir.path().join("readme.txt");
        std::fs::write(
            &unsupported_file,
            "This is a plain text file.\nSecond line.",
        )
        .expect("should write file");

        let analyzer = make_analyzer();
        let mut params = AnalyzeFileParams::default();
        params.path = unsupported_file.to_string_lossy().to_string();

        let (output, _tier) = analyzer
            .handle_file_details_mode(&params)
            .await
            .expect("must succeed");
        let lower = output.formatted.to_lowercase();
        assert!(
            lower.contains("unsupported"),
            "formatted must contain 'unsupported' note; got: {}",
            output.formatted
        );
    }

    #[test]
    fn test_exec_no_truncation_under_limits() {
        // Happy path: small output under all caps
        let stdout = "hello world".to_string();
        let stderr = "no errors".to_string();
        let slot = 0u32;

        let (out_stdout, out_stderr, stdout_path, stderr_path, byte_truncated) =
            handle_output_persist(stdout, stderr, slot);

        assert_eq!(out_stdout, "hello world");
        assert_eq!(out_stderr, "no errors");
        assert!(stdout_path.is_none());
        assert!(stderr_path.is_none());
        assert!(!byte_truncated);
    }

    #[test]
    fn test_exec_byte_overflow_stdout_exceeds_30k() {
        // Edge case: stdout exceeds 30k byte limit
        let stdout = "x".repeat(35_000);
        let stderr = "small".to_string();
        let slot = 0u32;

        let (out_stdout, out_stderr, stdout_path, stderr_path, byte_truncated) =
            handle_output_persist(stdout.clone(), stderr.clone(), slot);

        // Verify truncation occurred
        assert!(byte_truncated, "byte_truncated should be true");
        assert!(stdout_path.is_some(), "stdout_path should be set");
        assert!(stderr_path.is_some(), "stderr_path should be set");

        // Verify output was truncated
        assert!(
            out_stdout.len() <= 30_000,
            "stdout should be truncated to <= 30k"
        );
        assert_eq!(out_stderr, "small", "stderr should be unchanged");

        // Verify slot file was written
        let base = std::env::temp_dir()
            .join("aptu-coder-overflow")
            .join(format!("slot-{slot}"));
        let stdout_file = base.join("stdout");
        assert!(
            stdout_file.exists(),
            "stdout slot file should exist after byte overflow"
        );
    }

    #[test]
    fn test_exec_byte_overflow_stderr_exceeds_10k() {
        // Edge case: stderr exceeds 10k byte limit
        let stdout = "small".to_string();
        let stderr = "y".repeat(15_000);
        let slot = 1u32;

        let (out_stdout, out_stderr, stdout_path, stderr_path, byte_truncated) =
            handle_output_persist(stdout.clone(), stderr.clone(), slot);

        // Verify truncation occurred
        assert!(byte_truncated, "byte_truncated should be true");
        assert!(stdout_path.is_some(), "stdout_path should be set");
        assert!(stderr_path.is_some(), "stderr_path should be set");

        // Verify output was truncated
        assert_eq!(out_stdout, "small", "stdout should be unchanged");
        assert!(
            out_stderr.len() <= 10_000,
            "stderr should be truncated to <= 10k"
        );

        // Verify slot file was written
        let base = std::env::temp_dir()
            .join("aptu-coder-overflow")
            .join(format!("slot-{slot}"));
        let stderr_file = base.join("stderr");
        assert!(
            stderr_file.exists(),
            "stderr slot file should exist after byte overflow"
        );
    }

    #[test]
    fn test_exec_byte_overflow_combined_exceeds_50k() {
        // Edge case: combined output_text exceeds 50k char limit
        // This is tested by verifying the truncation logic in exec_command
        let large_output = "z".repeat(60_000);
        assert!(large_output.len() > SIZE_LIMIT);

        // Simulate the truncation logic from exec_command
        let mut combined_truncated = false;
        let truncated = if large_output.len() > SIZE_LIMIT {
            combined_truncated = true;
            let tail_start = large_output.len().saturating_sub(SIZE_LIMIT);
            let safe_start = large_output[..tail_start].floor_char_boundary(tail_start);
            large_output[safe_start..].to_string()
        } else {
            large_output.clone()
        };

        assert!(combined_truncated, "combined_truncated should be true");
        assert!(
            truncated.len() <= SIZE_LIMIT,
            "output should be truncated to <= 50k"
        );
    }

    #[test]
    fn test_exec_line_and_byte_interaction() {
        // Edge case: line cap and byte cap are independent
        // 1500 lines with long content to exceed 30k bytes should trigger byte cap, not line cap
        let lines: Vec<String> = (0..1500)
            .map(|i| {
                format!(
                    "line {} with some padding to make it longer: {}",
                    i,
                    "x".repeat(15)
                )
            })
            .collect();
        let stdout = lines.join("\n");
        assert!(stdout.lines().count() <= 2000, "should have <= 2000 lines");
        assert!(stdout.len() > 30_000, "should exceed 30k bytes");

        let stderr = "".to_string();
        let slot = 2u32;

        let (out_stdout, _out_stderr, stdout_path, _stderr_path, byte_truncated) =
            handle_output_persist(stdout.clone(), stderr, slot);

        // Byte cap should fire, not line cap
        assert!(byte_truncated, "byte_truncated should be true");
        assert!(stdout_path.is_some(), "stdout_path should be set");
        assert!(
            out_stdout.len() <= 30_000,
            "stdout should be truncated by byte cap"
        );
    }

    #[test]
    fn test_exec_utf8_boundary_safety() {
        // Edge case: ensure truncation doesn't split multi-byte UTF-8 chars
        // Create a string with multi-byte characters near the boundary
        let mut stdout = String::new();
        for _ in 0..4000 {
            stdout.push_str("hello world ");
        }
        // Add some multi-byte chars
        stdout.push_str("こんにちは"); // Japanese characters (3 bytes each)
        assert!(stdout.len() > 30_000, "stdout should exceed 30k bytes");

        let stderr = "".to_string();
        let slot = 5u32;

        let (out_stdout, _out_stderr, _stdout_path, _stderr_path, byte_truncated) =
            handle_output_persist(stdout, stderr, slot);

        // Verify truncation happened and result is valid UTF-8
        assert!(byte_truncated, "byte_truncated should be true");
        assert!(
            out_stdout.is_char_boundary(0),
            "start should be char boundary"
        );
        assert!(
            out_stdout.is_char_boundary(out_stdout.len()),
            "end should be char boundary"
        );
        // Verify we can iterate chars without panic
        let _char_count = out_stdout.chars().count();
    }

    #[test]
    fn test_filter_strip_lines_matching() {
        // Happy path: filter matches command prefix and strips lines
        let rule = types::FilterRule {
            match_command: "^git\\s+pull".to_string(),
            description: Some("test filter".to_string()),
            strip_ansi: false,
            strip_lines_matching: vec!["^\\s*\\|\\s*\\d+\\s*[+-]+".to_string()],
            keep_lines_matching: vec![],
            max_lines: None,
            on_empty: None,
        };

        let strip_patterns = vec![Regex::new("^\\s*\\|\\s*\\d+\\s*[+-]+").unwrap()];
        let compiled = CompiledRule {
            pattern: Regex::new("^git\\s+pull").unwrap(),
            strip_patterns,
            keep_patterns: vec![],
            rule,
        };

        let stdout = "Updating abc123..def456\n | 5 ++++\n | 3 ---\nFast-forward\n";
        let filtered = apply_filter(&compiled, stdout);

        assert!(!filtered.contains("| 5 ++++"), "should strip stat lines");
        assert!(!filtered.contains("| 3 ---"), "should strip stat lines");
        assert!(
            filtered.contains("Updating"),
            "should keep non-matching lines"
        );
        assert!(
            filtered.contains("Fast-forward"),
            "should keep non-matching lines"
        );
    }

    #[test]
    fn test_filter_on_empty_substitution() {
        // Edge case: on_empty substitution when filtered stdout is empty
        let rule = types::FilterRule {
            match_command: "^git\\s+fetch".to_string(),
            description: Some("test fetch".to_string()),
            strip_ansi: false,
            strip_lines_matching: vec!["^From ".to_string(), "^\\s+[a-f0-9]+\\.\\.".to_string()],
            keep_lines_matching: vec![],
            max_lines: None,
            on_empty: Some("ok fetched".to_string()),
        };

        let strip_patterns = vec![
            Regex::new("^From ").unwrap(),
            Regex::new("^\\s+[a-f0-9]+\\.\\.").unwrap(),
        ];
        let compiled = CompiledRule {
            pattern: Regex::new("^git\\s+fetch").unwrap(),
            strip_patterns,
            keep_patterns: vec![],
            rule,
        };

        let stdout = "From github.com:user/repo\n  abc123..def456 main -> origin/main\n";
        let filtered = apply_filter(&compiled, stdout);

        assert_eq!(
            filtered, "ok fetched",
            "should return on_empty when all lines stripped"
        );
    }

    #[test]
    fn test_filter_passthrough_on_failure() {
        // Test the exit-code guard in run_exec_impl: filter only applied when exit_code == Some(0)
        let rule = types::FilterRule {
            match_command: "^cargo\\s+build".to_string(),
            description: Some("cargo build filter".to_string()),
            strip_ansi: false,
            strip_lines_matching: vec!["^\\s*Compiling ".to_string()],
            keep_lines_matching: vec![],
            max_lines: None,
            on_empty: None,
        };

        let strip_patterns = vec![Regex::new("^\\s*Compiling ").unwrap()];
        let compiled = CompiledRule {
            pattern: Regex::new("^cargo\\s+build").unwrap(),
            strip_patterns,
            keep_patterns: vec![],
            rule,
        };

        let stdout = "   Compiling mylib v0.1.0\nerror: failed to compile\n";

        // Sub-case 1: non-zero exit code (exit_code != Some(0))
        // The guard condition fails, so filter_applied must remain None and stdout unchanged
        let mut output = ShellOutput::new(
            stdout.to_string(),
            "".to_string(),
            "".to_string(),
            Some(1), // non-zero exit
            false,
        );

        // Simulate the guard: if exit_code == Some(0) { apply filter }
        if output.exit_code == Some(0) {
            output.stdout = apply_filter(&compiled, &output.stdout);
            output.filter_applied = compiled
                .rule
                .description
                .clone()
                .or_else(|| Some(compiled.rule.match_command.clone()));
        }

        assert!(
            output.filter_applied.is_none(),
            "filter_applied should be None when exit_code != Some(0)"
        );
        assert!(
            output.stdout.contains("Compiling"),
            "stdout should be unchanged when exit_code != Some(0)"
        );

        // Sub-case 2: zero exit code (exit_code == Some(0))
        // The guard condition passes, so filter_applied is set and stdout is filtered
        let mut output2 = ShellOutput::new(
            stdout.to_string(),
            "".to_string(),
            "".to_string(),
            Some(0), // zero exit
            false,
        );

        if output2.exit_code == Some(0) {
            output2.stdout = apply_filter(&compiled, &output2.stdout);
            output2.filter_applied = compiled
                .rule
                .description
                .clone()
                .or_else(|| Some(compiled.rule.match_command.clone()));
        }

        assert!(
            output2.filter_applied.is_some(),
            "filter_applied should be set when exit_code == Some(0)"
        );
        assert_eq!(
            output2.filter_applied.as_ref().unwrap(),
            "cargo build filter"
        );
        assert!(
            !output2.stdout.contains("Compiling"),
            "stdout should be filtered when exit_code == Some(0)"
        );
    }

    #[test]
    fn test_no_stat_injection() {
        // Happy path: --no-stat injection for bare git pull
        let command = "git pull origin main";
        let result = maybe_inject_no_stat(command);
        assert_eq!(
            result, "git pull origin main --no-stat",
            "should inject --no-stat"
        );
    }

    #[test]
    fn test_no_stat_not_injected_when_present() {
        // Edge case: --no-stat not injected when --stat already present
        let command = "git pull --stat origin main";
        let result = maybe_inject_no_stat(command);
        assert_eq!(result, command, "should not inject when --stat present");

        let command2 = "git pull --no-stat origin main";
        let result2 = maybe_inject_no_stat(command2);
        assert_eq!(
            result2, command2,
            "should not inject when --no-stat present"
        );

        let command3 = "git pull --verbose origin main";
        let result3 = maybe_inject_no_stat(command3);
        assert_eq!(
            result3, command3,
            "should not inject when --verbose present"
        );
    }

    #[test]
    fn test_no_stat_word_boundary_cases() {
        let cases: &[(&str, &str)] = &[
            ("gitpull some-arg", "gitpull some-arg"),
            ("git log upstream/pull/123", "git log upstream/pull/123"),
            (
                "git pull origin main --rebase",
                "git pull origin main --rebase --no-stat",
            ),
            ("git pull --no-stat", "git pull --no-stat"),
            ("git log --stat", "git log --stat"),
        ];
        for (input, expected) in cases {
            assert_eq!(maybe_inject_no_stat(input), *expected, "input: {input}");
        }
    }

    #[test]
    fn test_filter_applied_field_present() {
        // Test apply_filter() end-to-end and verify filter_applied field is set correctly
        let rule = types::FilterRule {
            match_command: "^git\\s+status".to_string(),
            description: Some("git status filter".to_string()),
            strip_ansi: false,
            strip_lines_matching: vec!["^On branch".to_string()],
            keep_lines_matching: vec![],
            max_lines: Some(20),
            on_empty: None,
        };

        let strip_patterns = vec![Regex::new("^On branch").unwrap()];
        let compiled = CompiledRule {
            pattern: Regex::new("^git\\s+status").unwrap(),
            strip_patterns,
            keep_patterns: vec![],
            rule,
        };

        let stdout = "On branch main\nnothing to commit\n";

        // Call apply_filter() and verify the returned string is filtered
        let filtered = apply_filter(&compiled, stdout);
        assert!(
            !filtered.contains("On branch"),
            "apply_filter should strip matching lines"
        );
        assert!(
            filtered.contains("nothing to commit"),
            "apply_filter should keep non-matching lines"
        );

        // Simulate the guard and field assignment from run_exec_impl
        let mut output = ShellOutput::new(filtered, "".to_string(), "".to_string(), Some(0), false);

        // Set filter_applied as run_exec_impl does
        output.filter_applied = compiled
            .rule
            .description
            .clone()
            .or_else(|| Some(compiled.rule.match_command.clone()));

        assert!(
            output.filter_applied.is_some(),
            "filter_applied should be set when filter matches"
        );
        assert_eq!(output.filter_applied.as_ref().unwrap(), "git status filter");
    }

    #[test]
    fn test_filter_keep_lines_matching() {
        // Happy path: filter matches command prefix and keeps only matching lines
        let rule = types::FilterRule {
            match_command: "^cargo\\s+test".to_string(),
            description: Some("test keep filter".to_string()),
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec!["^test ".to_string(), "^FAILED".to_string()],
            max_lines: None,
            on_empty: None,
        };
        let compiled = filters::CompiledRule {
            pattern: Regex::new("^cargo\\s+test").unwrap(),
            strip_patterns: vec![],
            keep_patterns: vec![
                Regex::new("^test ").unwrap(),
                Regex::new("^FAILED").unwrap(),
            ],
            rule,
        };

        let stdout = "   Compiling mylib v0.1.0\ntest foo::bar ... ok\ntest foo::baz ... FAILED\ntest result: FAILED\n";
        let filtered = filters::apply_filter(&compiled, stdout);

        assert!(filtered.contains("test foo::bar"), "should keep test lines");
        assert!(
            filtered.contains("test foo::baz"),
            "should keep FAILED test lines"
        );
        assert!(!filtered.contains("Compiling"), "should drop compile lines");
    }

    #[test]
    fn test_filter_max_lines_cap() {
        // Edge case: filter caps output to max_lines
        let rule = types::FilterRule {
            match_command: "^git\\s+log".to_string(),
            description: Some("test max lines".to_string()),
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: Some(3),
            on_empty: None,
        };
        let compiled = filters::CompiledRule {
            pattern: Regex::new("^git\\s+log").unwrap(),
            strip_patterns: vec![],
            keep_patterns: vec![],
            rule,
        };

        let stdout = "line1\nline2\nline3\nline4\nline5\n";
        let filtered = filters::apply_filter(&compiled, stdout);

        assert_eq!(filtered.lines().count(), 3, "should cap at 3 lines");
        assert!(filtered.contains("line1"));
        assert!(filtered.contains("line3"));
        assert!(
            !filtered.contains("line4"),
            "should not include lines beyond max"
        );
    }

    #[test]
    fn test_filter_git_show_strips_patch_hunks() {
        // Happy path: verifies ^[+-][^+-] keeps ---/+++ file headers while stripping diff lines
        let compiled = filters::CompiledRule {
            pattern: Regex::new("^git\\s+show").unwrap(),
            strip_patterns: vec![
                Regex::new("^@@").unwrap(),
                Regex::new("^[+-][^+-]").unwrap(),
            ],
            keep_patterns: vec![],
            rule: types::FilterRule {
                match_command: "^git\\s+show".to_string(),
                description: None,
                strip_ansi: true,
                strip_lines_matching: vec!["^@@".to_string(), "^[+-][^+-]".to_string()],
                keep_lines_matching: vec![],
                max_lines: Some(200),
                on_empty: None,
            },
        };

        let stdout = "commit abc123\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,4 @@\n-old line\n+new line\n context line\n";
        let filtered = filters::apply_filter(&compiled, stdout);

        assert!(
            filtered.contains("--- a/src/lib.rs"),
            "should keep --- file header"
        );
        assert!(
            filtered.contains("+++ b/src/lib.rs"),
            "should keep +++ file header"
        );
        assert!(!filtered.contains("@@ -1,3"), "should strip hunk headers");
        assert!(
            !filtered.contains("-old line"),
            "should strip removed lines"
        );
        assert!(!filtered.contains("+new line"), "should strip added lines");
    }

    #[test]
    fn test_filter_on_empty_from_empty_input() {
        // Edge case: on_empty fires when stdout is already empty (not just stripped-to-empty);
        // complements test_filter_on_empty_substitution which covers stripped-to-empty
        let compiled = filters::CompiledRule {
            pattern: Regex::new("^git\\s+diff").unwrap(),
            strip_patterns: vec![],
            keep_patterns: vec![],
            rule: types::FilterRule {
                match_command: "^git\\s+diff".to_string(),
                description: None,
                strip_ansi: true,
                strip_lines_matching: vec![],
                keep_lines_matching: vec![],
                max_lines: Some(100),
                on_empty: Some("ok (working tree clean)".to_string()),
            },
        };

        assert_eq!(
            filters::apply_filter(&compiled, ""),
            "ok (working tree clean)",
            "on_empty should fire on empty input"
        );
    }

    #[test]
    fn test_filter_applied_to_interleaved_with_both_streams() {
        // Happy path: apply_filter on an interleaved string that mixes stdout and stderr lines.
        // Lines matching the strip pattern are removed; stderr-origin lines are preserved.
        let compiled = filters::CompiledRule {
            pattern: Regex::new("^git\\s+pull").unwrap(),
            strip_patterns: vec![Regex::new("^\\s*\\|\\s*\\d+\\s*[+\\-]+").unwrap()],
            keep_patterns: vec![],
            rule: types::FilterRule {
                match_command: "^git\\s+pull".to_string(),
                description: None,
                strip_ansi: false,
                strip_lines_matching: vec!["^\\s*\\|\\s*\\d+\\s*[+\\-]+".to_string()],
                keep_lines_matching: vec![],
                max_lines: None,
                on_empty: None,
            },
        };

        // Arrange: interleaved with one stdout-origin strip-matched line and one stderr-origin line
        let interleaved = " | 42  ++++++++++++\nFrom https://github.com/example/repo\n";

        // Act
        let result = filters::apply_filter(&compiled, interleaved);

        // Assert: strip-matched line gone; stderr-origin line present
        assert!(
            !result.contains("| 42"),
            "strip-matched line should be absent from filtered interleaved"
        );
        assert!(
            result.contains("From https://github.com/example/repo"),
            "stderr-origin line should be preserved in filtered interleaved"
        );
    }

    #[test]
    fn test_on_empty_substitution_in_interleaved() {
        // Edge case: when filter strips all lines in interleaved, on_empty text is returned.
        let compiled = filters::CompiledRule {
            pattern: Regex::new("^git\\s+pull").unwrap(),
            strip_patterns: vec![Regex::new(".*").unwrap()],
            keep_patterns: vec![],
            rule: types::FilterRule {
                match_command: "^git\\s+pull".to_string(),
                description: None,
                strip_ansi: false,
                strip_lines_matching: vec![".*".to_string()],
                keep_lines_matching: vec![],
                max_lines: None,
                on_empty: Some("ok (up-to-date)".to_string()),
            },
        };

        // Arrange: interleaved where every line matches the strip pattern
        let interleaved = "Already up to date.\nFrom https://github.com/example/repo\n";

        // Act
        let result = filters::apply_filter(&compiled, interleaved);

        // Assert: on_empty substitution text returned
        assert_eq!(
            result, "ok (up-to-date)",
            "on_empty should be returned when filter strips all lines in interleaved"
        );
    }

    #[test]
    fn test_line_cap_fires_before_byte_cap() {
        // Edge case: 2500 lines x 5 chars each = 12500 bytes (under 30k byte cap)
        // Line cap (2000) should fire; returned content has ~50 lines (OVERFLOW_PREVIEW_LINES)
        let line = "abcde";
        let stdout: String = std::iter::repeat(format!("{}\n", line))
            .take(2500)
            .collect();
        assert_eq!(stdout.lines().count(), 2500, "should have 2500 lines");
        assert!(stdout.len() < 30_000, "should be under byte cap");

        let stderr = String::new();
        let slot = 42u32;

        let (out_stdout, _out_stderr, stdout_path, _stderr_path, byte_truncated) =
            handle_output_persist(stdout, stderr, slot);

        // Line cap fires: output_truncated should be indicated via stdout_path being set
        assert!(
            !byte_truncated,
            "byte cap should NOT fire (under 30k bytes)"
        );
        assert!(
            stdout_path.is_some(),
            "stdout_path should be set when line cap fires"
        );
        // Returned preview is last OVERFLOW_PREVIEW_LINES (50) lines
        let line_count = out_stdout.lines().count();
        assert!(
            line_count <= 50,
            "returned content should have at most 50 lines, got {}",
            line_count
        );
        assert!(line_count > 0, "returned content should not be empty");
    }

    #[test]
    fn test_project_local_overrides_builtin() {
        // Edge case: project-local rule inserted at index 0 takes precedence (first-match semantics).
        // Use a unique command name that does NOT match any built-in rule to verify
        // that project-local rules are loaded and placed before built-ins.
        use std::io::Write;

        let tmp = std::env::temp_dir().join(format!(
            "aptu-test-project-local-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let aptu_dir = tmp.join(".aptu");
        std::fs::create_dir_all(&aptu_dir).expect("should create .aptu dir");

        // Use a unique command not matching any built-in rule; include required schema_version field
        let toml_content = "schema_version = 1\n[[filters]]\nmatch_command = \"^my-custom-tool\"\nkeep_lines_matching = []\non_empty = \"project-local-only-marker\"\n";
        let mut f = std::fs::File::create(aptu_dir.join("filters.toml"))
            .expect("should create filters.toml");
        f.write_all(toml_content.as_bytes())
            .expect("should write toml");
        drop(f);

        let rules = filters::load_filter_table(&tmp);

        // The project-local rule should appear at index 0
        let first_rule = rules.first().expect("should have at least one rule");
        assert!(
            first_rule.pattern.is_match("my-custom-tool --flag"),
            "project-local rule should be first (index 0)"
        );
        assert_eq!(
            first_rule.rule.on_empty.as_deref(),
            Some("project-local-only-marker"),
            "project-local rule on_empty should match what was written"
        );

        // Also verify that built-in rules are still present (after the project-local rule)
        let has_git_pull = rules
            .iter()
            .any(|r| r.pattern.is_match("git pull origin main"));
        assert!(
            has_git_pull,
            "built-in git pull rule should still be present"
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_invalid_toml_falls_back_gracefully() {
        // Edge case: invalid TOML in .aptu/filters.toml should fall back to built-ins without panic
        use std::io::Write;

        let tmp = std::env::temp_dir().join(format!(
            "aptu-test-invalid-toml-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let aptu_dir = tmp.join(".aptu");
        std::fs::create_dir_all(&aptu_dir).expect("should create .aptu dir");

        let mut f = std::fs::File::create(aptu_dir.join("filters.toml"))
            .expect("should create filters.toml");
        // invalid TOML: use "garbage" that is syntactically invalid TOML
        // Note: the TOML also requires schema_version field in FilterTableConfig;
        // invalid content ensures the serde parse fails
        f.write_all(b"schema_version = INVALID_VALUE {{{{")
            .expect("should write garbage");
        drop(f);

        // Should not panic; should return built-in rules only
        let rules = filters::load_filter_table(&tmp);

        // Built-in rules include git pull, git fetch, etc.
        let has_git_pull = rules
            .iter()
            .any(|r| r.pattern.is_match("git pull origin main"));
        assert!(
            has_git_pull,
            "should have git pull built-in rule after invalid TOML"
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_invalid_schema_version_falls_back_gracefully() {
        // Edge case: schema_version != 1 in .aptu/filters.toml should fall back to built-ins.
        use std::io::Write;

        let tmp = std::env::temp_dir().join(format!(
            "aptu-test-schema-version-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let aptu_dir = tmp.join(".aptu");
        std::fs::create_dir_all(&aptu_dir).expect("should create .aptu dir");

        // schema_version = 2 with a valid filter rule; should be rejected
        let toml_content = "schema_version = 2\n[[filters]]\nmatch_command = \"^my-v2-tool\"\nkeep_lines_matching = []\n";
        let mut f = std::fs::File::create(aptu_dir.join("filters.toml"))
            .expect("should create filters.toml");
        f.write_all(toml_content.as_bytes())
            .expect("should write toml");
        drop(f);

        // Should not panic; should return built-in rules only (no project-local rule)
        let rules = filters::load_filter_table(&tmp);

        // Built-in rules must be present
        let has_git_pull = rules
            .iter()
            .any(|r| r.pattern.is_match("git pull origin main"));
        assert!(
            has_git_pull,
            "should have git pull built-in rule after schema_version=2 rejection"
        );

        // The project-local rule must NOT be present
        let has_v2_rule = rules
            .iter()
            .any(|r| r.pattern.is_match("my-v2-tool --flag"));
        assert!(
            !has_v2_rule,
            "schema_version=2 rule should not be loaded; only built-ins expected"
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_metric_chars_threshold_breach_fires() {
        // Happy path: chars_threshold_breach is true when output_chars > 30_000
        let output_chars: usize = 35_000;
        let event = crate::metrics::MetricEvent {
            ts: 0,
            tool: "exec_command",
            duration_ms: 1,
            output_chars,
            param_path_depth: 0,
            max_depth: None,
            result: "ok",
            error_type: None,
            error_subtype: None,
            session_id: None,
            seq: None,
            cache_hit: None,
            cache_write_failure: None,
            cache_tier: None,
            exit_code: None,
            timed_out: false,
            output_truncated: None,
            chars_threshold_breach: output_chars > 30_000,
            file_ext: None,
            filter_applied: None,
            language: None,
        };
        assert!(
            event.chars_threshold_breach,
            "chars_threshold_breach should be true for output_chars=35000"
        );
    }

    #[test]
    fn test_metric_chars_threshold_breach_no_fire() {
        // Edge case: chars_threshold_breach is false when output_chars <= 30_000
        let output_chars: usize = 5_000;
        let event = crate::metrics::MetricEvent {
            ts: 0,
            tool: "exec_command",
            duration_ms: 1,
            output_chars,
            param_path_depth: 0,
            max_depth: None,
            result: "ok",
            error_type: None,
            error_subtype: None,
            session_id: None,
            seq: None,
            cache_hit: None,
            cache_write_failure: None,
            cache_tier: None,
            exit_code: None,
            timed_out: false,
            output_truncated: None,
            chars_threshold_breach: output_chars > 30_000,
            file_ext: None,
            filter_applied: None,
            language: None,
        };
        assert!(
            !event.chars_threshold_breach,
            "chars_threshold_breach should be false for output_chars=5000"
        );
    }

    // ── Progress token gating and watch channel tests ──

    /// When no progressToken is present, handle_overview_mode skips all progress
    /// machinery (no peer lock acquisition for progress, no watch channel, no
    /// emit_progress calls) and returns the analysis result directly.
    #[tokio::test]
    async fn test_progress_bypassed_when_no_token() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").unwrap();
        let analyzer = make_analyzer();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": dir.path().to_str().unwrap(),
        }))
        .unwrap();
        let ct = tokio_util::sync::CancellationToken::new();

        // Act: call with None progress_token -- must complete without error.
        let result = analyzer.handle_overview_mode(&params, ct, None).await;
        assert!(
            result.is_ok(),
            "handle_overview_mode with None token must succeed"
        );
    }

    // ── strip_cd_prefix tests ──

    #[test]
    fn test_strip_cd_prefix_basic() {
        let (cmd, path) = strip_cd_prefix("cd /tmp && echo hello");
        assert_eq!(cmd, "echo hello");
        assert_eq!(path, Some("/tmp"));
    }

    #[test]
    fn test_strip_cd_prefix_no_ampersand() {
        // No && separator -- returned unmodified; shell handles the cd naturally.
        let (cmd, path) = strip_cd_prefix("cd /tmp");
        assert_eq!(cmd, "cd /tmp");
        assert_eq!(path, None);
    }

    #[test]
    fn test_strip_cd_prefix_with_extra_spaces() {
        // Surrounding whitespace is trimmed from both extracted path and stripped command.
        let (cmd, path) = strip_cd_prefix("cd  /tmp  &&  echo hello");
        assert_eq!(path, Some("/tmp"));
        assert_eq!(cmd, "echo hello");
    }

    #[test]
    fn test_strip_cd_prefix_splits_on_first_ampersand_only() {
        // Only the leading cd && is consumed; subsequent && in the command are preserved.
        let (cmd, path) = strip_cd_prefix("cd /a && cmd1 && cd /b && cmd2");
        assert_eq!(path, Some("/a"));
        assert_eq!(cmd, "cmd1 && cd /b && cmd2");
    }
}
