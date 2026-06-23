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

pub(crate) async fn analyze_directory_impl(
    analyzer: &CodeAnalyzer,
    params: Parameters<AnalyzeDirectoryParams>,
    context: RequestContext<RoleServer>,
) -> Result<CallToolResult, ErrorData> {
    let mut params = params.0;
    // Apply max_depth default: 3. Pass 0 for unlimited depth.
    params.max_depth = params.max_depth.or(Some(3));
    let t_start = std::time::Instant::now();
    let (seq, sid) = analyzer.emit_received_metric("analyze_directory").await;
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
    span.record("gen_ai.tool.name", "analyze_directory");
    span.record("path", &params.path);
    let _validated_path = match validate_path(&params.path, true) {
        Ok(p) => p,
        Err(e) => {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            return Ok(err_to_tool_result(e));
        }
    };
    let ct = context.ct.clone();
    let param_path = params.path.clone();
    let max_depth_val = params.max_depth;

    // Call handler for analysis and progress tracking
    let progress_token = context.meta.get_progress_token();
    let (arc_output, dir_cache_hit) = match analyzer
        .handle_overview_mode(&params, ct, progress_token)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            span.record("error", true);
            span.record("error.type", "internal_error");
            return Ok(err_to_tool_result(e));
        }
    };
    // Extract the value from Arc for modification. On a cache hit the Arc is shared,
    // so try_unwrap may fail; fall back to cloning the underlying value in that case.
    let mut output = match std::sync::Arc::try_unwrap(arc_output) {
        Ok(owned) => owned,
        Err(arc) => (*arc).clone(),
    };

    // summary=true (explicit) and cursor are mutually exclusive.
    // Auto-summarization (summary=None + large output) must NOT block cursor pagination.
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

    // Determine output mode:
    //   summary=true  -> compact summary (format_summary)
    //   summary=false -> explicit paginated flat list (format_structure_paginated)
    //   summary=None, small output (<=SIZE_LIMIT) -> tree as-is (format_structure)
    //   summary=None, large output (>SIZE_LIMIT)  -> compact summary (format_summary)
    let use_summary = if params.output_control.summary == Some(true) {
        true
    } else if params.output_control.summary == Some(false) {
        false
    } else {
        output.formatted.len() > SIZE_LIMIT
    };

    // summary=false is the only path that uses format_structure_paginated
    let use_paginated = params.output_control.summary == Some(false);

    if use_summary {
        output.formatted = format_summary(
            &output.entries,
            &output.files,
            params.max_depth,
            output.subtree_counts.as_deref(),
        );
    }

    // Decode pagination cursor if provided (only relevant for paginated mode)
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

    // Apply pagination to files (used only in paginated mode)
    let paginated = match paginate_slice(&output.files, offset, page_size, PaginationMode::Default)
    {
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

    // Update next_cursor in output after pagination (only in paginated mode)
    if use_paginated {
        output.next_cursor.clone_from(&paginated.next_cursor);
    } else {
        output.next_cursor = None;
    }

    // Build final text output with pagination cursor if present (only in paginated mode)
    let mut final_text = output.formatted.clone();
    if use_paginated && let Some(cursor) = paginated.next_cursor {
        final_text.push('\n');
        final_text.push_str("NEXT_CURSOR: ");
        final_text.push_str(&cursor);
    }

    // Record cache tier in span
    tracing::Span::current().record("cache_tier", dir_cache_hit.as_str());

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
    let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
    result.structured_content = Some(structured);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    analyzer.metrics_tx.send(
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
