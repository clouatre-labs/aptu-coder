//! Extraction pattern for milestone 17 (lib.rs decomposition).
//!
//! This module will contain extracted tool handler logic as free functions,
//! keeping the `#[tool(...)]`-decorated methods in `lib.rs` as thin shims.
//! The pattern ensures a smooth, incremental decomposition with no single
//! large-bang refactor.
//!
//! Pattern rules
//! -------------
//!
//! (a) **Extracted handlers are free functions, not methods.** Each handler
//!     is a plain `pub(crate) fn` in this module, not an `impl CodeAnalyzer`
//!     method. This breaks the coupling to `&self` and makes the function
//!     testable in isolation.
//!
//! (b) **Explicit parameters instead of `&self`.** State that the handler
//!     needs (e.g., `config: &Config`, `state: &HandlerState`) is passed
//!     explicitly as function parameters. The shim in `lib.rs` extracts
//!     values from `&self` before calling the extracted function.
//!
//! (c) **`#[tool(...)]`-decorated method and `#[instrument(...)]` decorator
//!     remain in `lib.rs` as thin shims.** The `#[tool(..)]` attribute
//!     macro and the outer `#[instrument(..)]` stay on the small stub in
//!     `lib.rs`. The stub validates parameters, extracts state, and
//!     delegates to the free function here.
//!
//! (d) **The extracted free function also carries `#[instrument(skip(...))]`
//!     on its own signature.** This preserves distributed tracing context
//!     after the call leaves the `lib.rs` shim. Use `skip` for large
//!     internal types whose fields are not useful trace attributes.
//!
//! (e) **`edit_failure_counts` stays in `CodeAnalyzer` and is passed by
//!     reference to extracted edit handlers.** The concurrent failure-tracking
//!     map is not moved into `tools/`; it remains an `Arc<Mutex<...>>` field
//!     on `CodeAnalyzer`. Extracted edit handlers receive `&edit_failure_counts`
//!     as a parameter.
//!
//! (f) **`#[tool_router]` and `#[tool_handler]` impl blocks remain in
//!     `lib.rs` permanently.** The `#[tool_router]` impl on `CodeAnalyzer`
//!     and the `#[tool_handler]` impl for `ServerHandler` must not be
//!     moved into this module. They are the framework glue that ties all
//!     tools together and must live in the crate root.

pub(crate) mod analyze_directory;
pub(crate) mod analyze_file;
pub(crate) mod analyze_module;
pub(crate) mod analyze_symbol;
pub(crate) mod common;
pub(crate) mod edit_overwrite;
pub(crate) mod edit_replace;
pub(crate) mod exec_command;
pub(crate) mod exec_runtime;
pub(crate) mod server;
pub(crate) mod symbol_focused;

pub(crate) use analyze_module::AnalyzeModuleContext;
pub(crate) use analyze_symbol::AnalyzeSymbolContext;

use aptu_coder_core::cache::AnalysisCache;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Shared handler context passed to extracted edit tool free functions.
///
/// Bundles the state extracted from `CodeAnalyzer` so the extracted functions
/// stay within clippy's `too_many_arguments` limit while keeping parameters explicit.
pub(crate) struct EditHandlerContext<'a> {
    pub(crate) sid: Option<String>,
    pub(crate) seq: u32,
    pub(crate) cache: &'a AnalysisCache,
    pub(crate) metrics_tx: &'a crate::metrics::MetricsSender,
    pub(crate) edit_failure_counts: &'a Arc<Mutex<HashMap<(String, String), u8>>>,
}

/// Shared handler context passed to extracted `analyze_directory` free functions.
///
/// Bundles the `CodeAnalyzer` fields needed by `handle_overview_mode` and
/// `analyze_directory_handler`, keeping them explicit without coupling to `&self`.
pub(crate) struct AnalyzeDirectoryContext {
    pub(crate) cache: AnalysisCache,
    pub(crate) disk_cache: Arc<aptu_coder_core::cache::DiskCache>,
    pub(crate) metrics_tx: crate::metrics::MetricsSender,
    // Retained for log-level notification infrastructure (separate active feature).
    #[allow(dead_code)]
    pub(crate) peer: Arc<tokio::sync::Mutex<Option<rmcp::Peer<rmcp::RoleServer>>>>,
    pub(crate) sid: Option<String>,
}

/// Per-call metadata passed to `analyze_directory_handler`.
///
/// Bundles the call-site values (timing, identity, request context) so the
/// handler stays within `clippy::too_many_arguments` (7 args max).
pub(crate) struct DirectoryHandlerCall {
    pub(crate) seq: u32,
    pub(crate) sid: Option<String>,
    pub(crate) t_start: std::time::Instant,
    pub(crate) param_path: String,
    pub(crate) max_depth_val: Option<u32>,
    pub(crate) ct: tokio_util::sync::CancellationToken,
}

/// Shared handler context passed to extracted `analyze_file` free functions.
///
/// Bundles the `CodeAnalyzer` fields needed by `handle_file_details_mode` and
/// `analyze_file_handler`, keeping them explicit without coupling to `&self`.
pub(crate) struct AnalyzeFileContext {
    pub(crate) cache: AnalysisCache,
    pub(crate) disk_cache: Arc<aptu_coder_core::cache::DiskCache>,
    pub(crate) metrics_tx: crate::metrics::MetricsSender,
    pub(crate) sid: Option<String>,
}

/// Per-call metadata passed to `analyze_symbol_handler`.
///
/// Bundles the call-site values so the handler stays within
/// `clippy::too_many_arguments` (7 args max).
pub(crate) struct AnalyzeSymbolCall {
    pub(crate) ct: tokio_util::sync::CancellationToken,
    pub(crate) param_path: String,
    pub(crate) max_depth_val: Option<u32>,
    pub(crate) span: tracing::Span,
    pub(crate) t_start: std::time::Instant,
}
