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

pub(crate) async fn analyze_file_impl(
    analyzer: &CodeAnalyzer,
    params: Parameters<AnalyzeFileParams>,
    context: RequestContext<RoleServer>,
) -> Result<CallToolResult, ErrorData> {
    let params = params.0;
    let t_start = std::time::Instant::now();
    let (seq, sid) = analyzer.emit_received_metric("analyze_file").await;
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
    span.record("gen_ai.tool.name", "analyze_file");
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

    // Check if path is a directory (not allowed for analyze_file)
    if std::path::Path::new(&params.path).is_dir() {
        span.record("error", true);
        span.record("error.type", "invalid_params");
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

    // summary=true and cursor are mutually exclusive
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

    // Call handler for analysis and caching
    let (arc_output, file_cache_hit) = match analyzer.handle_file_details_mode(&params).await {
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
            analyzer.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("analyze_file", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(&param_path))
                    .error_type(error_type)
                    .session_id(sid.clone())
                    .seq(Some(seq))
                    .file_ext(crate::metrics::path_file_ext(&param_path))
                    .language(crate::metrics::path_language(&param_path))
                    .build(),
            );
            return Ok(err_to_tool_result(e));
        }
    };

    // Clone only the two fields that may be mutated per-request (formatted and
    // next_cursor). The heavy SemanticAnalysis data is shared via Arc and never
    // modified, so we borrow it directly from the cached pointer.
    let mut formatted = arc_output.formatted.clone();
    let line_count = arc_output.line_count;

    // Apply summary/output size limiting logic
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

    // Decode pagination cursor if provided (analyze_file)
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

    // Filter to top-level functions only (exclude methods) before pagination
    let top_level_fns: Vec<crate::types::FunctionInfo> = arc_output
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

    // Paginate top-level functions only
    let paginated = match paginate_slice(&top_level_fns, offset, page_size, PaginationMode::Default)
    {
        Ok(v) => v,
        Err(e) => {
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                e.to_string(),
                Some(error_meta("transient", true, "retry the request")),
            )));
        }
    };

    // Regenerate formatted output using the paginated formatter (handles verbose and pagination correctly)
    // Skip regeneration when the output is an unsupported-extension fallback (sentinel in formatted).
    let is_unsupported_fallback = arc_output
        .formatted
        .contains("[Unsupported extension: semantic analysis not available]");
    if !use_summary && !is_unsupported_fallback {
        // fields: serde rejects unknown enum variants at deserialization; no runtime validation required
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

    // Capture next_cursor from pagination result (unless using summary mode)
    let next_cursor = if use_summary {
        None
    } else {
        paginated.next_cursor.clone()
    };

    // Build final text output with pagination cursor if present (unless using summary mode)
    let mut final_text = formatted.clone();
    if !use_summary && let Some(ref cursor) = next_cursor {
        final_text.push('\n');
        final_text.push_str("NEXT_CURSOR: ");
        final_text.push_str(cursor);
    }

    // Build the response output, projecting SemanticAnalysis to only the requested sections.
    let response_output = analyze::FileAnalysisOutput::new(
        formatted,
        arc_output.semantic.project(params.fields.as_deref()),
        line_count,
        next_cursor,
    );

    // Record cache tier in span
    tracing::Span::current().record("cache_tier", file_cache_hit.as_str());

    // Add content_hash to _meta
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
    let structured = serde_json::to_value(&response_output).unwrap_or(Value::Null);
    result.structured_content = Some(structured);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    analyzer.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("analyze_file", "ok", dur)
            .output_chars(final_text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .session_id(sid)
            .seq(Some(seq))
            .cache_hit(Some(file_cache_hit != CacheTier::Miss))
            .cache_tier(Some(file_cache_hit.as_str()))
            .file_ext(crate::metrics::path_file_ext(&param_path))
            .language(crate::metrics::path_language(&param_path))
            .build(),
    );
    Ok(result)
}
