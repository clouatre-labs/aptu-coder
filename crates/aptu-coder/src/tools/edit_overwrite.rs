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
use crate::{EDIT_FAILURE_MAP_CAP, EDIT_STALE_THRESHOLD};
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

pub(crate) async fn edit_overwrite_impl(
    analyzer: &CodeAnalyzer,
    params: Parameters<EditOverwriteParams>,
    context: RequestContext<RoleServer>,
) -> Result<CallToolResult, ErrorData> {
    let params = params.0;
    let t_start = std::time::Instant::now();
    let (seq, sid) = analyzer.emit_received_metric("edit_overwrite").await;
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
    span.record("gen_ai.tool.name", "edit_overwrite");
    span.record("path", &params.path);
    let resolved_path: std::path::PathBuf = if let Some(ref wd) = params.working_dir {
        match validate_path_in_dir(&params.path, false, std::path::Path::new(wd)) {
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
        match validate_path(&params.path, false) {
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
        analyzer.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .error_type(Some("invalid_params".to_string()))
                .session_id(sid.clone())
                .seq(Some(seq))
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

    let output = match handle.await {
        Ok(Ok(v)) => v,
        Ok(Err(aptu_coder_core::EditError::NotAFile(_))) => {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            analyzer.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .session_id(sid.clone())
                    .seq(Some(seq))
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
            analyzer.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("internal_error".to_string()))
                    .session_id(sid.clone())
                    .seq(Some(seq))
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
            analyzer.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("internal_error".to_string()))
                    .session_id(sid.clone())
                    .seq(Some(seq))
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
            analyzer.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_overwrite", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(Some("internal_error".to_string()))
                    .session_id(sid.clone())
                    .seq(Some(seq))
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
    analyzer
        .cache
        .invalidate_file(&std::path::PathBuf::from(&param_path));
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    // Reset circuit breaker on successful write
    {
        let sid_str = sid.clone().unwrap_or_default();
        let canonical = output.path.clone();
        let mut counts = analyzer
            .edit_failure_counts
            .lock()
            .expect("edit_failure_counts poisoned");
        counts.remove(&(sid_str, canonical));
    }

    analyzer.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("edit_overwrite", "ok", dur)
            .output_chars(text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .session_id(sid)
            .seq(Some(seq))
            .build(),
    );
    Ok(result)
}
