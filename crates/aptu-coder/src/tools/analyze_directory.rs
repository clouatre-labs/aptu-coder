//! Extracted handler logic for the `analyze_directory` MCP tool.
//!
//! The `#[tool(...)]`-decorated method and `#[instrument]` outer decorator
//! remain in `lib.rs` as a thin shim. This module contains the free functions
//! that implement the actual logic, following the extraction pattern documented
//! in `tools/mod.rs`.

use aptu_coder_core::analyze;
use aptu_coder_core::cache::{CacheTier, DirectoryCacheKey};
use aptu_coder_core::formatter::{format_structure_paginated, format_summary};
use aptu_coder_core::pagination::{DEFAULT_PAGE_SIZE, PaginationMode, decode_cursor};
use aptu_coder_core::traversal::{
    WalkEntry, changed_files_from_git_ref, filter_entries_by_git_ref, walk_directory,
};
use aptu_coder_core::types::{AnalysisMode, AnalyzeDirectoryParams};
use rmcp::model::{CallToolResult, Content, ErrorData, ProgressToken};
use rmcp::{Peer, RoleServer};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::instrument;

use crate::SIZE_LIMIT;
use crate::tools::common::{
    err_to_tool_result, error_meta, no_cache_meta, summary_cursor_conflict,
};
use crate::tools::{AnalyzeDirectoryContext, DirectoryHandlerCall};

/// Emit a progress notification to the MCP client peer.
async fn emit_progress_notification(
    peer: Option<Peer<RoleServer>>,
    token: &ProgressToken,
    progress: f64,
    total: f64,
    message: String,
) {
    if let Some(peer) = peer {
        let notification = rmcp::model::ServerNotification::ProgressNotification(
            rmcp::model::Notification::new(rmcp::model::ProgressNotificationParam {
                progress_token: token.clone(),
                progress,
                total: Some(total),
                message: Some(message),
            }),
        );
        if let Err(e) = peer.send_notification(notification).await {
            tracing::warn!("Failed to send progress notification: {}", e);
        }
    }
}

