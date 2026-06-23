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

pub(crate) async fn analyze_symbol_impl(
    analyzer: &CodeAnalyzer,
    params: Parameters<AnalyzeSymbolParams>,
    context: RequestContext<RoleServer>,
) -> Result<CallToolResult, ErrorData> {
    let params = params.0;
    let t_start = std::time::Instant::now();
    let (seq, sid) = analyzer.emit_received_metric("analyze_symbol").await;
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
    span.record("gen_ai.tool.name", "analyze_symbol");
    span.record("symbol", &params.symbol);
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
    let max_depth_val = params.follow_depth;

    // Check if path is a file (not allowed for analyze_symbol)
    if std::path::Path::new(&params.path).is_file() {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        return Ok(err_to_tool_result(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            format!(
                "'{}' is a file; analyze_symbol requires a directory path",
                params.path
            ),
            Some(error_meta(
                "validation",
                false,
                "pass a directory path, not a file",
            )),
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

    // import_lookup=true is mutually exclusive with a non-empty symbol.
    if let Err(e) = CodeAnalyzer::validate_import_lookup(params.import_lookup, &params.symbol) {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        return Ok(err_to_tool_result(e));
    }

    // import_lookup mode: scan for files importing `params.symbol` as a module path.
    if params.import_lookup == Some(true) {
        let path_owned = PathBuf::from(&params.path);
        let symbol = params.symbol.clone();
        let git_ref = params.git_ref.clone();
        let max_depth = params.max_depth;

        let handle = tokio::task::spawn_blocking(move || {
            let path = path_owned.as_path();
            let raw_entries = match walk_directory(path, max_depth) {
                Ok(e) => e,
                Err(e) => {
                    return Err(ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        format!("Failed to walk directory: {e}"),
                        Some(error_meta(
                            "resource",
                            false,
                            "check path permissions and availability",
                        )),
                    ));
                }
            };
            // Apply git_ref filter when requested (non-empty string only).
            let entries = if let Some(ref git_ref_val) = git_ref
                && !git_ref_val.is_empty()
            {
                let changed = match changed_files_from_git_ref(path, git_ref_val) {
                    Ok(c) => c,
                    Err(e) => {
                        return Err(ErrorData::new(
                            rmcp::model::ErrorCode::INVALID_PARAMS,
                            format!("git_ref filter failed: {e}"),
                            Some(error_meta(
                                "resource",
                                false,
                                "ensure git is installed and path is inside a git repository",
                            )),
                        ));
                    }
                };
                filter_entries_by_git_ref(raw_entries, &changed, path)
            } else {
                raw_entries
            };
            let output = match analyze::analyze_import_lookup(path, &symbol, &entries, None) {
                Ok(v) => v,
                Err(e) => {
                    return Err(ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        format!("import_lookup failed: {e}"),
                        Some(error_meta(
                            "resource",
                            false,
                            "check path and file permissions",
                        )),
                    ));
                }
            };
            Ok(output)
        });

        let output = match handle.await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Ok(err_to_tool_result(e)),
            Err(e) => {
                return Ok(err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    format!("spawn_blocking failed: {e}"),
                    Some(error_meta("resource", false, "internal error")),
                )));
            }
        };

        let final_text = output.formatted.clone();

        // Record cache tier in span
        tracing::Span::current().record("cache_tier", "Miss");

        // Add content_hash to _meta
        let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
        let mut meta = no_cache_meta().0;
        meta.insert(
            "content_hash".to_string(),
            serde_json::Value::String(content_hash),
        );

        let mut result = CallToolResult::success(vec![
            Content::text(final_text.clone()).with_priority(0.9_f32),
        ])
        .with_meta(Some(Meta(meta)));
        let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
        result.structured_content = Some(structured);
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        analyzer.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("analyze_symbol", "ok", dur)
                .output_chars(final_text.len())
                .param_path_depth(crate::metrics::path_component_count(&param_path))
                .max_depth(max_depth_val)
                .session_id(sid)
                .seq(Some(seq))
                .cache_hit(Some(false))
                .cache_tier(Some(CacheTier::Miss.as_str()))
                .build(),
        );
        return Ok(result);
    }

    // Call handler for analysis and progress tracking
    let progress_token = context.meta.get_progress_token();
    let (graph_cache_tier, mut output) = match analyzer
        .handle_focused_mode(&params, ct, progress_token)
        .await
    {
        Ok(v) => v,
        Err(e) => return Ok(err_to_tool_result(e)),
    };

    // Surface cache tier in structuredContent for observability and testing.
    output.cache_tier = Some(graph_cache_tier.as_str().to_owned());

    // Decode pagination cursor if provided (analyze_symbol)
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
            Err(e) => return Ok(err_to_tool_result(e)),
        };
        cursor_data.offset
    } else {
        0
    };

    // SymbolFocus pagination: decode cursor mode to determine callers vs callees
    let cursor_mode = if let Some(ref cursor_str) = params.pagination.cursor {
        decode_cursor(cursor_str)
            .map(|c| c.mode)
            .unwrap_or(PaginationMode::Callers)
    } else {
        PaginationMode::Callers
    };

    let use_summary = params.output_control.summary == Some(true);

    let mut callee_cursor = match cursor_mode {
        PaginationMode::Callers => {
            let (paginated_items, paginated_next) = match paginate_focus_chains(
                &output.prod_chains,
                PaginationMode::Callers,
                offset,
                page_size,
            ) {
                Ok(v) => v,
                Err(e) => return Ok(err_to_tool_result(e)),
            };

            if !use_summary
                && (paginated_next.is_some() || offset > 0 || !output.outgoing_chains.is_empty())
            {
                let base_path = Path::new(&params.path);
                output.formatted = format_focused_paginated(
                    &paginated_items,
                    output.prod_chains.len(),
                    PaginationMode::Callers,
                    &params.symbol,
                    &output.prod_chains,
                    &output.test_chains,
                    &output.outgoing_chains,
                    output.def_count,
                    offset,
                    Some(base_path),
                    false,
                );
                paginated_next
            } else {
                None
            }
        }
        PaginationMode::Callees => {
            let (paginated_items, paginated_next) = match paginate_focus_chains(
                &output.outgoing_chains,
                PaginationMode::Callees,
                offset,
                page_size,
            ) {
                Ok(v) => v,
                Err(e) => return Ok(err_to_tool_result(e)),
            };

            if paginated_next.is_some() || offset > 0 {
                let base_path = Path::new(&params.path);
                output.formatted = format_focused_paginated(
                    &paginated_items,
                    output.outgoing_chains.len(),
                    PaginationMode::Callees,
                    &params.symbol,
                    &output.prod_chains,
                    &output.test_chains,
                    &output.outgoing_chains,
                    output.def_count,
                    offset,
                    Some(base_path),
                    false,
                );
                paginated_next
            } else {
                None
            }
        }
        PaginationMode::Default => {
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "invalid cursor: unknown pagination mode".to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "use a cursor returned by a previous analyze_symbol call",
                )),
            )));
        }
        PaginationMode::DefUse => {
            let total_sites = output.def_use_sites.len();
            let (paginated_sites, paginated_next) = match paginate_slice(
                &output.def_use_sites,
                offset,
                page_size,
                PaginationMode::DefUse,
            ) {
                Ok(r) => (r.items, r.next_cursor),
                Err(e) => return Ok(err_to_tool_result_from_pagination(e)),
            };

            // Always regenerate formatted output for DefUse mode so the
            // first page (offset=0) is not skipped.
            if !use_summary {
                let base_path = Path::new(&params.path);
                output.formatted = format_focused_paginated_defuse(
                    &paginated_sites,
                    total_sites,
                    &params.symbol,
                    offset,
                    Some(base_path),
                    false,
                );
            }

            // Slice output.def_use_sites to the current page window so
            // structuredContent only contains the paginated subset.
            output.def_use_sites = paginated_sites;

            paginated_next
        }
    };

    // When callers are exhausted and callees exist, bootstrap callee pagination
    // by emitting a {mode:callees, offset:0} cursor. This makes PaginationMode::Callees
    // reachable; without it the branch was dead code. Suppressed in summary mode
    // because summary and pagination are mutually exclusive.
    if callee_cursor.is_none()
        && cursor_mode == PaginationMode::Callers
        && !output.outgoing_chains.is_empty()
        && !use_summary
        && let Ok(cursor) = encode_cursor(&CursorData {
            mode: PaginationMode::Callees,
            offset: 0,
        })
    {
        callee_cursor = Some(cursor);
    }

    // When callees are exhausted and def_use_sites exist, bootstrap defuse cursor
    // by emitting a {mode:defuse, offset:0} cursor. This makes PaginationMode::DefUse
    // reachable. Suppressed in summary mode because summary and pagination are mutually exclusive.
    // Also bootstrap directly from Callers mode when there are no outgoing chains
    // (e.g. SymbolNotFound path or symbols with no callees) so def-use pagination
    // is reachable even without a Callees phase.
    if callee_cursor.is_none()
        && matches!(
            cursor_mode,
            PaginationMode::Callees | PaginationMode::Callers
        )
        && !output.def_use_sites.is_empty()
        && !use_summary
        && let Ok(cursor) = encode_cursor(&CursorData {
            mode: PaginationMode::DefUse,
            offset: 0,
        })
    {
        // Only bootstrap from Callers when callees are empty (otherwise
        // the Callees bootstrap above takes priority).
        if cursor_mode == PaginationMode::Callees || output.outgoing_chains.is_empty() {
            callee_cursor = Some(cursor);
        }
    }

    // Update next_cursor in output
    output.next_cursor.clone_from(&callee_cursor);

    // Build final text output with pagination cursor if present
    let mut final_text = output.formatted.clone();
    if let Some(cursor) = callee_cursor {
        final_text.push('\n');
        final_text.push_str("NEXT_CURSOR: ");
        final_text.push_str(&cursor);
    }

    // Record cache tier in span
    tracing::Span::current().record("cache_tier", graph_cache_tier.as_str());

    // Add content_hash to _meta
    let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
    let mut meta = no_cache_meta().0;
    meta.insert(
        "content_hash".to_string(),
        serde_json::Value::String(content_hash),
    );

    let mut result = CallToolResult::success(vec![
        Content::text(final_text.clone()).with_priority(0.9_f32),
    ])
    .with_meta(Some(Meta(meta)));
    // Only include def_use_sites in structuredContent when in DefUse mode.
    // In Callers/Callees modes, clearing the vec prevents large def-use
    // payloads from leaking into paginated non-def-use responses.
    if cursor_mode != PaginationMode::DefUse {
        output.def_use_sites = Vec::new();
    }
    let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
    result.structured_content = Some(structured);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    analyzer.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("analyze_symbol", "ok", dur)
            .output_chars(final_text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .max_depth(max_depth_val)
            .session_id(sid)
            .seq(Some(seq))
            .cache_hit(Some(graph_cache_tier != CacheTier::Miss))
            .cache_tier(Some(graph_cache_tier.as_str()))
            .build(),
    );
    Ok(result)
}
