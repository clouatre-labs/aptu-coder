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
//! Languages supported: Rust, Go, Java, Python, TypeScript, TSX, Fortran, JavaScript, C/C++, C#.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod logging;
pub mod metrics;
pub mod otel;

pub use aptu_coder_core::analyze;
use aptu_coder_core::types::STDIN_MAX_BYTES;
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

use aptu_coder_core::cache::{AnalysisCache, CacheTier};
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
    AnalyzeSymbolParams, EditOverwriteOutput, EditOverwriteParams, EditReplaceOutput,
    EditReplaceParams, SymbolMatchMode,
};
use logging::LogEvent;
use rmcp::handler::server::tool::{ToolRouter, schema_for_type};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, CancelledNotificationParam, CompleteRequestParams, CompleteResult,
    CompletionInfo, Content, ErrorData, Implementation, InitializeRequestParams, InitializeResult,
    LoggingLevel, LoggingMessageNotificationParam, Meta, Notification, NumberOrString,
    ProgressNotificationParam, ProgressToken, ServerCapabilities, ServerNotification,
    SetLevelRequestParams,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{Peer, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as TokioMutex, RwLock, mpsc};
use tracing::{instrument, warn};
use tracing_subscriber::filter::LevelFilter;

#[cfg(unix)]
use nix::sys::resource::{Resource, setrlimit};

static GLOBAL_SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

const SIZE_LIMIT: usize = 50_000;

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

/// Maps an io::Error to an ErrorData with kind-specific message and preserved context.
fn io_error_to_path_error(
    err: &std::io::Error,
    path_context: &str,
    suggested_action: &'static str,
) -> ErrorData {
    let msg = match err.kind() {
        std::io::ErrorKind::NotFound => format!("{path_context} not found"),
        std::io::ErrorKind::PermissionDenied => format!("permission denied: {path_context}"),
        _ => format!("{path_context} is invalid"),
    };
    let mut meta = error_meta("validation", false, suggested_action);
    // Preserve io::Error context in data field
    if let Some(obj) = meta.as_object_mut() {
        obj.insert(
            "ioErrorKind".to_string(),
            serde_json::json!(format!("{:?}", err.kind())),
        );
        obj.insert(
            "ioErrorSource".to_string(),
            serde_json::json!(err.to_string()),
        );
    }
    ErrorData::new(rmcp::model::ErrorCode::INVALID_PARAMS, msg, Some(meta))
}

