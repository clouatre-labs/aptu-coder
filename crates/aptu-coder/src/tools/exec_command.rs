// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Free-function implementation of the `exec_command` tool handler.
//!
//! All logic that was previously in the `exec_command` method body of `CodeAnalyzer`
//! lives here, along with the five exclusive private helpers. The `#[tool(...)]`-decorated
//! shim in `lib.rs` extracts state from `&self` and delegates to `exec_command_impl`.

use std::sync::Arc;

use rmcp::RoleServer;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::service::RequestContext;
use tracing::instrument;

use crate::filters::{CompiledRule, maybe_inject_no_stat};
use crate::metrics::MetricsSender;
use crate::otel::{ClientMetadata, extract_and_set_trace_context};
use crate::shell_write;
use crate::tools::common::{err_to_tool_result, error_meta, no_cache_meta};
use crate::tools::exec_runtime::{DEFAULT_DRAIN_TIMEOUT_MS, run_exec_impl};
use crate::{ExecCommandParams, SIZE_LIMIT, STDIN_MAX_BYTES, ShellOutput, validate_path};

/// State extracted from `&self` in the `exec_command` shim and passed to `exec_command_impl`.
pub(crate) struct ExecContext {
    pub(crate) seq: u32,
    pub(crate) sid: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) client_name: Option<String>,
    pub(crate) client_version: Option<String>,
    pub(crate) resolved_path: Option<String>,
    pub(crate) filter_table: std::sync::Arc<Vec<CompiledRule>>,
    pub(crate) metrics_tx: MetricsSender,
    pub(crate) t_start: std::time::Instant,
}

/// Phase 1: Resolve working directory and promote cd-prefix.
///
/// Canonicalizes the `working_dir` parameter, validates it is a directory, and
/// promotes a `cd <path> &&` prefix into the working directory when no explicit
/// `working_dir` was provided. Returns `(effective_command, resolved_working_dir_path)`.
#[allow(clippy::result_large_err)]
fn validate_working_dir_phase(
    params: &ExecCommandParams,
    span: &tracing::Span,
) -> Result<(String, Option<std::path::PathBuf>), CallToolResult> {
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
                    return Err(result);
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
                return Err(result);
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
                    Ok(p) if p.is_dir() => {
                        tracing::debug!(
                            "exec_command: promoting cd prefix path as working_dir: {}",
                            p.display()
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
                        return Err(result);
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
                        return Err(result);
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

    Ok((command, working_dir_path))
}

/// Phase 2: Validate pre-spawn requirements (stdin size, heredocs, drain timeout).
#[allow(clippy::result_large_err)]
fn validate_pre_spawn_phase(
    params: &ExecCommandParams,
    command: &str,
    span: &tracing::Span,
) -> Result<std::time::Duration, CallToolResult> {
    // Validate stdin size cap (1 MB)
    if let Some(ref stdin_content) = params.stdin
        && stdin_content.len() > STDIN_MAX_BYTES
    {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        let result = CallToolResult::error(vec![Content::text(
            ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "stdin exceeds 1 MB limit".to_string(),
                Some(error_meta("validation", false, "reduce stdin content size")),
            )
            .message,
        )])
        .with_meta(Some(no_cache_meta()));
        return Err(result);
    }

    // Validate heredocs before spawning any process
    if let Err(e) = shell_write::validate_heredocs(command, params.stdin.is_some()) {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        return Err(err_to_tool_result(e));
    }

    // Validate drain_timeout_secs: negative values are invalid.
    if let Some(n) = params.drain_timeout_secs
        && n < 0
    {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        let result = CallToolResult::error(vec![Content::text(
            ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "drain_timeout_secs must be >= 0".to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "use a non-negative value or omit it",
                )),
            )
            .message,
        )])
        .with_meta(Some(no_cache_meta()));
        return Err(result);
    }

    // Compute effective drain timeout
    let drain_dur = match params.drain_timeout_secs {
        Some(n) if n > 0 => std::time::Duration::from_millis(n as u64),
        _ => std::time::Duration::from_millis(DEFAULT_DRAIN_TIMEOUT_MS),
    };

    Ok(drain_dur)
}

/// Phase 3: Spawn and collect output.
///
/// Executes the command, handles timeout, and returns the raw output.
#[allow(clippy::too_many_arguments)]
async fn spawn_and_collect_phase(
    command: String,
    working_dir_path: Option<std::path::PathBuf>,
    params: &ExecCommandParams,
    seq: u32,
    resolved_path_str: Option<&str>,
    filter_table: &Arc<Vec<CompiledRule>>,
    drain_dur: std::time::Duration,
    span: &tracing::Span,
) -> Result<ShellOutput, CallToolResult> {
    let output = run_exec_impl(
        command,
        working_dir_path,
        params.stdin.clone(),
        seq,
        resolved_path_str,
        filter_table,
        params.timeout_secs,
        drain_dur,
    )
    .await;

    // Short-circuit on timeout: return error before any output processing.
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
        return Err(result);
    }

    Ok(output)
}

