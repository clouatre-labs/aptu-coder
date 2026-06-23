#![allow(unused_imports)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::cast_precision_loss)]
use crate::CodeAnalyzer;
use crate::filters::{CompiledRule, apply_filter, load_filter_table, maybe_inject_no_stat};
use crate::logging::LogEvent;
use crate::shell::resolve_shell;
use crate::tools::common::{
    ClientMetadata, FocusedAnalysisParams, SIZE_LIMIT, disable_routes, err_to_tool_result,
    err_to_tool_result_from_pagination, error_meta, extract_and_set_trace_context, no_cache_meta,
    paginate_focus_chains, summary_cursor_conflict,
};
use crate::validation::{validate_path, validate_path_in_dir};
use aptu_coder_core::analyze;
use aptu_coder_core::cache::{AnalysisCache, CacheTier, CallGraphCache, CallGraphCacheKey};
use aptu_coder_core::formatter::{
    format_file_details_paginated, format_file_details_summary, format_focused_paginated,
    format_module_info, format_structure_paginated, format_summary,
};
use aptu_coder_core::formatter_defuse::format_focused_paginated_defuse;
use aptu_coder_core::pagination::{
    CursorData, DEFAULT_PAGE_SIZE, PaginationMode, decode_cursor, encode_cursor, paginate_slice,
};
use aptu_coder_core::parser::ParserError;
use aptu_coder_core::traversal::{
    WalkEntry, changed_files_from_git_ref, filter_entries_by_git_ref, walk_directory,
};
use aptu_coder_core::types::{
    AnalysisMode, AnalyzeDirectoryParams, AnalyzeFileParams, AnalyzeModuleParams,
    AnalyzeSymbolParams, EditOverwriteOutput, EditOverwriteParams, EditReplaceOutput,
    EditReplaceParams, SymbolMatchMode,
};
use aptu_coder_core::{cache, completion, graph, traversal, types};
use rmcp::handler::server::tool::{ToolRouter, schema_for_type};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, CancelledNotificationParam, CompleteRequestParams, CompleteResult,
    CompletionInfo, Content, ErrorData, Implementation, InitializeRequestParams, InitializeResult,
    LoggingLevel, LoggingMessageNotificationParam, Meta, Notification, ProgressNotificationParam,
    ProgressToken, ServerCapabilities, ServerNotification, SetLevelRequestParams,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{Peer, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as TokioMutex, RwLock, mpsc, watch};
use tracing::{instrument, warn};
use tracing_subscriber::filter::LevelFilter;

pub(crate) async fn analyze_module_impl(
    analyzer: &CodeAnalyzer,
    params: Parameters<AnalyzeModuleParams>,
    context: RequestContext<RoleServer>,
) -> Result<CallToolResult, ErrorData> {
    let params = params.0;
    let t_start = std::time::Instant::now();
    let (seq, sid) = analyzer.emit_received_metric("analyze_module").await;
    // Extract W3C Trace Context from request _meta if present
    let session_id = analyzer.session_id.lock().await.clone();
    let client_name = analyzer.client_name.lock().await.clone();
    let client_version = analyzer.client_version.lock().await.clone();
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

    // Issue 340: Guard against directory paths
    if std::fs::metadata(&params.path)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        analyzer.metrics_tx.send(
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
            analyzer.metrics_tx.send(
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

    let (module_info, module_tier) = if let Some(cached) = analyzer
        .disk_cache
        .get::<types::ModuleInfo>("analyze_module", &disk_key)
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
                    analyzer.metrics_tx.send(
                        crate::metrics::MetricEventBuilder::new("analyze_module", "ok", dur)
                            .param_path_depth(crate::metrics::path_component_count(&param_path))
                            .session_id(sid.clone())
                            .seq(Some(seq))
                            .file_ext(crate::metrics::path_file_ext(&param_path))
                            .language(crate::metrics::path_language(&param_path))
                            .build(),
                    );
                    return {
                        let mut mi = types::ModuleInfo::new(name, line_count, ext, vec![], vec![]);
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
                analyzer.metrics_tx.send(
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
            let dc = analyzer.disk_cache.clone();
            let k = disk_key;
            let mi_clone = mi.clone();
            let metrics_tx2 = analyzer.metrics_tx.clone();
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
    analyzer.metrics_tx.send(
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
