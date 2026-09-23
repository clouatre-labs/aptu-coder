//! Extracted handler logic for the `analyze_directory` MCP tool.
//!
//! The `#[tool(...)]`-decorated method and `#[instrument]` outer decorator
//! remain in `lib.rs` as a thin shim. This module contains the free functions
//! that implement the actual logic, following the extraction pattern documented
//! in `tools/mod.rs`.

use aptu_coder_core::analyze;
use aptu_coder_core::cache::{CacheTier, DirectoryCacheKey};
use aptu_coder_core::formatter::{format_structure_paginated, format_summary};
use aptu_coder_core::pagination::{PaginationMode, decode_cursor};

/// Fixed server-side page size for analyze_directory. Clients cannot override it.
const ANALYZE_DIRECTORY_PAGE_SIZE: usize = 50;
use aptu_coder_core::traversal::{
    WalkEntry, changed_files_from_git_ref, filter_entries_by_git_ref, walk_directory,
};
use aptu_coder_core::types::{AnalysisMode, AnalyzeDirectoryParams};
use rmcp::model::{Annotations, CallToolResult, ContentBlock, ErrorData, TextContent};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tracing::instrument;

use crate::SIZE_LIMIT;
use crate::tools::common::{
    err_to_tool_result, error_meta, no_cache_meta, normalize_cursor, summary_cursor_conflict,
};
use crate::tools::{AnalyzeDirectoryContext, DirectoryHandlerCall};

