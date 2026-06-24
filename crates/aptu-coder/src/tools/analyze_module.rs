//! Extracted handler logic for the `analyze_module` MCP tool.
//!
//! The `#[tool(...)]`-decorated method and `#[instrument]` outer decorator
//! remain in `lib.rs` as a thin shim. This module contains the free functions
//! that implement the actual logic, following the extraction pattern documented
//! in `tools/mod.rs`.

use aptu_coder_core::analyze;
use aptu_coder_core::cache::CacheTier;
use aptu_coder_core::formatter::format_module_info;
use aptu_coder_core::types::{AnalyzeModuleParams, ModuleInfo};
use rmcp::model::{CallToolResult, Content, ErrorData, Meta};
use std::sync::Arc;
use tracing::instrument;

use crate::tools::common::{err_to_tool_result, error_meta, no_cache_meta};

/// Shared handler context passed to extracted `analyze_module` free functions.
///
/// Bundles the `CodeAnalyzer` fields needed by the handler, keeping them
/// explicit without coupling to `&self`.
pub(crate) struct AnalyzeModuleContext {
    pub(crate) disk_cache: Arc<aptu_coder_core::cache::DiskCache>,
    pub(crate) metrics_tx: crate::metrics::MetricsSender,
    pub(crate) sid: Option<String>,
    pub(crate) seq: u32,
}