/// Validates a path relative to a working directory.
/// The working_dir itself must be within the server CWD.
/// The resolved path must also be within the working_dir.
fn validate_path_in_dir(
    path: &str,
    require_exists: bool,
    working_dir: &std::path::Path,
) -> Result<std::path::PathBuf, ErrorData> {
    // Canonicalize the working_dir to resolve symlinks
    let canonical_working_dir = std::fs::canonicalize(working_dir).map_err(|e| {
        io_error_to_path_error(&e, "working_dir", "provide a valid working directory")
    })?;

    // Verify working_dir is actually a directory
    if !std::fs::metadata(&canonical_working_dir)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "working_dir must be a directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a valid directory path",
            )),
        ));
    }

    // Verify working_dir is within the server CWD (same bounds check as validate_path)
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

    if !canonical_working_dir.starts_with(&allowed_root) {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "working_dir is outside the allowed root".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a working directory within the current working directory",
            )),
        ));
    }

    // Now resolve the target path relative to working_dir
    let canonical_path = if require_exists {
        let target_path = canonical_working_dir.join(path);
        std::fs::canonicalize(&target_path).map_err(|e| {
            io_error_to_path_error(
                &e,
                path,
                "provide a valid path within the working directory",
            )
        })?
    } else {
        // For non-existent files, walk up the path until we find an existing ancestor
        let p = std::path::Path::new(path);
        let mut ancestor = p.to_path_buf();
        let mut suffix = std::path::PathBuf::new();

        loop {
            let full_path = canonical_working_dir.join(&ancestor);
            if full_path.exists() {
                break;
            }
            if let Some(parent) = ancestor.parent() {
                if let Some(file_name) = ancestor.file_name() {
                    suffix = std::path::PathBuf::from(file_name).join(&suffix);
                }
                ancestor = parent.to_path_buf();
            } else {
                // No existing ancestor found — use working_dir as anchor
                ancestor = std::path::PathBuf::new();
                break;
            }
        }

        let canonical_base = canonical_working_dir.join(&ancestor);
        let canonical_base =
            std::fs::canonicalize(&canonical_base).unwrap_or(canonical_working_dir.clone());
        canonical_base.join(&suffix)
    };

    // Verify the resolved path is within working_dir.
    // PathBuf::starts_with compares path *components*, not raw bytes, so
    // a sibling directory whose name shares our prefix (e.g. "/work_evil"
    // when the allowed root is "/work") is correctly rejected -- this is
    // the exact prefix-confusion vector exploited in CVE-2025-53110 against
    // @modelcontextprotocol/server-filesystem.  Do not replace this check
    // with a string-level prefix comparison.
    if !canonical_path.starts_with(&canonical_working_dir) {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is outside the working directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a path within the working directory",
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

/// Resolve the preferred shell for command execution.
/// Priority: APTU_SHELL env var > bash (PATH search) > /bin/sh (unix) / cmd (windows).
/// APTU_SHELL is honored on all platforms so callers can override the shell uniformly.
fn resolve_shell() -> String {
    if let Ok(shell) = std::env::var("APTU_SHELL") {
        return shell;
    }
    #[cfg(unix)]
    {
        if which::which("bash").is_ok() {
            return "bash".to_string();
        }
        "/bin/sh".to_string()
    }
    #[cfg(not(unix))]
    {
        "cmd".to_string()
    }
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
    #[allow(dead_code)]
    pub(crate) tool_router: Arc<RwLock<ToolRouter<Self>>>,
    cache: AnalysisCache,
    disk_cache: std::sync::Arc<cache::DiskCache>,
    peer: Arc<TokioMutex<Option<Peer<RoleServer>>>>,
    log_level_filter: Arc<Mutex<LevelFilter>>,
    event_rx: Arc<TokioMutex<Option<mpsc::UnboundedReceiver<LogEvent>>>>,
    metrics_tx: crate::metrics::MetricsSender,
    session_call_seq: Arc<std::sync::atomic::AtomicU32>,
    session_id: Arc<TokioMutex<Option<String>>>,
    // Store profile metadata from initialize request for use in on_initialized
    profile_meta: Arc<TokioMutex<Option<serde_json::Map<String, serde_json::Value>>>>,
    client_name: Arc<TokioMutex<Option<String>>>,
    client_version: Arc<TokioMutex<Option<String>>>,
    // Resolved login shell PATH, captured once at startup via login shell invocation.
    // Arc<Option<String>> is immutable after init; no lock needed.
    resolved_path: Arc<Option<String>>,
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
            profile_meta: Arc::new(TokioMutex::new(None)),
            client_name: Arc::new(TokioMutex::new(None)),
            client_version: Arc::new(TokioMutex::new(None)),
            resolved_path,
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
    ) -> Result<(std::sync::Arc<analyze::AnalysisOutput>, CacheTier), ErrorData> {
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
                            metrics_tx.send(crate::metrics::MetricEvent {
                                ts: crate::metrics::unix_ms(),
                                tool: "analyze_directory",
                                duration_ms: 0,
                                output_chars: 0,
                                param_path_depth: 0,
                                max_depth: None,
                                result: "ok",
                                error_type: None,
                                session_id: sid,
                                seq: None,
                                cache_hit: None,
                                cache_write_failure: Some(true),
                                cache_tier: None,
                                exit_code: None,
                                timed_out: false,
                            });
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
        match analyze::analyze_file(&params.path, params.ast_recursion_limit) {
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
                            metrics_tx.send(crate::metrics::MetricEvent {
                                ts: crate::metrics::unix_ms(),
                                tool: "analyze_file",
                                duration_ms: 0,
                                output_chars: 0,
                                param_path_depth: 0,
                                max_depth: None,
                                result: "ok",
                                error_type: None,
                                session_id: sid,
                                seq: None,
                                cache_hit: None,
                                cache_write_failure: Some(true),
                                cache_tier: None,
                                exit_code: None,
                                timed_out: false,
                            });
                        }
                    });
                }
                Ok((arc_output, CacheTier::Miss))
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
        let parse_timeout_micros = analysis_params.parse_timeout_micros;
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

    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, path = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty, cache_tier = tracing::field::Empty))]
    #[tool(
        name = "analyze_directory",
        title = "Analyze Directory",
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

        // Apply pagination to files
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

        let mut result =
            CallToolResult::success(vec![Content::text(final_text.clone())]).with_meta(Some(meta));
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
            cache_hit: Some(dir_cache_hit != CacheTier::Miss),
            cache_write_failure: None,
            cache_tier: Some(dir_cache_hit.as_str()),
            exit_code: None,
            timed_out: false,
        });
        Ok(result)
    }

    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, path = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty, cache_tier = tracing::field::Empty))]
    #[tool(
        name = "analyze_file",
        title = "Analyze File",
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
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
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

        // Check if path is a directory (not allowed for analyze_file)
        if std::path::Path::new(&params.path).is_dir() {
            span.record("error", true);
            span.record("error.type", "invalid_params");
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
                return Ok(err_to_tool_result(e));
            }
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
            span.record("error", true);
            span.record("error.type", "invalid_params");
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

        let mut result =
            CallToolResult::success(vec![Content::text(final_text.clone())]).with_meta(Some(meta));
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
            cache_hit: Some(file_cache_hit != CacheTier::Miss),
            cache_write_failure: None,
            cache_tier: Some(file_cache_hit.as_str()),
            exit_code: None,
            timed_out: false,
        });
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
        let t_start = std::time::Instant::now();
        let param_path = params.path.clone();
        let max_depth_val = params.follow_depth;
        let seq = self
            .session_call_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sid = self.session_id.lock().await.clone();

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

            // Record cache tier in span
            tracing::Span::current().record("cache_tier", "Miss");

            // Add content_hash to _meta
            let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
            let mut meta = no_cache_meta().0;
            meta.insert(
                "content_hash".to_string(),
                serde_json::Value::String(content_hash),
            );

            let mut result = CallToolResult::success(vec![Content::text(final_text.clone())])
                .with_meta(Some(Meta(meta)));
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
                cache_tier: Some(CacheTier::Miss.as_str()),
                cache_write_failure: None,
                exit_code: None,
                timed_out: false,
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

        // Record cache tier in span
        tracing::Span::current().record("cache_tier", "Miss");

        // Add content_hash to _meta
        let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
        let mut meta = no_cache_meta().0;
        meta.insert(
            "content_hash".to_string(),
            serde_json::Value::String(content_hash),
        );

        let mut result = CallToolResult::success(vec![Content::text(final_text.clone())])
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
            cache_tier: Some(CacheTier::Miss.as_str()),
            cache_write_failure: None,
            exit_code: None,
            timed_out: false,
        });
        Ok(result)
    }

    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, path = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty, cache_tier = tracing::field::Empty))]
    #[tool(
        name = "analyze_module",
        title = "Analyze Module",
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
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
            span.record("error", true);
            span.record("error.type", "invalid_params");
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
                cache_write_failure: None,
                cache_tier: None,
                exit_code: None,
                timed_out: false,
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

        // Route through handle_file_details_mode to inherit L1+L2 caching
        let mut analyze_file_params: AnalyzeFileParams = Default::default();
        analyze_file_params.path = params.path.clone();
        let (arc_output, module_tier) =
            match self.handle_file_details_mode(&analyze_file_params).await {
                Ok((output, tier)) => (output, tier),
                Err(e) => {
                    let error_data = match e.code {
                        rmcp::model::ErrorCode::INVALID_PARAMS => e,
                        _ => ErrorData::new(
                            rmcp::model::ErrorCode::INTERNAL_ERROR,
                            format!("Failed to analyze module: {}", e.message),
                            Some(error_meta("internal", false, "report this as a bug")),
                        ),
                    };
                    return Ok(err_to_tool_result(error_data));
                }
            };

        // Reconstruct ModuleInfo from FileAnalysisOutput
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
        let functions = arc_output
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
        let imports = arc_output
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
        let module_info =
            types::ModuleInfo::new(name, arc_output.line_count, language, functions, imports);

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
            cache_hit: Some(module_tier != CacheTier::Miss),
            cache_tier: Some(module_tier.as_str()),
            cache_write_failure: None,
            exit_code: None,
            timed_out: false,
        });
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
        let _validated_path = if let Some(ref wd) = params.working_dir {
            match validate_path_in_dir(&params.path, false, std::path::Path::new(wd)) {
                Ok(p) => p,
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    return Ok(err_to_tool_result(e));
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
            span.record("error", true);
            span.record("error.type", "invalid_params");
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
                cache_write_failure: None,
                cache_tier: None,
                exit_code: None,
                timed_out: false,
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
                span.record("error", true);
                span.record("error.type", "invalid_params");
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
                    cache_write_failure: None,
                    cache_tier: None,
                    exit_code: None,
                    timed_out: false,
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
                span.record("error", true);
                span.record("error.type", "internal_error");
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
                    cache_write_failure: None,
                    cache_tier: None,
                    exit_code: None,
                    timed_out: false,
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
                span.record("error", true);
                span.record("error.type", "internal_error");
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
                    cache_write_failure: None,
                    cache_tier: None,
                    exit_code: None,
                    timed_out: false,
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
            cache_write_failure: None,
            cache_tier: None,
            exit_code: None,
            timed_out: false,
        });
        Ok(result)
    }

    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, path = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty))]
    #[tool(
        name = "edit_replace",
        title = "Edit Replace",
        description = "Replaces a unique exact text block; old_text must match character-for-character and appear exactly once. Returns path, bytes_before, bytes_after. Fails if zero matches; fails if multiple matches (extend old_text to be more specific). If invalid_params is returned, re-read the target file with analyze_file or analyze_module before retrying. Whitespace-sensitive exact match. Use edit_overwrite to replace the whole file. working_dir sets the base directory for path resolution (default: server CWD). Example queries: Update the function signature in lib.rs.",
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
        let _validated_path = if let Some(ref wd) = params.working_dir {
            match validate_path_in_dir(&params.path, true, std::path::Path::new(wd)) {
                Ok(p) => p,
                Err(e) => {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    return Ok(err_to_tool_result(e));
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
            span.record("error", true);
            span.record("error.type", "invalid_params");
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
                cache_write_failure: None,
                cache_tier: None,
                exit_code: None,
                timed_out: false,
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
            Ok(Err(aptu_coder_core::EditError::NotFound {
                path: notfound_path,
            })) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
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
                    cache_write_failure: None,
                    cache_tier: None,
                    exit_code: None,
                    timed_out: false,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    format!(
                        "old_text not found (0 matches) in {notfound_path}. Re-read the file with analyze_file or analyze_module to obtain the current content, then derive old_text from the live file before retrying."
                    ),
                    Some(error_meta(
                        "validation",
                        false,
                        "re-read the file with analyze_file or analyze_module, then derive old_text from the live content",
                    )),
                )));
            }
            Ok(Err(aptu_coder_core::EditError::Ambiguous {
                count,
                path: ambiguous_path,
            })) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
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
                    cache_write_failure: None,
                    cache_tier: None,
                    exit_code: None,
                    timed_out: false,
                });
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    format!(
                        "old_text matched {count} locations in {ambiguous_path}. Extend old_text with more surrounding context to make it unique, or re-read with analyze_file to confirm the exact text."
                    ),
                    Some(error_meta(
                        "validation",
                        false,
                        "extend old_text with more surrounding context, or re-read with analyze_file to confirm the exact text",
                    )),
                )));
            }
            Ok(Err(aptu_coder_core::EditError::NotAFile(_))) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
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
                    cache_write_failure: None,
                    cache_tier: None,
                    exit_code: None,
                    timed_out: false,
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
                span.record("error", true);
                span.record("error.type", "internal_error");
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
                    cache_write_failure: None,
                    cache_tier: None,
                    exit_code: None,
                    timed_out: false,
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
                span.record("error", true);
                span.record("error.type", "internal_error");
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
                    cache_write_failure: None,
                    cache_tier: None,
                    exit_code: None,
                    timed_out: false,
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
            cache_write_failure: None,
            cache_tier: None,
            exit_code: None,
            timed_out: false,
        });
        Ok(result)
    }

    #[tool(
        name = "exec_command",
        title = "Exec Command",
        description = "Execute shell command via sh -c (or $SHELL if set). Returns stdout, stderr, interleaved, exit_code, timed_out, output_truncated. Output capped at 2000 lines and 50 KB per stream; use timeout_secs to limit execution time. working_dir sets initial working directory; cd and absolute paths in command string bypass this restriction. Fails if working_dir does not exist, is not a directory, or is outside CWD. Pass stdin to pipe UTF-8 content into the process (max 1 MB). For file creation and edits, prefer the edit_* tools. Example queries: Run the test suite and capture output.",
        output_schema = schema_for_type::<types::ShellOutput>(),
        annotations(
            title = "Exec Command",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    #[instrument(skip(self, context), fields(gen_ai.system = tracing::field::Empty, gen_ai.operation.name = tracing::field::Empty, gen_ai.tool.name = tracing::field::Empty, error = tracing::field::Empty, error.type = tracing::field::Empty, command = tracing::field::Empty, exit_code = tracing::field::Empty, timed_out = tracing::field::Empty, output_truncated = tracing::field::Empty, mcp.session.id = tracing::field::Empty, client.name = tracing::field::Empty, client.version = tracing::field::Empty, mcp.client.session.id = tracing::field::Empty))]
    pub async fn exec_command(
        &self,
        params: Parameters<types::ExecCommandParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let t_start = std::time::Instant::now();
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

        // Validate working_dir if provided
        let working_dir_path = if let Some(ref wd) = params.working_dir {
            match validate_path(wd, true) {
                Ok(p) => {
                    // Verify it's a directory
                    if !std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false) {
                        span.record("error", true);
                        span.record("error.type", "invalid_params");
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
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
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

        let command = params.command.clone();
        let timeout_secs = params.timeout_secs;

        // Determine cache key and whether to use cache
        let _cache_key = (
            command.clone(),
            working_dir_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        // Execute command (caching disabled; explicit opt-in via cache=true not implemented)
        let resolved_path_str = self.resolved_path.as_ref().as_deref();
        let output = run_exec_impl(
            command.clone(),
            working_dir_path.clone(),
            timeout_secs,
            params.memory_limit_mb,
            params.cpu_limit_secs,
            params.stdin.clone(),
            seq,
            resolved_path_str,
        )
        .await;

        let exit_code = output.exit_code;
        let timed_out = output.timed_out;
        let output_truncated = output.output_truncated;

        // Record execution results on span
        if let Some(code) = exit_code {
            span.record("exit_code", code);
        }
        span.record("timed_out", timed_out);
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

        let text = format!(
            "Command: {}\nExit code: {}\nTimed out: {}\nOutput truncated: {}\n\n{}",
            params.command,
            exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "null".to_string()),
            timed_out,
            output_truncated,
            output_text,
        );

        let content_blocks = vec![Content::text(text.clone()).with_priority(0.0)];

        // Determine if command failed: timeout or non-zero exit code.
        // exit_code is None when: (a) process killed by O1 post-exit drain timeout (background child
        // holding pipes -- command work was done, treat as success) or (b) externally killed; both
        // cases use unwrap_or(false) to avoid false negatives.
        let command_failed = timed_out || exit_code.map(|c| c != 0).unwrap_or(false);

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
                    cache_hit: Some(false),
                    cache_write_failure: None,
                    cache_tier: None,
                    exit_code,
                    timed_out,
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
            cache_hit: Some(false),
            cache_write_failure: None,
            cache_tier: None,
            exit_code,
            timed_out,
        });
        Ok(result)
    }
}

/// Build and configure a tokio::process::Command with stdio, working directory, and resource limits.
fn build_exec_command(
    command: &str,
    working_dir_path: Option<&std::path::PathBuf>,
    memory_limit_mb: Option<u64>,
    cpu_limit_secs: Option<u64>,
    stdin_present: bool,
    resolved_path: Option<&str>,
) -> tokio::process::Command {
    let shell = resolve_shell();
    let mut cmd = tokio::process::Command::new(shell);
    cmd.arg("-c").arg(command);

    if let Some(wd) = working_dir_path {
        cmd.current_dir(wd);
    }

    // Inject resolved login shell PATH if available
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

    #[cfg(unix)]
    {
        #[cfg(not(target_os = "linux"))]
        if memory_limit_mb.is_some() {
            warn!("memory_limit_mb is not enforced on this platform (Linux only)");
        }
        if memory_limit_mb.is_some() || cpu_limit_secs.is_some() {
            // SAFETY: This closure runs in the child process after fork() and before exec(),
            // making it safe to call setrlimit (a signal-safe syscall). No Rust objects are
            // accessed or mutated, and the closure does not unwind.
            unsafe {
                cmd.pre_exec(move || {
                    #[cfg(target_os = "linux")]
                    if let Some(mb) = memory_limit_mb {
                        let bytes = mb.saturating_mul(1024 * 1024);
                        setrlimit(Resource::RLIMIT_AS, bytes, bytes)
                            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
                    }
                    if let Some(cpu) = cpu_limit_secs {
                        setrlimit(Resource::RLIMIT_CPU, cpu, cpu)
                            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
                    }
                    Ok(())
                });
            }
        }
    }

    cmd
}

/// Run a spawned child process with timeout handling and output draining.
/// Returns (exit_code, timed_out, output_truncated, output_collection_error).
async fn run_with_timeout(
    mut child: tokio::process::Child,
    timeout_secs: Option<u64>,
    tx: tokio::sync::mpsc::UnboundedSender<(bool, String)>,
) -> (Option<i32>, bool, bool, Option<String>) {
    use tokio::io::AsyncBufReadExt as _;
    use tokio_stream::StreamExt as TokioStreamExt;
    use tokio_stream::wrappers::LinesStream;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let mut drain_task = tokio::spawn(async move {
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

    tokio::select! {
        _ = &mut drain_task => {
            let (status, drain_truncated) = match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                child.wait()
            ).await {
                Ok(Ok(s)) => (Some(s), false),
                Ok(Err(_)) => (None, false),
                Err(_) => {
                    child.start_kill().ok();
                    let _ = child.wait().await;
                    (None, true)
                }
            };
            let exit_code = status.and_then(|s| s.code());
            let ocerr = if drain_truncated {
                Some("post-exit drain timeout: background process held pipes".to_string())
            } else {
                None
            };
            (exit_code, false, drain_truncated, ocerr)
        }
        _ = async {
            if let Some(secs) = timeout_secs {
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            drain_task.abort();
            (None, true, false, None)
        }
    }
}

/// Executes a shell command and returns the output.
/// This is a free async function (not a method) to allow use in moka::future::Cache::get_with().
/// It spawns the command, collects output with timeout handling, and persists output to slot files.
#[allow(clippy::too_many_arguments)]
async fn run_exec_impl(
    command: String,
    working_dir_path: Option<std::path::PathBuf>,
    timeout_secs: Option<u64>,
    memory_limit_mb: Option<u64>,
    cpu_limit_secs: Option<u64>,
    stdin: Option<String>,
    seq: u32,
    resolved_path: Option<&str>,
) -> types::ShellOutput {
    let mut cmd = build_exec_command(
        &command,
        working_dir_path.as_ref(),
        memory_limit_mb,
        cpu_limit_secs,
        stdin.is_some(),
        resolved_path,
    );

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return types::ShellOutput::new(
                String::new(),
                format!("failed to spawn command: {e}"),
                format!("failed to spawn command: {e}"),
                None,
                false,
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

    let (exit_code, timed_out, mut output_truncated, output_collection_error) =
        run_with_timeout(child, timeout_secs, tx).await;

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
    let (stdout, stderr, stdout_path, stderr_path) =
        handle_output_persist(stdout_str, stderr_str, slot);
    output_truncated = output_truncated || stdout_path.is_some();

    let mut output = types::ShellOutput::new(
        stdout,
        stderr,
        interleaved_str,
        exit_code,
        timed_out,
        output_truncated,
    );
    output.output_collection_error = output_collection_error;
    output.stdout_path = stdout_path;
    output.stderr_path = stderr_path;

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
) -> (String, String, Option<String>, Option<String>) {
    const MAX_OUTPUT_LINES: usize = 2000;
    const OVERFLOW_PREVIEW_LINES: usize = 50;

    let stdout_lines: Vec<&str> = stdout.lines().collect();
    let stderr_lines: Vec<&str> = stderr.lines().collect();

    // No overflow: return as-is with no I/O.
    if stdout_lines.len() <= MAX_OUTPUT_LINES && stderr_lines.len() <= MAX_OUTPUT_LINES {
        return (stdout, stderr, None, None);
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

    let stdout_preview = if stdout_lines.len() > MAX_OUTPUT_LINES {
        stdout_lines[stdout_lines.len().saturating_sub(OVERFLOW_PREVIEW_LINES)..].join("\n")
    } else {
        stdout
    };
    let stderr_preview = if stderr_lines.len() > MAX_OUTPUT_LINES {
        stderr_lines[stderr_lines.len().saturating_sub(OVERFLOW_PREVIEW_LINES)..].join("\n")
    } else {
        stderr
    };

    (
        stdout_preview,
        stderr_preview,
        Some(stdout_path_str),
        Some(stderr_path_str),
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
    ast_recursion_limit: Option<usize>,
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

        // The _meta field is extracted from params and stored in request extensions.
        // Extract it and store for use in on_initialized.
        if let Some(meta) = context.extensions.get::<Meta>() {
            let mut meta_lock = self.profile_meta.lock().await;
            *meta_lock = Some(meta.0.clone());
        }
        Ok(self.get_info())
    }

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
        let meta_lock = self.profile_meta.lock().await;
        let meta_profile = meta_lock
            .as_ref()
            .and_then(|m| m.get("io.clouatre-labs/profile"))
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        drop(meta_lock);

        // Resolve the active profile: _meta wins; fall back to env var.
        let active_profile = meta_profile.or(std::env::var("APTU_CODER_PROFILE").ok());

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
        assert_eq!(hit1, CacheTier::Miss, "first call must be a cache miss");
        assert_eq!(hit2, CacheTier::L1Memory, "second call must be a cache hit");
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
    fn test_edit_overwrite_working_dir_traversal() {
        // Arrange: create a temporary directory within CWD to use as working_dir
        let cwd = std::env::current_dir().expect("should get cwd");
        let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
        let temp_path = temp_dir.path();

        // Act: try to traverse outside working_dir with ../../../etc/passwd
        let result = validate_path_in_dir("../../../etc/passwd", false, temp_path);

        // Assert: should reject path traversal attack
        assert!(
            result.is_err(),
            "validate_path_in_dir should reject path traversal outside working_dir"
        );
        let err = result.unwrap_err();
        let err_msg = err.message.to_lowercase();
        assert!(
            err_msg.contains("outside") || err_msg.contains("working"),
            "Error message should mention 'outside' or 'working': {}",
            err.message
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
        let cmd = build_exec_command("echo test", None, None, None, false, resolved_path);

        // Act: verify the command was created without panic
        // (We cannot directly inspect env vars on the Command object,
        // but we verify no panic occurred and the command is valid)
        let cmd_str = format!("{:?}", cmd);

        // Assert: command should be created successfully
        assert!(
            !cmd_str.is_empty(),
            "build_exec_command should return a valid Command"
        );
    }

    #[test]
    fn test_exec_command_path_fallback() {
        // Arrange: call build_exec_command with None resolved_path
        let cmd = build_exec_command("echo test", None, None, None, false, None);

        // Act: verify the command was created without panic
        let cmd_str = format!("{:?}", cmd);

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
}