/// Applies an optional `git_ref` filter to the directory walk entries.
///
/// If `git_ref` is `None` or empty, `entries` is returned unchanged.
/// If `git_ref` is non-empty, calls `changed_files_from_git_ref` to resolve the
/// changed set and filters `entries` accordingly.
pub(crate) fn apply_git_ref_filter(
    path: &Path,
    entries: Vec<WalkEntry>,
    git_ref: Option<&str>,
) -> Result<Vec<WalkEntry>, ErrorData> {
    if let Some(git_ref) = git_ref
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
        Ok(filter_entries_by_git_ref(entries, &changed, path))
    } else {
        Ok(entries)
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

    let all_entries = apply_git_ref_filter(path, all_entries, params.git_ref.as_deref())?;

    let canonical_max_depth = max_depth.filter(|&d| d != 0);
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

    let path_owned = std::path::PathBuf::from(&params.path);
    let counter_clone = counter.clone();
    let ct_clone = ct.clone();

    let handle = tokio::task::spawn_blocking(move || {
        analyze::analyze_directory_with_progress(&path_owned, entries, counter_clone, ct_clone)
    });

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

/// Emit a terminal `result="error"` metric on an `invalid_params` early return
/// so the invocation does not disappear from the shutdown summary call_count.
/// `error_type` reflects the failure class: `invalid_params` for validation
/// failures, `internal_error` for walk/pagination failures.
#[allow(clippy::too_many_arguments)]
fn emit_validation_error(
    ctx: &AnalyzeDirectoryContext,
    params: &AnalyzeDirectoryParams,
    seq: u32,
    sid: &Option<String>,
    t_start: std::time::Instant,
    param_path: &str,
    cursor: Option<&str>,
    error_type: &str,
) {
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("analyze_directory", "error", dur)
            .param_path_depth(crate::metrics::path_component_count(param_path))
            .error_type(Some(error_type.to_string()))
            .session_id(sid.clone())
            .seq(Some(seq))
            .summary_mode(params.output_control.summary.unwrap_or(false))
            .is_paginated(cursor.is_some())
            .build(),
    );
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
    } = call;
    let cursor = normalize_cursor(params.pagination.cursor.as_deref());
    let (arc_output, dir_cache_hit) = match handle_overview_mode(ctx, &params, ct).await {
        Ok(v) => v,
        Err(e) => {
            span.record("error", true);
            let error_type = match e.code {
                rmcp::model::ErrorCode::INVALID_PARAMS => "invalid_params",
                _ => "internal_error",
            };
            span.record("error.type", error_type);
            emit_validation_error(
                ctx,
                &params,
                seq,
                &sid,
                t_start,
                &param_path,
                cursor,
                error_type,
            );
            return Ok(err_to_tool_result(e));
        }
    };

    let mut output = match Arc::try_unwrap(arc_output) {
        Ok(owned) => owned,
        Err(arc) => (*arc).clone(),
    };

    if summary_cursor_conflict(params.output_control.summary, cursor) {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        emit_validation_error(
            ctx,
            &params,
            seq,
            &sid,
            t_start,
            &param_path,
            cursor,
            "invalid_params",
        );
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

    let page_size = ANALYZE_DIRECTORY_PAGE_SIZE;
    let offset = if let Some(cursor_str) = cursor {
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
                emit_validation_error(
                    ctx,
                    &params,
                    seq,
                    &sid,
                    t_start,
                    &param_path,
                    cursor,
                    "invalid_params",
                );
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
            emit_validation_error(
                ctx,
                &params,
                seq,
                &sid,
                t_start,
                &param_path,
                cursor,
                "internal_error",
            );
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
    let meta = rmcp::model::MetaObject(meta);

    let mut result = CallToolResult::success(vec![ContentBlock::Text(
        TextContent::new(final_text.clone())
            .with_annotations(Annotations::default().with_priority(0.9_f32)),
    )])
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
            .git_ref_used(params.git_ref.is_some())
            .summary_mode(use_summary)
            .is_paginated(cursor.is_some())
            .build(),
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aptu_coder_core::cache::AnalysisCache;

    /// Builds a minimal `AnalyzeDirectoryContext` backed by an unbounded metrics
    /// channel, returning the context and the receiving end so tests can inspect
    /// emitted events.
    fn test_context() -> (
        AnalyzeDirectoryContext,
        tokio::sync::mpsc::UnboundedReceiver<crate::metrics::MetricEvent>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = AnalyzeDirectoryContext {
            cache: AnalysisCache::new(1),
            disk_cache: Arc::new(aptu_coder_core::cache::DiskCache::new(
                std::path::PathBuf::new(),
                true,
            )),
            metrics_tx: crate::metrics::MetricsSender(tx),
            sid: None,
        };
        (ctx, rx)
    }

    fn test_call(param_path: String) -> DirectoryHandlerCall {
        DirectoryHandlerCall {
            seq: 0,
            sid: None,
            t_start: std::time::Instant::now(),
            param_path,
            max_depth_val: None,
            ct: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn handler_git_ref_failure_emits_single_terminal_error_event() {
        // Arrange: a non-git directory with a git_ref filter forces
        // handle_overview_mode to fail (walk succeeds, git_ref filter fails).
        let (ctx, mut rx) = test_context();
        let dir = tempfile::TempDir::new().expect("create temp dir");
        std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").expect("write temp file");
        let path = dir.path().to_str().expect("valid utf8 path").to_string();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": path,
            "max_depth": 1,
            "git_ref": "nonexistent-ref",
        }))
        .expect("valid AnalyzeDirectoryParams JSON");
        let call = test_call(path.clone());

        // Act
        let _ = analyze_directory_handler(&ctx, params, call, &tracing::Span::none()).await;

        // Assert: exactly one terminal event, paired with the implicit receipt.
        let event = rx.try_recv().expect("expected a terminal error metric");
        assert_eq!(event.result, "error");
        assert_eq!(event.error_type.as_deref(), Some("invalid_params"));
        assert!(
            rx.try_recv().is_err(),
            "no additional events should be emitted"
        );
    }

    #[tokio::test]
    async fn emit_validation_error_internal_error_sends_terminal_event() {
        // Arrange: covers the paginate_slice / internal failure classification.
        let (ctx, mut rx) = test_context();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": "src",
        }))
        .expect("valid AnalyzeDirectoryParams JSON");

        // Act
        emit_validation_error(
            &ctx,
            &params,
            0,
            &None,
            std::time::Instant::now(),
            "src",
            None,
            "internal_error",
        );

        // Assert
        let event = rx.try_recv().expect("expected a terminal error metric");
        assert_eq!(event.result, "error");
        assert_eq!(event.error_type.as_deref(), Some("internal_error"));
    }
}