/// Main handler for the `analyze_module` tool.
///
/// Called from the thin shim in `lib.rs` after the preamble (emit_received_metric,
/// trace context, span recording, validate_path) has been completed.
#[instrument(skip(ctx, params, span))]
pub(crate) async fn analyze_module_handler(
    ctx: AnalyzeModuleContext,
    params: AnalyzeModuleParams,
    param_path: String,
    span: &tracing::Span,
    t_start: std::time::Instant,
) -> Result<CallToolResult, ErrorData> {
    let sid = ctx.sid.clone();
    let seq = ctx.seq;

    // Issue 340: Guard against directory paths
    if std::fs::metadata(&params.path)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        ctx.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("analyze_module", "error", dur)
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .error_type(Some("invalid_params".to_string()))
                .session_id(sid.clone())
                .seq(Some(seq))
                .build(),
        );
        return Ok(err_to_tool_result(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is a directory; use analyze_directory for directories, or pass a file path to analyze_module",
            {
                let mut meta =
                    error_meta("validation", false, "use analyze_directory for directories");
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("path".to_string(), serde_json::json!(params.path));
                }
                Some(meta)
            },
        )));
    }

    // Module-only cache path: L2 (content hash) -> analyze_module_file fast path.
    // Uses AnalysisMode::ModuleOnly disk key so entries are distinct from analyze_file.
    // L1 in-memory cache is not used here: the existing L1 stores Arc<FileAnalysisOutput>
    // and adding a new typed slot is out of scope; L2 avoids the parse cost across restarts.
    let file_bytes = match tokio::fs::read(&params.path).await {
        Ok(b) => b,
        Err(_e) => {
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("analyze_module", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("internal_error".to_string()))
                    .session_id(sid.clone())
                    .seq(Some(seq))
                    .file_ext(crate::metrics::path_file_ext(&param_path))
                    .language(crate::metrics::path_language(&param_path))
                    .build(),
            );
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                "failed to read file; check file path and permissions",
                {
                    let mut meta = error_meta("resource", false, "check file path and permissions");
                    if let Some(obj) = meta.as_object_mut() {
                        obj.insert("path".to_string(), serde_json::json!(params.path));
                    }
                    Some(meta)
                },
            )));
        }
    };
    let disk_key = blake3::hash(&file_bytes);

    let (module_info, module_tier) = if let Some(cached) = ctx
        .disk_cache
        .get::<ModuleInfo>("analyze_module", &disk_key)
    {
        (cached, CacheTier::L2Disk)
    } else {
        // Cache miss: run the lightweight fast path
        let mi = match analyze::analyze_module_file(&params.path) {
            Ok(mi) => mi,
            Err(e) => {
                let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                // Graceful fallback for unsupported extensions: return empty ModuleInfo
                // with a note instead of INVALID_PARAMS.
                if matches!(
                    &e,
                    analyze::AnalyzeError::Parser(
                        aptu_coder_core::parser::ParserError::UnsupportedLanguage(_)
                    )
                ) {
                    let source = String::from_utf8_lossy(&file_bytes).into_owned();
                    let line_count = source.lines().count();
                    let name = std::path::Path::new(&params.path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    let ext = std::path::Path::new(&params.path)
                        .extension()
                        .and_then(|x| x.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    ctx.metrics_tx.send(
                        crate::metrics::MetricEventBuilder::new("analyze_module", "ok", dur)
                            .param_path_depth(crate::metrics::path_component_count(&param_path))
                            .session_id(sid.clone())
                            .seq(Some(seq))
                            .file_ext(crate::metrics::path_file_ext(&param_path))
                            .language(crate::metrics::path_language(&param_path))
                            .build(),
                    );
                    return {
                        let mut mi = ModuleInfo::new(name, line_count, ext, vec![], vec![]);
                        mi.unsupported = Some(true);
                        let text = format_module_info(&mi);
                        let content_hash = format!("{}", blake3::hash(text.as_bytes()));
                        let mut meta = no_cache_meta().0;
                        meta.insert(
                            "content_hash".to_string(),
                            serde_json::Value::String(content_hash),
                        );
                        let mut result = CallToolResult::success(vec![Content::text(text)])
                            .with_meta(Some(Meta(meta)));
                        match serde_json::to_value(&mi) {
                            Ok(v) => {
                                result.structured_content = Some(v);
                                Ok(result)
                            }
                            Err(se) => Ok(err_to_tool_result(ErrorData::new(
                                rmcp::model::ErrorCode::INTERNAL_ERROR,
                                format!("serialization failed: {se}"),
                                Some(error_meta("internal", false, "report this as a bug")),
                            ))),
                        }
                    };
                }
                let (error_type, error_data) = (
                    Some("internal_error".to_string()),
                    ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        format!("Failed to analyze module: {e}"),
                        Some(error_meta("internal", false, "report this as a bug")),
                    ),
                );
                ctx.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("analyze_module", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(&param_path))
                        .error_type(error_type)
                        .session_id(sid.clone())
                        .seq(Some(seq))
                        .file_ext(crate::metrics::path_file_ext(&param_path))
                        .language(crate::metrics::path_language(&param_path))
                        .build(),
                );
                return Ok(err_to_tool_result(error_data));
            }
        };
        // Write-behind: store ModuleInfo in L2 disk cache
        {
            let dc = ctx.disk_cache.clone();
            let k = disk_key;
            let mi_clone = mi.clone();
            let metrics_tx2 = ctx.metrics_tx.clone();
            let sid2 = sid.clone();
            tokio::spawn(async move {
                let handle = tokio::task::spawn_blocking(move || {
                    dc.put("analyze_module", &k, &mi_clone);
                    dc.drain_write_failures()
                });
                if let Ok(failures) = handle.await
                    && failures > 0
                {
                    tracing::warn!(
                        tool = "analyze_module",
                        failures,
                        "L2 disk cache write failed"
                    );
                    metrics_tx2.send(
                        crate::metrics::MetricEventBuilder::new("analyze_module", "ok", 0)
                            .session_id(sid2)
                            .cache_write_failure(Some(true))
                            .build(),
                    );
                }
            });
        }
        (mi, CacheTier::Miss)
    };

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
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("analyze_module", "ok", dur)
            .output_chars(text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .session_id(sid)
            .seq(Some(seq))
            .cache_hit(Some(module_tier != CacheTier::Miss))
            .cache_tier(Some(module_tier.as_str()))
            .file_ext(crate::metrics::path_file_ext(&param_path))
            .language(crate::metrics::path_language(&param_path))
            .build(),
    );
    Ok(result)
}
