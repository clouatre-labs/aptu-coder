//! Extracted server initialization, route management, and metric helpers.
//!
//! This module follows the extraction pattern documented in `tools/mod.rs`:
//! free functions that operate on explicit state rather than `&CodeAnalyzer`.
//!
//! **Architecture rule:** `#[tool(...)]`, `#[tool_router]`, `#[tool_handler]`,
//! `impl CodeAnalyzer`, and `impl ServerHandler for CodeAnalyzer` remain in
//! `lib.rs`. This module contains the extracted bodies called by thin shims.

use crate::filters::load_filter_table;
use crate::shell::resolve_shell;
use aptu_coder_core::cache::{AnalysisCache, CallGraphCache};
use rmcp::handler::server::tool::ToolRouter;
use rmcp::{Peer, RoleServer};

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as TokioMutex, RwLock};
use tracing::instrument;
use tracing_subscriber::filter::LevelFilter;

/// Builds a fully initialized `CodeAnalyzer`.
///
/// Contains the constructor logic previously in `CodeAnalyzer::new()`.
#[must_use]
pub(crate) fn build_analyzer(
    peer: Arc<TokioMutex<Option<Peer<RoleServer>>>>,
    log_level_filter: Arc<Mutex<LevelFilter>>,
    metrics_tx: crate::metrics::MetricsSender,
) -> crate::CodeAnalyzer {
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
    let disk_cache = std::sync::Arc::new(aptu_coder_core::cache::DiskCache::new(
        disk_cache_dir,
        disk_cache_disabled,
    ));

    // Snapshot login shell PATH once at startup
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
        let path = login_path.or_else(|| std::env::var("PATH").ok());
        Arc::new(path)
    };

    let filter_table = Arc::new(load_filter_table(Path::new(".")));

    crate::CodeAnalyzer {
        tool_router: Arc::new(RwLock::new(crate::CodeAnalyzer::tool_router())),
        cache: AnalysisCache::new(file_cap),
        disk_cache,
        peer,
        log_level_filter,
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

/// Handles the `on_initialized` server lifecycle event.
///
/// Contains the logic previously in `ServerHandler::on_initialized`.
#[instrument(skip(peer, tool_router))]
pub(crate) async fn on_initialized_impl(
    peer: Arc<TokioMutex<Option<Peer<RoleServer>>>>,
    session_id: Arc<TokioMutex<Option<String>>>,
    session_call_seq: Arc<std::sync::atomic::AtomicU32>,
    session_profile: Arc<std::sync::OnceLock<String>>,
    tool_router: Arc<RwLock<ToolRouter<crate::CodeAnalyzer>>>,
    context_peer: &Peer<RoleServer>,
) {
    {
        let mut peer_lock = peer.lock().await;
        *peer_lock = Some(context_peer.clone());
    }

    // Generate session_id in MILLIS-N format
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    let counter = crate::GLOBAL_SESSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let sid = format!("{millis}-{counter}");
    {
        let mut session_id_lock = session_id.lock().await;
        *session_id_lock = Some(sid);
    }
    session_call_seq.store(0, std::sync::atomic::Ordering::Relaxed);

    // NON-STANDARD VENDOR EXTENSION: profile-based tool filtering.
    let active_profile = session_profile
        .get()
        .cloned()
        .or_else(|| std::env::var("APTU_CODER_PROFILE").ok());

    {
        let mut router = tool_router.write().await;

        if let Some(ref profile) = active_profile {
            match profile.as_str() {
                "edit" => {
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
                    disable_routes(
                        &mut router,
                        &["edit_replace", "edit_overwrite", "exec_command"],
                    );
                }
                _ => {}
            }
        }

        router.bind_peer_notifier(context_peer);
    }
}

/// Disables the given tool routes on the router.
pub(crate) fn disable_routes(router: &mut ToolRouter<crate::CodeAnalyzer>, tools: &[&'static str]) {
    for tool in tools {
        router.disable_route(*tool);
    }
}

/// Emit a "received" metric event for the given tool name.
///
/// Increments the session call sequence, locks the session ID, and sends
/// the metric event via the channel. Returns the (seq, sid) pair for use
/// by the caller in exit metrics, preserving per-call seq uniqueness.
#[instrument(skip(metrics_tx, session_id, session_call_seq))]
pub(crate) async fn emit_received_metric(
    metrics_tx: &crate::metrics::MetricsSender,
    session_id: &TokioMutex<Option<String>>,
    session_call_seq: &std::sync::atomic::AtomicU32,
    tool: &'static str,
) -> (u32, Option<String>) {
    // Relaxed: per-session monotonic counter; unique allocation is all that is
    // needed. No cross-thread happens-before required. Contrast:
    // GLOBAL_SESSION_COUNTER uses SeqCst for cross-session uniqueness.
    let seq = session_call_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let sid = session_id.lock().await.clone();
    metrics_tx.send(crate::metrics::MetricEvent {
        tool,
        result: "received",
        session_id: sid.clone(),
        seq: Some(seq),
        duration_ms: 0,
        ..Default::default()
    });
    (seq, sid)
}

/// Delegates to [`crate::tools::analyze_directory::handle_overview_mode`].
#[cfg(test)]
pub(crate) async fn handle_overview_mode(
    ctx: &crate::tools::AnalyzeDirectoryContext,
    params: &aptu_coder_core::types::AnalyzeDirectoryParams,
    ct: tokio_util::sync::CancellationToken,
) -> Result<
    (
        std::sync::Arc<aptu_coder_core::analyze::AnalysisOutput>,
        aptu_coder_core::cache::CacheTier,
    ),
    rmcp::model::ErrorData,
> {
    crate::tools::analyze_directory::handle_overview_mode(ctx, params, ct).await
}

/// Delegates to [`crate::tools::analyze_file::handle_file_details_mode`].
#[cfg(test)]
pub(crate) async fn handle_file_details_mode(
    cache: AnalysisCache,
    disk_cache: std::sync::Arc<aptu_coder_core::cache::DiskCache>,
    metrics_tx: crate::metrics::MetricsSender,
    sid: Option<String>,
    params: &aptu_coder_core::types::AnalyzeFileParams,
) -> Result<
    (
        std::sync::Arc<aptu_coder_core::analyze::FileAnalysisOutput>,
        aptu_coder_core::cache::CacheTier,
    ),
    rmcp::model::ErrorData,
> {
    let ctx = crate::tools::AnalyzeFileContext {
        cache,
        disk_cache,
        metrics_tx,
        sid,
    };
    crate::tools::analyze_file::handle_file_details_mode(&ctx, params).await
}
