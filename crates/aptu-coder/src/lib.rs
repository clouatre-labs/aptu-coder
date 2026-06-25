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
pub(crate) mod logging;
pub(crate) mod metrics;
pub(crate) mod otel;
pub(crate) mod shell;
/// Heredoc and shell file-write pattern detection (pre-spawn guard for exec_command).
pub(crate) mod shell_write;
pub(crate) mod tools;
pub(crate) mod validation;

pub use logging::{LogEvent, McpLoggingLayer};
pub use metrics::{MetricEvent, MetricsSender, MetricsWriter, migrate_legacy_metrics_dir};
pub use otel::{
    ClientMetadata, extract_and_set_trace_context, init_log_appender, init_meter, init_otel,
};

use aptu_coder_core::analyze;
use aptu_coder_core::{cache, completion, types};
use validation::validate_path;

use crate::tools::common::{err_to_tool_result, no_cache_meta};

pub const STDIN_MAX_BYTES: usize = 1_048_576;

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
    #[must_use]
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
    #[must_use]
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

#[cfg(test)]
use aptu_coder_core::cache::CacheTier;
use aptu_coder_core::cache::{AnalysisCache, CallGraphCache};
use aptu_coder_core::types::{
    AnalyzeDirectoryParams, AnalyzeFileParams, AnalyzeModuleParams, AnalyzeSymbolParams,
    EditOverwriteOutput, EditOverwriteParams, EditReplaceOutput, EditReplaceParams,
};
use filters::CompiledRule;
#[cfg(test)]
use filters::{apply_filter, maybe_inject_no_stat};

use rmcp::handler::server::tool::{ToolRouter, schema_for_type};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, CancelledNotificationParam, CompleteRequestParams, CompleteResult,
    CompletionInfo, Content, ErrorData, Implementation, InitializeRequestParams, InitializeResult,
    LoggingLevel, Meta, ServerCapabilities, SetLevelRequestParams,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{Peer, RoleServer, ServerHandler, tool, tool_handler, tool_router};

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as TokioMutex, RwLock, mpsc};
use tracing::instrument;
use tracing_subscriber::filter::LevelFilter;

static GLOBAL_SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// 5_000 chars fires at ~150-180 files at depth=2 (~28-33 chars/file).
// Empirical data (684 calls, Jun 2026): max observed output was 4,882 chars; the old
// 50_000 threshold never triggered once. At 5_000, auto-summary engages for repos that
// would otherwise produce an overwhelming flat response.
pub(crate) const SIZE_LIMIT: usize = 5_000;

