//! Extracted handler logic for the `analyze_file` MCP tool.
//!
//! The `#[tool(...)]`-decorated method and `#[instrument]` outer decorator
//! remain in `lib.rs` as a thin shim. This module contains the free functions
//! that implement the actual logic, following the extraction pattern documented
//! in `tools/mod.rs`.

use aptu_coder_core::analyze;
use aptu_coder_core::cache::{CacheKey, CacheTier};
use aptu_coder_core::formatter::{format_file_details_paginated, format_file_details_summary};

/// Fixed server-side page size for analyze_file. Clients cannot override it.
const ANALYZE_FILE_PAGE_SIZE: usize = 50;
use aptu_coder_core::parser::ParserError;
use aptu_coder_core::types::{AnalysisMode, AnalyzeFileParams, FunctionInfo};
use rmcp::model::{Annotations, CallToolResult, ContentBlock, ErrorData, TextContent};
use serde_json::Value;
use std::sync::Arc;
use tracing::instrument;

use crate::SIZE_LIMIT;
use crate::tools::AnalyzeFileContext;
use crate::tools::common::{
    err_to_tool_result, error_meta, no_cache_meta, normalize_cursor, summary_cursor_conflict,
};

/// Core analysis logic for the `analyze_file` tool (file details mode).
///
/// Checks L1/L2 caches, runs file analysis on a cache miss, and stores results.
#[instrument(skip(ctx, params))]
pub(crate) async fn handle_file_details_mode(
    ctx: &AnalyzeFileContext,
    params: &AnalyzeFileParams,
) -> Result<(Arc<analyze::FileAnalysisOutput>, CacheTier), ErrorData> {
    let cache_key = std::fs::metadata(&params.path).ok().and_then(|meta| {
        meta.modified().ok().map(|mtime| CacheKey {
            path: std::path::PathBuf::from(&params.path),
            modified: mtime,
            mode: AnalysisMode::FileDetails,
        })
    });

    if let Some(ref key) = cache_key
        && let Some(cached) = ctx.cache.get(key)
    {
        tracing::debug!(cache_hit = true, message = "returning cached result");
        return Ok((cached, CacheTier::L1Memory));
    }

    let file_bytes = std::fs::read(&params.path).unwrap_or_default();
    let disk_key = blake3::hash(&file_bytes);

    if let Some(cached) = ctx
        .disk_cache
        .get::<analyze::FileAnalysisOutput>("analyze_file", &disk_key)
    {
        let arc = Arc::new(cached);
        if let Some(ref key) = cache_key {
            ctx.cache.put(key.clone(), arc.clone());
        }
        return Ok((arc, CacheTier::L2Disk));
    }

    match analyze::analyze_file(&params.path, None) {
        Ok(output) => {
            let arc_output = Arc::new(output);
            if let Some(key) = cache_key {
                ctx.cache.put(key, arc_output.clone());
            }
            {
                let dc = ctx.disk_cache.clone();
                let k = disk_key;
                let v = arc_output.as_ref().clone();
                let handle = tokio::task::spawn_blocking(move || {
                    dc.put("analyze_file", &k, &v);
                    dc.drain_write_failures()
                });
                let metrics_tx = ctx.metrics_tx.clone();
                let sid = ctx.sid.clone();
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
                    params.path.clone(),
                    formatted,
                    aptu_coder_core::types::SemanticAnalysis::default(),
                    line_count,
                    None,
                );
                let _ = ext;
                let mut output = output;
                output.unsupported = Some(true);
                Ok((Arc::new(output), CacheTier::Miss))
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

/// Emit a terminal `result="error"` metric on an `invalid_params` early return
/// so the invocation does not disappear from the shutdown summary call_count.
/// `error_type` reflects the failure class: `invalid_params` for validation
/// failures, `internal_error` for read/pagination failures.
#[allow(clippy::too_many_arguments)]
fn emit_validation_error(
    ctx: &AnalyzeFileContext,
    params: &AnalyzeFileParams,
    seq: u32,
    sid: &Option<String>,
    t_start: std::time::Instant,
    param_path: &str,
    cursor: Option<&str>,
    error_type: &str,
) {
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("analyze_file", "error", dur)
            .param_path_depth(crate::metrics::path_component_count(param_path))
            .error_type(Some(error_type.to_string()))
            .session_id(sid.clone())
            .seq(Some(seq))
            .file_ext(crate::metrics::path_file_ext(param_path))
            .language(crate::metrics::path_language(param_path))
            .fields_projected(params.fields.is_some())
            .summary_mode(params.output_control.summary.unwrap_or(false))
            .is_paginated(cursor.is_some())
            .build(),
    );
}

/// Handler body for the `analyze_file` MCP tool.
///
/// Called by the thin shim in `lib.rs` after parameter extraction and metric
/// emission. Applies summary/pagination logic and builds the `CallToolResult`.
#[instrument(skip(ctx, params, span))]
pub(crate) async fn analyze_file_handler(
    ctx: &AnalyzeFileContext,
    params: AnalyzeFileParams,
    seq: u32,
    sid: Option<String>,
    t_start: std::time::Instant,
    param_path: String,
    span: &tracing::Span,
) -> Result<CallToolResult, ErrorData> {
    let cursor = normalize_cursor(params.pagination.cursor.as_deref());

    if std::path::Path::new(&params.path).is_dir() {
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
            "path is a directory; use analyze_directory instead",
            {
                let mut meta = error_meta("validation", false, "pass a file path, not a directory");
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("path".to_string(), serde_json::json!(params.path));
                }
                Some(meta)
            },
        )));
    }

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

    let (arc_output, file_cache_hit) = match handle_file_details_mode(ctx, &params).await {
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
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("analyze_file", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(error_type)
                    .session_id(sid.clone())
                    .seq(Some(seq))
                    .file_ext(crate::metrics::path_file_ext(&param_path))
                    .language(crate::metrics::path_language(&param_path))
                    .fields_projected(params.fields.is_some())
                    .summary_mode(params.output_control.summary.unwrap_or(false))
                    .is_paginated(cursor.is_some())
                    .build(),
            );
            return Ok(err_to_tool_result(e));
        }
    };

    let mut formatted = arc_output.formatted.clone();
    let line_count = arc_output.line_count;

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
            message,
            Some(error_meta(
                "validation",
                false,
                "use force=true, fields, or summary=true",
            )),
        )));
    }

    let page_size = ANALYZE_FILE_PAGE_SIZE;
    let offset = match super::common::decode_offset(cursor) {
        Ok(o) => o,
        Err(e) => {
            return super::common::emit_internal_error(span, "invalid_params", e, |error_type| {
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
            });
        }
    };

    let top_level_fns: Vec<FunctionInfo> = arc_output
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

    let paginated =
        match super::common::paginate_or_internal_error(&top_level_fns, offset, page_size) {
            Ok(v) => v,
            Err(e) => {
                return super::common::emit_internal_error(
                    span,
                    "internal_error",
                    e,
                    |error_type| {
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
                    },
                );
            }
        };

    let is_unsupported_fallback = arc_output
        .formatted
        .contains("[Unsupported extension: semantic analysis not available]");
    if !use_summary && !is_unsupported_fallback {
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

    let next_cursor = if use_summary {
        None
    } else {
        paginated.next_cursor.clone()
    };

    let mut final_text = formatted.clone();
    if !use_summary && let Some(ref cursor) = next_cursor {
        final_text.push('\n');
        final_text.push_str("NEXT_CURSOR: ");
        final_text.push_str(cursor);
    }

    let response_output = analyze::FileAnalysisOutput::new(
        params.path.clone(),
        formatted,
        arc_output.semantic.project(params.fields.as_deref()),
        line_count,
        next_cursor,
    );

    tracing::Span::current().record("cache_tier", file_cache_hit.as_str());

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
    let structured = serde_json::to_value(&response_output).unwrap_or(Value::Null);
    result.structured_content = Some(structured);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("analyze_file", "ok", dur)
            .output_chars(final_text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .session_id(sid)
            .seq(Some(seq))
            .cache_hit(Some(file_cache_hit != CacheTier::Miss))
            .cache_tier(Some(file_cache_hit.as_str()))
            .file_ext(crate::metrics::path_file_ext(&param_path))
            .language(crate::metrics::path_language(&param_path))
            .fields_projected(params.fields.is_some())
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

    /// Builds a minimal `AnalyzeFileContext` backed by an unbounded metrics
    /// channel, returning the context and the receiving end so tests can inspect
    /// emitted events.
    fn test_context() -> (
        AnalyzeFileContext,
        tokio::sync::mpsc::UnboundedReceiver<crate::metrics::MetricEvent>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = AnalyzeFileContext {
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

    #[tokio::test]
    async fn emit_validation_error_internal_error_sends_terminal_event() {
        // Arrange: covers the paginate_slice / internal failure classification
        // where the handler returns without an "ok" completion event.
        let (ctx, mut rx) = test_context();
        let params: AnalyzeFileParams = serde_json::from_value(serde_json::json!({
            "path": "src/lib.rs",
        }))
        .expect("valid AnalyzeFileParams JSON");

        // Act
        emit_validation_error(
            &ctx,
            &params,
            0,
            &None,
            std::time::Instant::now(),
            "src/lib.rs",
            None,
            "internal_error",
        );

        // Assert
        let event = rx.try_recv().expect("expected a terminal error metric");
        assert_eq!(event.result, "error");
        assert_eq!(event.error_type.as_deref(), Some("internal_error"));
    }
}
