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

use crate::filters::{CompiledRule, apply_filter, maybe_inject_no_stat};
use crate::metrics::MetricsSender;
use crate::otel::{ClientMetadata, extract_and_set_trace_context};
use crate::shell::resolve_shell;
use crate::shell_write;
use crate::tools::common::{err_to_tool_result, error_meta, no_cache_meta};
use crate::{ExecCommandParams, SIZE_LIMIT, STDIN_MAX_BYTES, ShellOutput, validate_path};

/// Default drain timeout in milliseconds for post-exit pipe drain (500ms).
const DEFAULT_DRAIN_TIMEOUT_MS: u64 = 500;

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

/// Result of a timed command execution.
pub(crate) struct ExecutionResult {
    exit_code: Option<i32>,
    output_truncated: bool,
    output_collection_error: Option<String>,
    timed_out: bool,
}

// ---------------------------------------------------------------------------
// Phase helpers extracted from exec_command_impl
// ---------------------------------------------------------------------------

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
    if let Err(e) = shell_write::validate_heredocs(command) {
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
        let safe_start = output_text[..tail_start].floor_char_boundary(tail_start);
        output_text[safe_start..].to_string()
    } else {
        output_text
    };

    let text = format!(
        "Command: {}\nExit code: {}\nOutput truncated: {}\n\n{}",
        params.command,
        output
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "null".to_string()),
        output.output_truncated || combined_truncated,
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
                    .build(),
            );
            return Ok(result);
        }
    };

    // Phase 3: Spawn and collect
    let resolved_path_str = resolved_path.as_deref();
    let output = match spawn_and_collect_phase(
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
            .build(),
    );
    Ok(result)
}

/// Build and configure a tokio::process::Command with stdio, working directory, and resource limits.
pub(crate) fn build_exec_command(
    command: &str,
    working_dir_path: Option<&std::path::PathBuf>,
    stdin_present: bool,
    resolved_path: Option<&str>,
) -> tokio::process::Command {
    let shell = resolve_shell();
    let mut cmd = tokio::process::Command::new(shell);

    // Unify command invocation: use -c on all platforms.
    // On macOS, the resolved PATH from the startup-captured login shell profile
    // is injected below, so -l is not needed per-command.
    cmd.arg("-c").arg(command);

    if let Some(wd) = working_dir_path {
        cmd.current_dir(wd);
    }

    // Inject resolved login shell PATH snapshot on all platforms.
    if let Some(path) = resolved_path {
        cmd.env("PATH", path);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if stdin_present {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }

    cmd
}

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

/// Run a spawned child process with output draining.
/// When `timeout_secs` is `Some(secs)` where `secs > 0`, the entire execution (drain +
/// wait) is bounded by that many seconds. If the timeout fires the child is killed.
pub(crate) async fn run_with_timeout(
    mut child: tokio::process::Child,
    tx: tokio::sync::mpsc::UnboundedSender<(bool, String)>,
    timeout_secs: Option<i64>,
    drain_timeout: std::time::Duration,
) -> ExecutionResult {
    use tokio::io::AsyncBufReadExt as _;
    use tokio_stream::StreamExt as TokioStreamExt;
    use tokio_stream::wrappers::LinesStream;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let drain_task = tokio::spawn(async move {
        let so_stream = stdout_pipe.map(|p| {
            LinesStream::new(tokio::io::BufReader::new(p).lines()).map(|l| l.map(|s| (false, s)))
        });
        let se_stream = stderr_pipe.map(|p| {
            LinesStream::new(tokio::io::BufReader::new(p).lines()).map(|l| l.map(|s| (true, s)))
        });

        match (so_stream, se_stream) {
            (Some(so), Some(se)) => {
                let mut merged = so.merge(se);
                while let Some(Ok((is_stderr, line))) = merged.next().await {
                    let _ = tx.send((is_stderr, line));
                }
            }
            (Some(so), None) => {
                let mut stream = so;
                while let Some(Ok((_, line))) = stream.next().await {
                    let _ = tx.send((false, line));
                }
            }
            (None, Some(se)) => {
                let mut stream = se;
                while let Some(Ok((_, line))) = stream.next().await {
                    let _ = tx.send((true, line));
                }
            }
            (None, None) => {}
        }
    });

    let drain_abort = drain_task.abort_handle();

    match timeout_secs {
        Some(secs) if secs > 0 => {
            // User timeout wraps only child.wait(); drain follows outside the timeout.
            let timeout_secs_u64 = u64::try_from(secs).unwrap_or(u64::MAX);
            let (exit_code, timed_out) = match tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs_u64),
                child.wait(),
            )
            .await
            {
                Ok(Ok(s)) => (s.code(), false),
                Ok(Err(_)) => (None, false),
                Err(_elapsed) => {
                    child.start_kill().ok();
                    // Reap the zombie so the OS does not accumulate a defunct child.
                    let _ = child.wait().await;
                    (None, true)
                }
            };

            // Drain remaining buffered output with drain_timeout grace (outside user timeout).
            let drain_truncated = if timed_out {
                drain_abort.abort();
                false
            } else {
                match tokio::time::timeout(drain_timeout, drain_task).await {
                    Ok(_) => false,
                    Err(_) => {
                        drain_abort.abort();
                        true
                    }
                }
            };

            let ocerr = if drain_truncated {
                Some("post-exit drain timeout: background process held pipes".to_string())
            } else {
                None
            };

            ExecutionResult {
                exit_code,
                output_truncated: drain_truncated,
                output_collection_error: ocerr,
                timed_out,
            }
        }
        _ => {
            // No user timeout: wait for child exit first, then drain buffered output
            // with a short grace period (drain_timeout) for background subprocesses.
            let exit_status = child.wait().await.ok();
            let drain_result = tokio::time::timeout(drain_timeout, drain_task).await;

            let drain_truncated = drain_result.is_err();
            if drain_truncated {
                drain_abort.abort();
            }
            let exit_code = exit_status.and_then(|s| s.code());
            let ocerr = if drain_truncated {
                Some("post-exit drain timeout: background process held pipes".to_string())
            } else {
                None
            };
            ExecutionResult {
                exit_code,
                output_truncated: drain_truncated,
                output_collection_error: ocerr,
                timed_out: false,
            }
        }
    }
}