pub(crate) fn err_to_tool_result_from_pagination(
    e: aptu_coder_core::pagination::PaginationError,
) -> CallToolResult {
    let msg = format!("Pagination error: {}", e);
    CallToolResult::error(vec![Content::text(msg)]).with_meta(Some(no_cache_meta()))
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
        crate::tools::server::build_analyzer(peer, log_level_filter, event_rx, metrics_tx)
    }

    /// Emit a "received" metric event for the given tool name.
    /// Increments the session call sequence, locks the session ID, and sends
    /// the metric event via the channel. Returns the (seq, sid) pair for use
    /// by the caller in exit metrics, preserving per-call seq uniqueness.
    async fn emit_received_metric(&self, tool: &'static str) -> (u32, Option<String>) {
        crate::tools::server::emit_received_metric(
            &self.metrics_tx,
            &self.session_id,
            &self.session_call_seq,
            tool,
        )
        .await
    }

    /// Delegates to [`tools::server::handle_overview_mode`].
    /// Kept for test access; production path goes through `analyze_directory` shim.
    #[cfg(test)]
    pub(crate) async fn handle_overview_mode(
        &self,
        params: &AnalyzeDirectoryParams,
        ct: tokio_util::sync::CancellationToken,
    ) -> Result<(std::sync::Arc<analyze::AnalysisOutput>, CacheTier), ErrorData> {
        let ctx = crate::tools::AnalyzeDirectoryContext {
            cache: self.cache.clone(),
            disk_cache: self.disk_cache.clone(),
            metrics_tx: self.metrics_tx.clone(),
            peer: self.peer.clone(),
            sid: self.session_id.lock().await.clone(),
        };
        crate::tools::server::handle_overview_mode(&ctx, params, ct).await
    }

    /// Delegates to [`tools::server::handle_file_details_mode`].
    /// Kept for test access; production path goes through `analyze_file` shim.
    #[cfg(test)]
    pub(crate) async fn handle_file_details_mode(
        &self,
        params: &aptu_coder_core::types::AnalyzeFileParams,
    ) -> Result<(std::sync::Arc<analyze::FileAnalysisOutput>, CacheTier), ErrorData> {
        crate::tools::server::handle_file_details_mode(
            self.cache.clone(),
            self.disk_cache.clone(),
            self.metrics_tx.clone(),
            self.session_id.lock().await.clone(),
            params,
        )
        .await
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
        params.max_depth = params.max_depth.or(Some(3));
        let t_start = std::time::Instant::now();
        let (seq, sid) = self.emit_received_metric("analyze_directory").await;
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
        let ctx = tools::AnalyzeDirectoryContext {
            cache: self.cache.clone(),
            disk_cache: self.disk_cache.clone(),
            metrics_tx: self.metrics_tx.clone(),
            peer: self.peer.clone(),
            sid: sid.clone(),
        };
        tools::analyze_directory::analyze_directory_handler(
            &ctx,
            params,
            tools::DirectoryHandlerCall {
                seq,
                sid,
                t_start,
                param_path,
                max_depth_val,
                ct,
            },
            &span,
        )
        .await
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
        let ctx = tools::AnalyzeFileContext {
            cache: self.cache.clone(),
            disk_cache: self.disk_cache.clone(),
            metrics_tx: self.metrics_tx.clone(),
            sid: sid.clone(),
        };
        tools::analyze_file::analyze_file_handler(
            &ctx, params, seq, sid, t_start, param_path, &span,
        )
        .await
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
        let ctx = tools::AnalyzeSymbolContext {
            metrics_tx: self.metrics_tx.clone(),
            call_graph_cache: self.call_graph_cache.clone(),
            disk_cache: self.disk_cache.clone(),
            sid: sid.clone(),
            seq,
        };
        let call = tools::AnalyzeSymbolCall {
            ct,
            param_path,
            max_depth_val,
            span,
            t_start,
        };
        tools::analyze_symbol::analyze_symbol_handler(ctx, params, call).await
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
        let ctx = tools::AnalyzeModuleContext {
            disk_cache: self.disk_cache.clone(),
            metrics_tx: self.metrics_tx.clone(),
            sid: sid.clone(),
            seq,
        };
        tools::analyze_module::analyze_module_handler(ctx, params, param_path, &span, t_start).await
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
        tools::edit_overwrite::edit_overwrite(
            params,
            tools::EditHandlerContext {
                sid,
                seq,
                cache: &self.cache,
                metrics_tx: &self.metrics_tx,
                edit_failure_counts: &self.edit_failure_counts,
            },
            &span,
            t_start,
        )
        .await
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
        tools::edit_replace::edit_replace(
            params,
            tools::EditHandlerContext {
                sid,
                seq,
                cache: &self.cache,
                metrics_tx: &self.metrics_tx,
                edit_failure_counts: &self.edit_failure_counts,
            },
            &span,
            t_start,
        )
        .await
    }

    #[tool(
        name = "exec_command",
        title = "Exec Command",
        description = "Execute shell command via sh -c (or $SHELL if set). Returns stdout, stderr, interleaved, exit_code, output_truncated. Output capped at 30 KB stdout / 10 KB stderr / 2000 lines. Set working_dir to the target directory and write commands using relative paths only; omit `cd`. Fails if working_dir does not exist or is not a directory. Pass stdin to pipe UTF-8 content into the process (max 1 MB). For file creation and edits use edit_overwrite or edit_replace; heredoc writes are rejected. Prefer machine-readable JSON output flags for build, lint, and test commands to reduce output tokens. Example queries: Run the test suite and capture output.",
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
        let session_id = self.session_id.lock().await.clone();
        let client_name = self.client_name.lock().await.clone();
        let client_version = self.client_version.lock().await.clone();
        let ctx = crate::tools::exec_command::ExecContext {
            seq,
            sid,
            session_id,
            client_name,
            client_version,
            resolved_path: self.resolved_path.as_ref().as_deref().map(str::to_owned),
            filter_table: self.filter_table.clone(),
            metrics_tx: self.metrics_tx.clone(),
            t_start,
        };
        crate::tools::exec_command::exec_command_impl(params, context, ctx).await
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
        crate::tools::server::on_initialized_impl(
            self.peer.clone(),
            self.event_rx.clone(),
            self.session_id.clone(),
            self.session_call_seq.clone(),
            self.session_profile.clone(),
            self.tool_router.clone(),
            &context.peer,
        )
        .await;
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
mod tests;