/// Phase 4: Format output text and apply truncation limits.
///
/// Returns (formatted_text, combined_truncated).
fn format_shell_output_phase(output: &ShellOutput, params: &ExecCommandParams) -> (String, bool) {
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
        let safe_start = output_text.floor_char_boundary(tail_start);
        output_text[safe_start..].to_string()
    } else {
        output_text
    };

    // Build truncation notice with slot file paths if present
    let mut truncation_notice = String::new();
    if output.stdout_path.is_some()
        || output.stderr_path.is_some()
        || output.interleaved_path.is_some()
    {
        truncation_notice.push_str("(Full output persisted to: ");
        let mut paths = Vec::new();
        if let Some(ref p) = output.stdout_path {
            paths.push(format!("stdout={}", p));
        }
        if let Some(ref p) = output.stderr_path {
            paths.push(format!("stderr={}", p));
        }
        if let Some(ref p) = output.interleaved_path {
            paths.push(format!("interleaved={}", p));
        }
        truncation_notice.push_str(&paths.join(", "));
        truncation_notice.push_str(")\n");
    }

    let text = format!(
        "Command: {}\nExit code: {}\nOutput truncated: {}\n{}{}",
        params.command,
        output
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "null".to_string()),
        output.output_truncated || combined_truncated,
        truncation_notice,
        truncated_output_text,
    );

    (text, combined_truncated)
}

/// Free-function implementation of the `exec_command` tool handler.
///
/// The `#[tool(...)]`-decorated shim in `lib.rs` extracts state from `&self` into
/// [`ExecContext`] and calls this function.
#[instrument(
    name = "exec_command_impl",
    skip(params, context, ctx),
    fields(
        gen_ai.system = tracing::field::Empty,
        gen_ai.operation.name = tracing::field::Empty,
        gen_ai.tool.name = tracing::field::Empty,
        error = tracing::field::Empty,
        error.type = tracing::field::Empty,
        command = tracing::field::Empty,
        exit_code = tracing::field::Empty,
        output_truncated = tracing::field::Empty,
        mcp.session.id = tracing::field::Empty,
        client.name = tracing::field::Empty,
        client.version = tracing::field::Empty,
        mcp.client.session.id = tracing::field::Empty
    )
)]
pub(crate) async fn exec_command_impl(
    params: ExecCommandParams,
    context: RequestContext<RoleServer>,
    ctx: ExecContext,
) -> Result<CallToolResult, ErrorData> {
    let ExecContext {
        seq,
        sid,
        session_id,
        client_name,
        client_version,
        resolved_path,
        filter_table,
        metrics_tx,
        t_start,
    } = ctx;
    // Extract W3C Trace Context from request _meta if present
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

    let param_path = params.working_dir.clone();
    let working_dir_used = params.working_dir.is_some();
    let stdin_provided = params.stdin.is_some();
    let timeout_configured_ms = params.timeout_secs.map(|s| s * 1000);
    let drain_timeout_ms = params.drain_timeout_secs;

    // Phase 1: Validate working_dir and resolve cd-prefix
    let (command, working_dir_path) = match validate_working_dir_phase(&params, &span) {
        Ok((cmd, wd)) => {
            span.record("command", &cmd);
            (cmd, wd)
        }
        Err(result) => {
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("exec_command", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(
                        param_path.as_deref().unwrap_or(""),
                    ))
                    .error_type(Some("invalid_params".to_string()))
                    .session_id(sid)
                    .seq(Some(seq))
                    .output_truncated(Some(false))
                    .stdin_provided(stdin_provided)
                    .timeout_configured_ms(timeout_configured_ms)
                    .drain_timeout_ms(drain_timeout_ms)
                    .working_dir_used(working_dir_used)
                    .build(),
            );
            return Ok(result);
        }
    };

    // Phase 2: Validate pre-spawn requirements
    let drain_dur = match validate_pre_spawn_phase(&params, &command, &span) {
        Ok(dur) => dur,
        Err(result) => {
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("exec_command", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(
                        param_path.as_deref().unwrap_or(""),
                    ))
                    .error_type(Some("invalid_params".to_string()))
                    .session_id(sid)
                    .seq(Some(seq))
                    .output_truncated(Some(false))
                    .stdin_provided(stdin_provided)
                    .timeout_configured_ms(timeout_configured_ms)
                    .drain_timeout_ms(drain_timeout_ms)
                    .working_dir_used(working_dir_used)
                    .build(),
            );
            return Ok(result);
        }
    };

    // Phase 3: Spawn and collect
    let resolved_path_str = resolved_path.as_deref();
    let mut output = match spawn_and_collect_phase(
        command.clone(),
        working_dir_path.clone(),
        &params,
        seq,
        resolved_path_str,
        &filter_table,
        drain_dur,
        &span,
    )
    .await
    {
        Ok(o) => o,
        Err(result) => {
            let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("exec_command", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(
                        param_path.as_deref().unwrap_or(""),
                    ))
                    .error_type(Some("timeout".to_string()))
                    .session_id(sid)
                    .seq(Some(seq))
                    .timed_out(true)
                    .output_truncated(Some(false))
                    .stdin_provided(stdin_provided)
                    .timeout_configured_ms(timeout_configured_ms)
                    .drain_timeout_ms(drain_timeout_ms)
                    .working_dir_used(working_dir_used)
                    .build(),
            );
            return Ok(result);
        }
    };

    let exit_code = output.exit_code;
    let mut output_truncated = output.output_truncated;

    // Record execution results on span
    if let Some(code) = exit_code {
        span.record("exit_code", code);
    }

    // Phase 4: Format output
    let (text, combined_truncated) = format_shell_output_phase(&output, &params);

    // Update output_truncated flag to include combined truncation
    output_truncated = output_truncated || combined_truncated;

    // Sync output_truncated to the struct before serialization (fix #1266)
    output.output_truncated = output_truncated;

    span.record("output_truncated", output_truncated);

    // Emit debug event for truncation
    if output_truncated {
        tracing::debug!(truncated = true, message = "output truncated");
    }

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
            metrics_tx.send(
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
                    .stdin_provided(stdin_provided)
                    .timeout_configured_ms(timeout_configured_ms)
                    .drain_timeout_ms(drain_timeout_ms)
                    .working_dir_used(working_dir_used)
                    .build(),
            );
            return Ok(err_to_tool_result(e));
        }
    };

    result.structured_content = Some(structured);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    metrics_tx.send(
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
            .stdin_provided(stdin_provided)
            .timeout_configured_ms(timeout_configured_ms)
            .drain_timeout_ms(drain_timeout_ms)
            .working_dir_used(working_dir_used)
            .build(),
    );
    Ok(result)
}

