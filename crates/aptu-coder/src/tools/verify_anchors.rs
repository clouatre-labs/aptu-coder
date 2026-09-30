// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Extracted handler for the `verify_anchors` MCP tool: validates the
//! workspace root and anchor paths before any fs access, then delegates
//! to the pure `aptu_coder_core::anchors` implementation.

use aptu_coder_core::anchors;
use aptu_coder_core::types::{VerifyAnchorsOutput, VerifyAnchorsParams};
use rmcp::model::{CallToolResult, ContentBlock, ErrorData, MetaObject};
use tracing::instrument;

use crate::tools::common::{err_to_tool_result, error_meta, no_cache_meta};

/// Shared handler context passed to the extracted `verify_anchors` free function.
pub(crate) struct VerifyAnchorsContext {
    pub(crate) metrics_tx: crate::metrics::MetricsSender,
    pub(crate) sid: Option<String>,
    pub(crate) seq: u32,
}

/// Record the failure span fields and error metric, then convert `err` into a tool result.
fn fail(
    ctx: &VerifyAnchorsContext,
    span: &tracing::Span,
    t_start: std::time::Instant,
    error_type: &str,
    err: ErrorData,
) -> Result<CallToolResult, ErrorData> {
    span.record("error", true);
    span.record("error.type", error_type);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("verify_anchors", "error", dur)
            .error_type(Some(error_type.to_string()))
            .session_id(ctx.sid.clone())
            .seq(Some(ctx.seq))
            .build(),
    );
    Ok(err_to_tool_result(err))
}

/// Main handler for the `verify_anchors` tool, called from the shim in `lib.rs`.
#[instrument(skip(ctx, params, span))]
pub(crate) async fn verify_anchors_handler(
    ctx: VerifyAnchorsContext,
    params: VerifyAnchorsParams,
    span: &tracing::Span,
    t_start: std::time::Instant,
) -> Result<CallToolResult, ErrorData> {
    let sid = ctx.sid.clone();
    let seq = ctx.seq;

    // Resolve the workspace root relative to the process CWD before any fs access.
    let root = match crate::validation::canonical_cwd().and_then(|cwd| {
        crate::validation::validate_path_relative_to(&params.workspace_root, true, &cwd)
    }) {
        Ok(p) => p,
        Err(e) => return fail(&ctx, span, t_start, "invalid_params", e),
    };

    // Validate each anchor path relative to the workspace root; reject escapes.
    for anchor in &params.anchors {
        let verdict = crate::validation::validate_path_relative_to(&anchor.path, true, &root)
            .and_then(|resolved| {
                if resolved.starts_with(&root) {
                    Ok(resolved)
                } else {
                    let mut meta = error_meta(
                        "validation",
                        false,
                        "provide anchor paths within the workspace root",
                    );
                    if let Some(obj) = meta.as_object_mut() {
                        obj.insert("path".to_string(), serde_json::json!(anchor.path));
                    }
                    Err(ErrorData::new(
                        rmcp::model::ErrorCode::INVALID_PARAMS,
                        format!("anchor path escapes the workspace root: {}", anchor.path),
                        Some(meta),
                    ))
                }
            });
        if let Err(e) = verdict {
            return fail(&ctx, span, t_start, "invalid_params", e);
        }
    }

    let output = {
        let root = root.clone();
        let anchors = params.anchors.clone();
        let handle = tokio::task::spawn_blocking(move || anchors::verify_anchors(&root, &anchors));
        match handle.await {
            Ok(out) => out,
            Err(e) => {
                return fail(
                    &ctx,
                    span,
                    t_start,
                    "internal_error",
                    ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        format!("anchor verification failed: {e}"),
                        Some(error_meta("internal", false, "report this as a bug")),
                    ),
                );
            }
        }
    };

    let verified = output
        .iter()
        .filter(|r| r.exists && r.in_range && r.symbol_found != Some(false))
        .count();
    let output = VerifyAnchorsOutput { results: output };
    let structured_value = match serde_json::to_value(&output).map_err(|e| {
        ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            format!("serialization failed: {e}"),
            Some(error_meta("internal", false, "report this as a bug")),
        )
    }) {
        Ok(v) => v,
        Err(e) => return Ok(err_to_tool_result(e)),
    };
    let text = format!("Verified {} of {} anchors", verified, output.results.len());
    let mut result = CallToolResult::success(vec![ContentBlock::text(text.clone())])
        .with_meta(Some(MetaObject(no_cache_meta().0)));
    result.structured_content = Some(structured_value);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("verify_anchors", "ok", dur)
            .output_chars(text.len())
            .session_id(sid)
            .seq(Some(seq))
            .build(),
    );
    Ok(result)
}