/// Executes a shell command and returns the output.
/// This is a free async function (not a method) to allow use in moka::future::Cache::get_with().
/// It spawns the command, collects output, and persists output to slot files.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_exec_impl(
    command: String,
    working_dir_path: Option<std::path::PathBuf>,
    stdin: Option<String>,
    seq: u32,
    resolved_path: Option<&str>,
    filter_table: &Arc<Vec<CompiledRule>>,
    timeout_secs: Option<i64>,
    drain_timeout: std::time::Duration,
) -> ShellOutput {
    let mut cmd = build_exec_command(
        &command,
        working_dir_path.as_ref(),
        stdin.is_some(),
        resolved_path,
    );

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ShellOutput::new(
                String::new(),
                format!("failed to spawn command: {e}"),
                format!("failed to spawn command: {e}"),
                None,
                false,
            );
        }
    };

    if let Some(stdin_content) = stdin
        && let Some(mut stdin_handle) = child.stdin.take()
    {
        use tokio::io::AsyncWriteExt as _;
        match stdin_handle.write_all(stdin_content.as_bytes()).await {
            Ok(()) => {
                drop(stdin_handle);
            }
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => {
                tracing::warn!("failed to write stdin: {e}");
            }
        }
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(bool, String)>();

    let exec_result = run_with_timeout(child, tx, timeout_secs, drain_timeout).await;
    let exit_code = exec_result.exit_code;
    let mut output_truncated = exec_result.output_truncated;
    let output_collection_error = exec_result.output_collection_error;
    let timed_out = exec_result.timed_out;

    rx.close();

    let mut lines: Vec<(bool, String)> = Vec::new();
    while let Some(item) = rx.recv().await {
        lines.push(item);
    }

    // Split tagged lines into stdout, stderr, interleaved post-facto (no locks needed).
    const MAX_BYTES: usize = 50 * 1024;
    let mut stdout_str = String::new();
    let mut stderr_str = String::new();
    let mut interleaved_str = String::new();
    let mut so_bytes = 0usize;
    let mut se_bytes = 0usize;
    let mut il_bytes = 0usize;
    for (is_stderr, line) in &lines {
        let entry = format!("{line}\n");
        if il_bytes < 2 * MAX_BYTES {
            il_bytes += entry.len();
            interleaved_str.push_str(&entry);
        }
        if *is_stderr {
            if se_bytes < MAX_BYTES {
                se_bytes += entry.len();
                stderr_str.push_str(&entry);
            }
        } else if so_bytes < MAX_BYTES {
            so_bytes += entry.len();
            stdout_str.push_str(&entry);
        }
    }

    let slot = seq % 8;
    let (stdout, stderr, stdout_path, stderr_path, byte_truncated) =
        handle_output_persist(stdout_str, stderr_str, slot);
    output_truncated = output_truncated || stdout_path.is_some() || byte_truncated;

    let mut output = ShellOutput::new(stdout, stderr, interleaved_str, exit_code, output_truncated);
    output.output_collection_error = output_collection_error;
    output.stdout_path = stdout_path;
    output.stderr_path = stderr_path;
    output.timed_out = timed_out;

    // Apply filter if exit_code == 0
    if exit_code == Some(0) {
        for compiled_rule in filter_table.iter() {
            if compiled_rule.pattern.is_match(&command) {
                let filtered_stdout = apply_filter(compiled_rule, &output.stdout);
                output.stdout = filtered_stdout;
                // Also filter interleaved: the response handler prefers interleaved when
                // non-empty (which it always is for commands that write to both streams),
                // so filtering only stdout would leave the LLM-visible output unfiltered.
                // apply_filter is called separately on each field; there is no double-filtering
                // because stdout and interleaved are independent strings assembled from the
                // same source lines -- updating one does not affect the other.
                output.interleaved = apply_filter(compiled_rule, &output.interleaved);
                output.filter_applied = compiled_rule
                    .rule
                    .description
                    .clone()
                    .or_else(|| Some(compiled_rule.rule.match_command.clone()));
                break;
            }
        }
    }

    output
}

