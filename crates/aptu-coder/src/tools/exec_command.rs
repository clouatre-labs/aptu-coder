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
use crate::tools::common::{
    DEFAULT_DRAIN_TIMEOUT_MS, ExecutionResult, build_exec_command, handle_output_persist,
    run_exec_impl, run_with_timeout, strip_cd_prefix,
};
use crate::validation::{validate_path, validate_path_in_dir};
use crate::{ExecCommandParams, STDIN_MAX_BYTES};
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

pub(crate) async fn exec_command_impl(
    analyzer: &CodeAnalyzer,
    params: Parameters<ExecCommandParams>,
    context: RequestContext<RoleServer>,
) -> Result<CallToolResult, ErrorData> {
    let t_start = std::time::Instant::now();
    let (seq, sid) = analyzer.emit_received_metric("exec_command").await;
    let params = params.0;
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
    span.record("gen_ai.tool.name", "exec_command");
    span.record("command", &params.command);

    // Validate working_dir if provided -- existence + is_dir only, no CWD confinement.
    // exec_command is a shell runner; CWD confinement applies only to edit_overwrite/edit_replace.
    let working_dir_path = if let Some(ref wd) = params.working_dir {
        match std::fs::canonicalize(wd) {
            Ok(p) => {
                if !p.is_dir() {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    let mut result = CallToolResult::error(vec![Content::text(
                        "working_dir is not a directory; provide an existing directory path"
                            .to_string(),
                    )])
                    .with_meta(Some(no_cache_meta()));
                    result.structured_content = Some(serde_json::json!({
                        "workingDir": wd,
                    }));
                    return Ok(result);
                }
                Some(p)
            }
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                let mut result = CallToolResult::error(vec![Content::text(
                    "working_dir is not valid; provide an existing directory path".to_string(),
                )])
                .with_meta(Some(no_cache_meta()));
                result.structured_content = Some(serde_json::json!({
                    "workingDir": wd,
                    "error": e.to_string(),
                }));
                return Ok(result);
            }
        }
    } else {
        None
    };

    // Strip leading "cd <path> &&" prefix from command only when provably redundant.
    // - No working_dir: promote the cd path as working_dir (unambiguous).
    // - working_dir already set: strip only if the cd path resolves to the same
    //   directory; otherwise pass the command through unmodified (the cd is
    //   load-bearing, e.g. a multi-step chain like "cd sub && build && cd ../other && build").
    let (effective_command, cd_extracted_path) = strip_cd_prefix(&params.command);
    let (command, working_dir_path) = if let Some(cd_path) = cd_extracted_path {
        if working_dir_path.is_none() {
            // Only promote when the path is a plain absolute literal -- no shell
            // special characters (~, $, -). Relative paths and shell-expanded forms
            // (cd ~, cd $VAR, cd -) must reach the shell unmodified; validate_path
            // cannot resolve them correctly before execution.
            let is_plain_absolute = cd_path.starts_with('/')
                && !cd_path.contains('$')
                && !cd_path.contains('~')
                && cd_path != "-";
            if !is_plain_absolute {
                // Shell-special or relative -- pass through unmodified.
                (params.command.clone(), working_dir_path)
            } else {
                // Promote the cd path as working_dir, run through validation
                match validate_path(cd_path, true) {
                    Ok(p) if std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false) => {
                        tracing::debug!(
                            "exec_command: promoting cd prefix path as working_dir: {}",
                            cd_path
                        );
                        (effective_command.to_owned(), Some(p))
                    }
                    Ok(_) => {
                        span.record("error", true);
                        span.record("error.type", "invalid_params");
                        let mut result = CallToolResult::error(vec![Content::text(
                            "cd prefix path is not a directory; set working_dir explicitly or use a valid directory path".to_string(),
                        )])
                        .with_meta(Some(no_cache_meta()));
                        result.structured_content = Some(serde_json::json!({
                            "cdPath": cd_path,
                        }));
                        return Ok(result);
                    }
                    Err(_) => {
                        span.record("error", true);
                        span.record("error.type", "invalid_params");
                        let mut result = CallToolResult::error(vec![Content::text(
                            "cd prefix path does not exist or is outside CWD; set working_dir explicitly".to_string(),
                        )])
                        .with_meta(Some(no_cache_meta()));
                        result.structured_content = Some(serde_json::json!({
                            "cdPath": cd_path,
                        }));
                        return Ok(result);
                    }
                }
            }
        } else {
            // working_dir is already set -- only strip if the cd path resolves to
            // the same directory (redundant). Otherwise keep the full original command.
            let cd_resolves_to_same = validate_path(cd_path, true)
                .ok()
                .map(|p| Some(&p) == working_dir_path.as_ref())
                .unwrap_or(false);
            if cd_resolves_to_same {
                tracing::debug!(
                    "exec_command: stripped redundant cd prefix; matches explicit working_dir"
                );
                (effective_command.to_owned(), working_dir_path)
            } else {
                // cd path differs from working_dir -- the cd is load-bearing; pass through.
                (params.command.clone(), working_dir_path)
            }
        }
    } else {
        (params.command.clone(), working_dir_path)
    };
    // Inject --no-stat for git pull if not already present
    let command = maybe_inject_no_stat(&command);
    span.record("command", &command);

    let param_path = params.working_dir.clone();

    // Validate stdin size cap (1 MB)
    if let Some(ref stdin_content) = params.stdin
        && stdin_content.len() > STDIN_MAX_BYTES
    {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        return Ok(err_to_tool_result(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "stdin exceeds 1 MB limit".to_string(),
            Some(error_meta("validation", false, "reduce stdin content size")),
        )));
    }

    // Validate heredocs before spawning any process
    if let Err(e) = crate::validation::validate_heredocs(&command) {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        analyzer.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("exec_command", "error", dur)
                .param_path_depth(crate::metrics::path_component_count(
                    param_path.as_deref().unwrap_or(""),
                ))
                .error_type(Some("invalid_params".to_string()))
                .session_id(sid)
                .seq(Some(seq))
                .output_truncated(Some(false))
                .build(),
        );
        return Ok(err_to_tool_result(e));
    }

    // Validate drain_timeout_secs: negative values are invalid.
    if let Some(n) = params.drain_timeout_secs
        && n < 0
    {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        return Ok(err_to_tool_result(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "drain_timeout_secs must be >= 0".to_string(),
            Some(error_meta(
                "validation",
                false,
                "use a non-negative value or omit it",
            )),
        )));
    }

    // Compute effective drain timeout
    let drain_dur = match params.drain_timeout_secs {
        Some(n) if n > 0 => std::time::Duration::from_millis(n as u64),
        _ => std::time::Duration::from_millis(DEFAULT_DRAIN_TIMEOUT_MS),
    };

    // Execute command (non-cacheable; exec_command is side-effecting and non-idempotent)
    let resolved_path_str = analyzer.resolved_path.as_ref().as_deref();
    let output = run_exec_impl(
        command.clone(),
        working_dir_path.clone(),
        params.stdin.clone(),
        seq,
        resolved_path_str,
        &analyzer.filter_table,
        params.timeout_secs,
        drain_dur,
    )
    .await;

    // Short-circuit on timeout: return INTERNAL_ERROR before any output processing.
    if output.timed_out {
        span.record("error", true);
        span.record("error.type", "timeout");
        let mut result = CallToolResult::error(vec![Content::text(
            "Command execution timed out; the process was killed.".to_string(),
        )])
        .with_meta(Some(no_cache_meta()));
        result.structured_content = Some(serde_json::json!({
            "timed_out": true,
            "timeout_secs": params.timeout_secs,
        }));
        let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        analyzer.metrics_tx.send(
            crate::metrics::MetricEventBuilder::new("exec_command", "error", dur)
                .param_path_depth(crate::metrics::path_component_count(
                    param_path.as_deref().unwrap_or(""),
                ))
                .error_type(Some("timeout".to_string()))
                .session_id(sid)
                .seq(Some(seq))
                .timed_out(true)
                .output_truncated(Some(false))
                .build(),
        );
        return Ok(result);
    }

    let exit_code = output.exit_code;
    let mut output_truncated = output.output_truncated;

    // Record execution results on span
    if let Some(code) = exit_code {
        span.record("exit_code", code);
    }
    span.record("output_truncated", output_truncated);

    // Emit debug event for truncation
    if output_truncated {
        tracing::debug!(truncated = true, message = "output truncated");
    }

    // Use interleaved if non-empty; fall back to separated stdout/stderr for empty-output commands
    let output_text = if output.interleaved.is_empty() {
        format!("Stdout:\n{}\n\nStderr:\n{}", output.stdout, output.stderr)
    } else {
        format!("Output:\n{}", output.interleaved)
    };

    // Apply combined output size limit (SIZE_LIMIT = 5_000 bytes). Per-stream caps
    // (MAX_STDOUT_BYTES = 30k stdout, MAX_STDERR_BYTES = 10k stderr) already fired in
    // handle_output_persist; this is the safety net for the interleaved assembly which
    // can still reach up to ~40k bytes from per-stream content plus headers and formatting.
    let mut combined_truncated = false;
    let truncated_output_text = if output_text.len() > SIZE_LIMIT {
        combined_truncated = true;
        // Use char-boundary-safe tail truncation
        let tail_start = output_text.len().saturating_sub(SIZE_LIMIT);
        let safe_start = output_text[..tail_start].floor_char_boundary(tail_start);
        output_text[safe_start..].to_string()
    } else {
        output_text
    };

    // Update output_truncated flag to include combined truncation
    output_truncated = output_truncated || combined_truncated;

    let text = format!(
        "Command: {}\nExit code: {}\nOutput truncated: {}\n\n{}",
        params.command,
        exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "null".to_string()),
        output_truncated,
        truncated_output_text,
    );

    let content_blocks = vec![Content::text(text.clone()).with_priority(0.0)];

    // Determine if command failed: non-zero exit code.
    // exit_code is None when the post-exit drain times out (background child
    // holding pipes -- command work was done, treat as success) or when the
    // process is externally killed; both cases use unwrap_or(false) to avoid
    // false negatives.
    let command_failed = exit_code.map(|c| c != 0).unwrap_or(false);

    let mut result = if command_failed {
        CallToolResult::error(content_blocks)
    } else {
        CallToolResult::success(content_blocks)
    }
    .with_meta(Some(no_cache_meta()));

    let structured = match serde_json::to_value(&output).map_err(|e| {
        ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            format!("serialization failed: {e}"),
            Some(error_meta("internal", false, "report this as a bug")),
        )
    }) {
        Ok(v) => v,
        Err(e) => {
            span.record("error", true);
            span.record("error.type", "internal_error");
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            analyzer.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("exec_command", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(
                        param_path.as_deref().unwrap_or(""),
                    ))
                    .error_type(Some("internal_error".to_string()))
                    .session_id(sid.clone())
                    .seq(Some(seq))
                    .exit_code(exit_code)
                    .timed_out(output.timed_out)
                    .output_truncated(Some(output_truncated))
                    .build(),
            );
            return Ok(err_to_tool_result(e));
        }
    };

    result.structured_content = Some(structured);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    analyzer.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("exec_command", "ok", dur)
            .output_chars(text.len())
            .param_path_depth(crate::metrics::path_component_count(
                param_path.as_deref().unwrap_or(""),
            ))
            .session_id(sid)
            .seq(Some(seq))
            .exit_code(exit_code)
            .timed_out(output.timed_out)
            .output_truncated(Some(output_truncated))
            .chars_threshold_breach(text.len() > 30_000)
            .filter_applied(output.filter_applied.clone())
            .build(),
    );
    Ok(result)
}
