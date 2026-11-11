// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Extracted handler for the `edit_overwrite` MCP tool.
//!
//! See `tools/mod.rs` for the extraction pattern rules.

use aptu_coder_core::types::{EditOverwriteOutput, EditOverwriteParams};
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use tracing::instrument;

use crate::tools::EditHandlerContext;
use crate::tools::common::{err_to_tool_result, error_meta, no_cache_meta};
use crate::validation::{canonical_cwd, validate_path_relative_to};

#[instrument(skip(params, ctx, span, t_start), fields(path = %params.path))]
pub(crate) async fn edit_overwrite(
    params: EditOverwriteParams,
    ctx: EditHandlerContext<'_>,
    span: &tracing::Span,
    t_start: std::time::Instant,
) -> Result<CallToolResult, ErrorData> {
    span.record("gen_ai.tool.name", "edit_overwrite");
    span.record("path", &params.path);

    let working_dir_used = params.working_dir.is_some();

    let resolved_path: std::path::PathBuf = if let Some(ref wd) = params.working_dir {
        match validate_path_relative_to(&params.path, false, std::path::Path::new(wd)) {
            Ok(p) => p,
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                return Ok(err_to_tool_result(e));
            }
        }
    } else {
        let resolved =
            canonical_cwd().and_then(|cwd| validate_path_relative_to(&params.path, false, &cwd));
        match resolved {
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
            crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .error_type(Some("invalid_params".to_string()))
                .error_subtype(Some("directory_path".to_string()))
                .session_id(ctx.sid.clone())
                .seq(Some(ctx.seq))
                .working_dir_used(working_dir_used)
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

    let output: EditOverwriteOutput = match handle.await {
        Ok(Ok(v)) => v,
        Ok(Err(aptu_coder_core::EditError::NotAFile(_))) => {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .error_subtype(Some("not_a_file".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
                    .working_dir_used(working_dir_used)
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
                crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("internal_error".to_string()))
                    .error_subtype(Some("io_error".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
                    .working_dir_used(working_dir_used)
                    .build(),
            );
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                "I/O error writing file; check file path and permissions".to_string(),
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
                crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("internal_error".to_string()))
                    .error_subtype(Some("unexpected".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
                    .working_dir_used(working_dir_used)
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
                crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("internal_error".to_string()))
                    .error_subtype(Some("unexpected".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
                    .working_dir_used(working_dir_used)
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
    let result = CallToolResult::success(vec![ContentBlock::text(text.clone())])
        .with_meta(Some(no_cache_meta()));
    ctx.cache
        .invalidate_file(&std::path::PathBuf::from(&param_path));
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    // Reset circuit breaker on successful write
    {
        let sid_str = ctx.sid.clone().unwrap_or_default();
        let canonical = output.path.clone();
        let mut counts =
            super::common::lock_or_recover(ctx.edit_failure_counts, "edit_failure_counts");
        counts.remove(&(sid_str, canonical));
    }

    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("edit_overwrite", "ok", dur)
            .output_chars(text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .session_id(ctx.sid)
            .seq(Some(ctx.seq))
            .working_dir_used(working_dir_used)
            .build(),
    );
    Ok(result)
}

/// Error-subtype telemetry contract: every `edit_overwrite` error metric event
/// must carry a non-null `error_subtype`.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod error_subtype_tests {
    use super::*;

    #[tokio::test]
    async fn edit_overwrite_directory_error_event_carries_non_null_subtype() {
        // Arrange: target a directory so the handler hits the directory guard.
        let dir = tempfile::TempDir::new().unwrap();
        let cache = aptu_coder_core::cache::AnalysisCache::new(4);
        let counts = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let locks: crate::tools::FileEditLockRegistry =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(crate::metrics::METRICS_CHANNEL_CAPACITY);
        let metrics_tx = crate::metrics::MetricsSender(tx);
        let ctx = EditHandlerContext {
            sid: Some("test-session".to_string()),
            seq: 1,
            cache: &cache,
            metrics_tx: &metrics_tx,
            edit_failure_counts: &counts,
            file_edit_locks: &locks,
        };
        let params: EditOverwriteParams = serde_json::from_value(serde_json::json!({
            "path": dir.path().to_str().unwrap(),
            "content": "x",
        }))
        .unwrap();
        let span = tracing::Span::none();

        // Act
        let result = edit_overwrite(params, ctx, &span, std::time::Instant::now())
            .await
            .unwrap();

        // Assert
        assert!(result.is_error.is_some_and(|e| e));
        let event = rx.try_recv().unwrap();
        assert_eq!(event.result, "error");
        assert!(event.error_subtype.is_some(), "subtype must be non-null");
    }
}