/// Core analysis logic for the `analyze_directory` tool (overview mode).
///
/// Checks L1/L2 caches, walks the directory, optionally filters by git ref,
/// spawns the blocking analysis task with progress tracking, and stores results.
#[instrument(skip(ctx, params, ct))]
pub(crate) async fn handle_overview_mode(
    ctx: &AnalyzeDirectoryContext,
    params: &AnalyzeDirectoryParams,
    ct: tokio_util::sync::CancellationToken,
    progress_token: Option<ProgressToken>,
) -> Result<(Arc<analyze::AnalysisOutput>, CacheTier), ErrorData> {
    let path = Path::new(&params.path);
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max_depth = params.max_depth;

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

    let canonical_max_depth = max_depth.and_then(|d| if d == 0 { None } else { Some(d) });
    let git_ref_val = params.git_ref.as_deref().filter(|s| !s.is_empty());
    let cache_key = DirectoryCacheKey::from_entries(
        &all_entries,
        canonical_max_depth,
        AnalysisMode::Overview,
        git_ref_val,
    );

    if let Some(cached) = ctx.cache.get_directory(&cache_key) {
        tracing::debug!(cache_hit = true, message = "returning cached result");
        return Ok((cached, CacheTier::L1Memory));
    }

    let root = Path::new(&params.path);
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

    if let Some(cached) = ctx
        .disk_cache
        .get::<analyze::AnalysisOutput>("analyze_directory", &disk_key)
    {
        let arc = Arc::new(cached);
        ctx.cache.put_directory(cache_key.clone(), arc.clone());
        return Ok((arc, CacheTier::L2Disk));
    }

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

    let subtree_counts = if max_depth.is_some_and(|d| d > 0) {
        Some(aptu_coder_core::traversal::subtree_counts_from_entries(
            path,
            &all_entries,
        ))
    } else {
        None
    };

    let entries: Vec<WalkEntry> = if let Some(depth) = max_depth
        && depth > 0
    {
        all_entries
            .into_iter()
            .filter(|e| e.depth <= depth as usize)
            .collect()
    } else {
        all_entries
    };

    let total_files = entries.iter().filter(|e| !e.is_dir).count();
    let path_owned = std::path::PathBuf::from(&params.path);
    let counter_clone = counter.clone();
    let ct_clone = ct.clone();

    let handle = tokio::task::spawn_blocking(move || {
        analyze::analyze_directory_with_progress(&path_owned, entries, counter_clone, ct_clone)
    });

    // Drive progress notifications while the blocking task runs.
    if let Some(ref token) = progress_token {
        let (tx, mut rx) = watch::channel(0usize);
        let peer = ctx.peer.lock().await.clone();
        let mut last_progress = 0usize;
        let mut cancelled = false;

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
                    break;
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
                                emit_progress_notification(
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
                        Err(_) => break,
                    }
                }
            }
            if handle.is_finished() {
                break;
            }
        }

        if !cancelled && total_files > 0 {
            emit_progress_notification(
                peer,
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
            let arc_output = Arc::new(output);
            ctx.cache.put_directory(cache_key, arc_output.clone());
            {
                let dc = ctx.disk_cache.clone();
                let k = disk_key;
                let v = arc_output.as_ref().clone();
                let spawn_handle = tokio::task::spawn_blocking(move || {
                    dc.put("analyze_directory", &k, &v);
                    dc.drain_write_failures()
                });
                let metrics_tx = ctx.metrics_tx.clone();
                let sid = ctx.sid.clone();
                tokio::spawn(async move {
                    if let Ok(failures) = spawn_handle.await
                        && failures > 0
                    {
                        tracing::warn!(
                            tool = "analyze_directory",
                            failures,
                            "L2 disk cache write failed"
                        );
                        metrics_tx.send(
                            crate::metrics::MetricEventBuilder::new("analyze_directory", "ok", 0)
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

/// Handler body for the `analyze_directory` MCP tool.
///
/// Called by the thin shim in `lib.rs` after parameter extraction and metric
/// emission. Applies summary/pagination logic and builds the `CallToolResult`.
#[instrument(skip(ctx, params, call, span))]
pub(crate) async fn analyze_directory_handler(
    ctx: &AnalyzeDirectoryContext,
    params: AnalyzeDirectoryParams,
    call: DirectoryHandlerCall,
    span: &tracing::Span,
) -> Result<CallToolResult, ErrorData> {
    let DirectoryHandlerCall {
        seq,
        sid,
        t_start,
        param_path,
        max_depth_val,
        ct,
        progress_token,
    } = call;
    let (arc_output, dir_cache_hit) =
        match handle_overview_mode(ctx, &params, ct, progress_token).await {
            Ok(v) => v,
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "internal_error");
                return Ok(err_to_tool_result(e));
            }
        };

    let mut output = match Arc::try_unwrap(arc_output) {
        Ok(owned) => owned,
        Err(arc) => (*arc).clone(),
    };

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

    let use_summary = if params.output_control.summary == Some(true) {
        true
    } else if params.output_control.summary == Some(false) {
        false
    } else {
        output.formatted.len() > SIZE_LIMIT
    };

    let use_paginated = params.output_control.summary == Some(false);

    if use_summary {
        output.formatted = format_summary(
            &output.entries,
            &output.files,
            params.max_depth,
            output.subtree_counts.as_deref(),
        );
    }

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

    let paginated = match aptu_coder_core::pagination::paginate_slice(
        &output.files,
        offset,
        page_size,
        PaginationMode::Default,
    ) {
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

    if use_paginated {
        output.next_cursor.clone_from(&paginated.next_cursor);
    } else {
        output.next_cursor = None;
    }

    let mut final_text = output.formatted.clone();
    if use_paginated && let Some(cursor) = paginated.next_cursor {
        final_text.push('\n');
        final_text.push_str("NEXT_CURSOR: ");
        final_text.push_str(&cursor);
    }

    tracing::Span::current().record("cache_tier", dir_cache_hit.as_str());

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
    ctx.metrics_tx.send(
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