/// Handles output persistence by writing to slot files only when output overflows the line limit.
/// Writes full stdout/stderr to:
///   {temp_dir}/aptu-coder-overflow/slot-{slot}/{stdout,stderr}
/// Returns (stdout_out, stderr_out, stdout_path, stderr_path).
/// On overflow: truncates to last 50 lines and sets paths to Some.
/// Under limit: returns output unchanged and paths as None (no I/O).
pub(crate) fn handle_output_persist(
    stdout: String,
    stderr: String,
    slot: u32,
) -> (String, String, Option<String>, Option<String>, bool) {
    const MAX_OUTPUT_LINES: usize = 2000;
    // Sized at p99.3 of observed exec_command output_chars (27k calls): 99.27% of calls are
    // under 20k chars; raising to 30k covers 99.67% while still capping pathological cases
    // (git pull on large repos, cargo test on large workspaces) that exceed 100k chars.
    const MAX_STDOUT_BYTES: usize = 30_000;
    const MAX_STDERR_BYTES: usize = 10_000;
    const OVERFLOW_PREVIEW_LINES: usize = 50;

    let stdout_lines: Vec<&str> = stdout.lines().collect();
    let stderr_lines: Vec<&str> = stderr.lines().collect();

    let mut byte_truncated = false;

    // Check for line overflow or byte overflow
    let line_overflow =
        stdout_lines.len() > MAX_OUTPUT_LINES || stderr_lines.len() > MAX_OUTPUT_LINES;
    let stdout_byte_overflow = stdout.len() > MAX_STDOUT_BYTES;
    let stderr_byte_overflow = stderr.len() > MAX_STDERR_BYTES;
    let byte_overflow = stdout_byte_overflow || stderr_byte_overflow;

    // No overflow: return as-is with no I/O.
    if !line_overflow && !byte_overflow {
        return (stdout, stderr, None, None, false);
    }

    // Overflow: write slot files and return last-N-lines preview.
    let base = std::env::temp_dir()
        .join("aptu-coder-overflow")
        .join(format!("slot-{slot}"));
    let _ = std::fs::create_dir_all(&base);

    let stdout_path = base.join("stdout");
    let stderr_path = base.join("stderr");

    let _ = std::fs::write(&stdout_path, stdout.as_bytes());
    let _ = std::fs::write(&stderr_path, stderr.as_bytes());

    let stdout_path_str = stdout_path.display().to_string();
    let stderr_path_str = stderr_path.display().to_string();

    // Truncate stdout if it exceeds byte limit
    let stdout_preview = if stdout_byte_overflow {
        byte_truncated = true;
        // Use char-boundary-safe tail truncation
        let tail_start = stdout.len().saturating_sub(MAX_STDOUT_BYTES);
        let safe_start = stdout[..tail_start].floor_char_boundary(tail_start);
        stdout[safe_start..].to_string()
    } else if stdout_lines.len() > MAX_OUTPUT_LINES {
        stdout_lines[stdout_lines.len().saturating_sub(OVERFLOW_PREVIEW_LINES)..].join("\n")
    } else {
        stdout
    };

    // Truncate stderr if it exceeds byte limit
    let stderr_preview = if stderr_byte_overflow {
        byte_truncated = true;
        // Use char-boundary-safe tail truncation
        let tail_start = stderr.len().saturating_sub(MAX_STDERR_BYTES);
        let safe_start = stderr[..tail_start].floor_char_boundary(tail_start);
        stderr[safe_start..].to_string()
    } else if stderr_lines.len() > MAX_OUTPUT_LINES {
        stderr_lines[stderr_lines.len().saturating_sub(OVERFLOW_PREVIEW_LINES)..].join("\n")
    } else {
        stderr
    };

    (
        stdout_preview,
        stderr_preview,
        Some(stdout_path_str),
        Some(stderr_path_str),
        byte_truncated,
    )
}
