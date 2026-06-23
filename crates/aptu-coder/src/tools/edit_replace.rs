// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Extracted handler for the `edit_replace` MCP tool.
//!
//! See `tools/mod.rs` for the extraction pattern rules.

use aptu_coder_core::types::{EditReplaceOutput, EditReplaceParams};
use rmcp::model::{CallToolResult, Content, ErrorData};
use tracing::instrument;

use crate::{
    err_to_tool_result, error_meta, no_cache_meta,
    tools::EditHandlerContext,
    validation::{validate_path, validate_path_in_dir},
};

/// Number of consecutive not_found or ambiguous edit_replace failures on the same
/// (session_id, canonical_path) pair before returning a stale-context directive error.
pub(crate) const EDIT_STALE_THRESHOLD: u8 = 5;

/// Maximum number of (session_id, canonical_path) entries in the failure counter map.
/// When the map reaches this size, it is cleared entirely to prevent unbounded growth.
/// The circuit breaker is advisory, so a full clear is safe: the worst case is one
/// missed trip per session per path after an eviction cycle.
pub(crate) const EDIT_FAILURE_MAP_CAP: usize = 1024;

#[instrument(skip(params, ctx, span, t_start), fields(path = %params.path))]
pub(crate) async fn edit_replace(
    params: EditReplaceParams,
    ctx: EditHandlerContext<'_>,
    span: &tracing::Span,
    t_start: std::time::Instant,
) -> Result<CallToolResult, ErrorData> {
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
        ctx.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .error_type(Some("invalid_params".to_string()))
                .session_id(ctx.sid.clone())
                .seq(Some(ctx.seq))
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
        let sid_str = ctx.sid.clone().unwrap_or_default();
        let mut counts = ctx
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

    let output: EditReplaceOutput = match handle.await {
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
                ctx.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("invalid_params".to_string()))
                        .error_subtype(Some("stale_context".to_string()))
                        .session_id(ctx.sid.clone())
                        .seq(Some(ctx.seq))
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

            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .error_subtype(Some("not_found".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
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
                ctx.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(Some("invalid_params".to_string()))
                        .error_subtype(Some("stale_context".to_string()))
                        .session_id(ctx.sid.clone())
                        .seq(Some(ctx.seq))
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

            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .error_subtype(Some("ambiguous".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
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
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
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
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("internal_error".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
                    .build(),
            );
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                "I/O error editing file; check file path and permissions".to_string(),
                {
                    let mut meta = error_meta("resource", false, "check file path and permissions");
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
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("internal_error".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
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
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("internal_error".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
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
    let mut result =
        CallToolResult::success(vec![Content::text(text.clone())]).with_meta(Some(no_cache_meta()));
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
    ctx.cache
        .invalidate_file(&std::path::PathBuf::from(&param_path));
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    // Reset circuit breaker on successful edit
    {
        let sid_str = ctx.sid.clone().unwrap_or_default();
        let canonical = output.path.clone();
        let mut counts = ctx
            .edit_failure_counts
            .lock()
            .expect("edit_failure_counts poisoned");
        counts.remove(&(sid_str, canonical));
    }

    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("edit_replace", "ok", dur)
            .output_chars(text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .session_id(ctx.sid)
            .seq(Some(ctx.seq))
            .build(),
    );
    Ok(result)
}