/// Build and configure a tokio::process::Command with stdio, working directory, and resource limits.
/// Strip a leading `cd <path> &&` prefix from a command string.
///
/// Returns `(stripped_command, Some(extracted_path))` when the command starts with
/// `cd <path> &&`. Returns `(cmd, None)` when no `cd ... &&` prefix is found.
///
/// Uses only `str` methods (no regex). Leading whitespace is trimmed before matching.
pub(crate) fn strip_cd_prefix(cmd: &str) -> (&str, Option<&str>) {
    let trimmed = cmd.trim_start();
    let Some(rest) = trimmed.strip_prefix("cd ") else {
        return (cmd, None);
    };
    // Find the && separator
    let Some((path_part, rest_part)) = rest.split_once("&&") else {
        return (cmd, None);
    };
    let path = path_part.trim();
    let stripped = rest_part.trim();
    (stripped, Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShellOutput;

    #[test]
    fn test_format_shell_output_mid_char_boundary() {
        // Regression: tail_start falls inside a multi-byte UTF-8 char.
        // Construct interleaved of SIZE_LIMIT + 1 bytes where byte 1
        // is the second byte of a 3-byte char (中, U+4E2D).
        // Old code: output_text[..tail_start] panics.
        // Fix: floor_char_boundary on the full string returns 0, no panic.
        let mut interleaved = String::new();
        interleaved.push('\u{4E2D}'); // 3 bytes: 0xE4 0xB8 0xAD
        interleaved.push_str(&"a".repeat(4998)); // total = 5001 bytes
        assert_eq!(interleaved.len(), 5001);

        let output = ShellOutput {
            stdout: String::new(),
            stderr: String::new(),
            interleaved,
            exit_code: Some(0),
            output_truncated: false,
            output_collection_error: None,
            stdout_path: None,
            stderr_path: None,
            interleaved_path: None,
            filter_applied: None,
            timed_out: false,
        };

        let params = ExecCommandParams {
            command: "echo test".to_string(),
            working_dir: None,
            stdin: None,
            timeout_secs: None,
            drain_timeout_secs: None,
        };

        let (result, truncated) = format_shell_output_phase(&output, &params);

        assert!(truncated, "should be truncated");
        assert!(result.is_char_boundary(0), "start should be char boundary");
        assert!(
            result.is_char_boundary(result.len()),
            "end should be char boundary"
        );
        let _char_count = result.chars().count();
    }
}
