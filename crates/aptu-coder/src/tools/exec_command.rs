// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Free-function implementation of the `exec_command` tool handler.
//!
//! All logic that was previously in the `exec_command` method body of `CodeAnalyzer`
//! lives here, along with the five exclusive private helpers. The `#[tool(...)]`-decorated
//! shim in `lib.rs` extracts state from `&self` and delegates to `exec_command_impl`.

use std::fmt::Write as _;
use std::sync::Arc;

use rmcp::RoleServer;
use rmcp::model::{Annotations, CallToolResult, ContentBlock, ErrorData, Resource, TextContent};
use rmcp::service::RequestContext;
use tracing::instrument;

use crate::filters::{CompiledRule, maybe_inject_no_stat};
use crate::metrics::MetricsSender;
use crate::otel::{ClientMetadata, extract_and_set_trace_context};
use crate::shell_write;
use crate::tools::common::{err_to_tool_result, error_meta, no_cache_meta};
use crate::tools::exec_runtime::run_exec_impl;
use crate::{ExecCommandParams, STDIN_MAX_BYTES, ShellOutput, ShellOutputMetadata, validate_path};

/// Machine-readable cause for an `exec_command` `invalid_params` error, used to
/// populate `MetricEvent::error_subtype` for per-cause metrics dashboards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecCommandErrorSubtype {
    /// `working_dir` canonicalized to a path that exists but is not a directory.
    WorkingDirNotDir,
    /// `working_dir` failed to canonicalize (does not exist or is inaccessible).
    WorkingDirNotFound,
    /// A promoted `cd <path> &&` prefix resolved to a path that is not a directory.
    CdPathNotDir,
    /// A promoted `cd <path> &&` prefix does not exist or is outside CWD.
    CdPathNotFound,
    /// `stdin` content exceeds the 1 MB size cap.
    StdinTooLarge,
    /// Heredoc validation failed (malformed or unterminated heredoc).
    HeredocError,
}

impl ExecCommandErrorSubtype {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WorkingDirNotDir => "working_dir_not_dir",
            Self::WorkingDirNotFound => "working_dir_not_found",
            Self::CdPathNotDir => "cd_path_not_dir",
            Self::CdPathNotFound => "cd_path_not_found",
            Self::StdinTooLarge => "stdin_too_large",
            Self::HeredocError => "heredoc_error",
        }
    }
}

impl std::fmt::Display for ExecCommandErrorSubtype {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    /// Per-request cancellation token; cancelled when the client sends
    /// `notifications/cancelled` for this request.
    pub(crate) ct: tokio_util::sync::CancellationToken,
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
) -> Result<(String, Option<std::path::PathBuf>), (CallToolResult, ExecCommandErrorSubtype)> {
    // Validate working_dir if provided -- existence + is_dir only, no CWD confinement.
    // exec_command is a shell runner; CWD confinement applies only to edit_overwrite/edit_replace.
    let working_dir_path = if let Some(ref wd) = params.working_dir {
        match std::fs::canonicalize(wd) {
            Ok(p) => {
                if !p.is_dir() {
                    span.record("error", true);
                    span.record("error.type", "invalid_params");
                    let mut result = CallToolResult::error(vec![ContentBlock::text(
                        "working_dir is not a directory; provide an existing directory path"
                            .to_string(),
                    )])
                    .with_meta(Some(no_cache_meta()));
                    result.structured_content = Some(serde_json::json!({
                        "workingDir": wd,
                    }));
                    return Err((result, ExecCommandErrorSubtype::WorkingDirNotDir));
                }
                Some(p)
            }
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                let mut result = CallToolResult::error(vec![ContentBlock::text(
                    "working_dir is not valid; provide an existing directory path".to_string(),
                )])
                .with_meta(Some(no_cache_meta()));
                result.structured_content = Some(serde_json::json!({
                    "workingDir": wd,
                    "error": e.to_string(),
                }));
                return Err((result, ExecCommandErrorSubtype::WorkingDirNotFound));
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
                        let mut result = CallToolResult::error(vec![ContentBlock::text(
                            "cd prefix path is not a directory; set working_dir explicitly or use a valid directory path".to_string(),
                        )])
                        .with_meta(Some(no_cache_meta()));
                        result.structured_content = Some(serde_json::json!({
                            "cdPath": cd_path,
                        }));
                        return Err((result, ExecCommandErrorSubtype::CdPathNotDir));
                    }
                    Err(_) => {
                        span.record("error", true);
                        span.record("error.type", "invalid_params");
                        let mut result = CallToolResult::error(vec![ContentBlock::text(
                            "cd prefix path does not exist or is outside CWD; set working_dir explicitly".to_string(),
                        )])
                        .with_meta(Some(no_cache_meta()));
                        result.structured_content = Some(serde_json::json!({
                            "cdPath": cd_path,
                        }));
                        return Err((result, ExecCommandErrorSubtype::CdPathNotFound));
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

/// Phase 2: Validate pre-spawn requirements (stdin size, heredocs).
#[allow(clippy::result_large_err)]
fn validate_pre_spawn_phase(
    params: &ExecCommandParams,
    command: &str,
    span: &tracing::Span,
) -> Result<(), (CallToolResult, ExecCommandErrorSubtype)> {
    // Validate stdin size cap (1 MB)
    if let Some(ref stdin_content) = params.stdin
        && stdin_content.len() > STDIN_MAX_BYTES
    {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        let result = CallToolResult::error(vec![ContentBlock::text(
            ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "stdin exceeds 1 MB limit".to_string(),
                Some(error_meta("validation", false, "reduce stdin content size")),
            )
            .message,
        )])
        .with_meta(Some(no_cache_meta()));
        return Err((result, ExecCommandErrorSubtype::StdinTooLarge));
    }

    // Validate heredocs before spawning any process
    if let Err(e) = shell_write::validate_heredocs(command, params.stdin.is_some()) {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        return Err((err_to_tool_result(e), ExecCommandErrorSubtype::HeredocError));
    }

    Ok(())
}

/// Phase 3: Spawn and collect output.
///
/// Executes the command, handles timeout/cancellation, and returns the raw output.
#[allow(clippy::too_many_arguments)]
async fn spawn_and_collect_phase(
    command: String,
    working_dir_path: Option<std::path::PathBuf>,
    params: &ExecCommandParams,
    seq: u32,
    resolved_path_str: Option<&str>,
    filter_table: &Arc<Vec<CompiledRule>>,
    ct: tokio_util::sync::CancellationToken,
    span: &tracing::Span,
) -> Result<(ShellOutput, String, Option<String>, u64, u64), CallToolResult> {
    let (output, interleaved, interleaved_path, raw_stdout_bytes, raw_stderr_bytes) =
        run_exec_impl(
            command,
            working_dir_path,
            params.stdin.clone(),
            seq,
            resolved_path_str,
            filter_table,
            ct,
        )
        .await;

    // Short-circuit on timeout or cancellation: return error before any output
    // processing. (rmcp drops this response when the request was cancelled, but
    // the kill/reap side effects still matter.)
    if output.timed_out {
        span.record("error", true);
        span.record("error.type", "timeout");
        let mut result = CallToolResult::error(vec![ContentBlock::text(
            "Command execution timed out; the process was killed.".to_string(),
        )])
        .with_meta(Some(no_cache_meta()));
        result.structured_content = Some(serde_json::json!({
            "timed_out": true,
        }));
        return Err(result);
    }

    Ok((
        output,
        interleaved,
        interleaved_path,
        raw_stdout_bytes,
        raw_stderr_bytes,
    ))
}

/// Phase 4: Format output text and apply truncation limits.
///
/// Returns the formatted output text string.
fn format_shell_output_phase(
    output: &ShellOutput,
    interleaved: &str,
    interleaved_path: Option<&String>,
    params: &ExecCommandParams,
    raw_stdout_bytes: u64,
    raw_stderr_bytes: u64,
) -> String {
    // Use interleaved if non-empty; fall back to separated stdout/stderr for empty-output commands
    let output_text = if interleaved.is_empty() {
        format!("Stdout:\n{}\n\nStderr:\n{}", output.stdout, output.stderr)
    } else {
        format!("Output:\n{}", interleaved)
    };

    // Build truncation notice with slot file paths if present
    let mut truncation_notice = String::new();
    // Hint selection: filter-capped slot files get a dedicated hint that names
    // the filter as the cause and states the last-run-wins retention policy;
    // overflow slot files keep the byte-count capture hint. Uncapped output
    // (neither flag) emits neither, keeping the text byte-identical.
    let hint = |path: &str, bytes: u64| -> String {
        if output.filter_capped {
            filter_capture_hint(path)
        } else {
            capture_hint(path, bytes)
        }
    };
    if output.stdout_path.is_some() || output.stderr_path.is_some() || interleaved_path.is_some() {
        if let Some(ref p) = output.stdout_path {
            let _ = writeln!(truncation_notice, "Full output available at: {p}");
        }
        if let Some(ref p) = output.stderr_path {
            let _ = writeln!(truncation_notice, "Full stderr available at: {p}");
        }
        if let Some(p) = interleaved_path {
            let _ = writeln!(truncation_notice, "Full output available at: {p}");
        }
        // Byte-count hint per overflow path; skipped when the raw counter is 0
        // (timed_out / drain-abort results report no raw counts).
        if let Some(ref p) = output.stdout_path {
            let _ = write!(truncation_notice, "{}", hint(p, raw_stdout_bytes));
        }
        if let Some(ref p) = output.stderr_path {
            let _ = write!(truncation_notice, "{}", hint(p, raw_stderr_bytes));
        }
        if let Some(p) = interleaved_path {
            let _ = write!(
                truncation_notice,
                "{}",
                hint(p, raw_stdout_bytes + raw_stderr_bytes)
            );
        }
    }

    // Filter notice: emitted only when a built-in rule capped the output, so
    // uncapped output stays byte-identical to the pre-filter-surface format.
    let filter_line = if output.filter_capped {
        format!(
            "Output filtered: {}\n",
            output
                .filter_effect
                .as_deref()
                .unwrap_or("filter rule applied")
        )
    } else {
        String::new()
    };

    format!(
        "Command: {}\nExit code: {}\nOutput truncated: {}\n{}{}{}",
        params.command,
        output
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "null".to_string()),
        output.output_truncated,
        filter_line,
        truncation_notice,
        output_text,
    )
}

/// Hint line for an overflow capture path; omits byte counts when 0.
fn capture_hint(path: &str, bytes: u64) -> String {
    if bytes > 0 {
        format!("output truncated; full capture at {path} ({bytes} bytes captured)\n")
    } else {
        format!("output truncated; full capture at {path}\n")
    }
}

/// Hint line for a filter-capped slot file; names the filter as the cause and
/// documents the last-run-wins retention of the slot directory.
fn filter_capture_hint(path: &str) -> String {
    format!(
        "output filter-capped; full pre-filter capture at {path} (filter-capped slot file, last-run-wins; readable via resources/read)\n"
    )
}

#[allow(clippy::too_many_arguments)]
/// Send the terminal `invalid_params` error metric shared by the working-dir and
/// pre-spawn validation phases. Preserves error_type/error_subtype semantics with
/// exactly one metric event per failure.
fn send_invalid_params_metric(
    metrics_tx: &MetricsSender,
    subtype: &ExecCommandErrorSubtype,
    param_path: &Option<String>,
    stdin_provided: bool,
    working_dir_used: bool,
    sid: Option<String>,
    seq: u32,
    t_start: std::time::Instant,
) {
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("exec_command", "error", dur)
            .param_path_depth(crate::metrics::path_component_count(
                param_path.as_deref().unwrap_or(""),
            ))
            .error_type(Some("invalid_params".to_string()))
            .error_subtype(Some(subtype.to_string()))
            .session_id(sid)
            .seq(Some(seq))
            .output_truncated(Some(false))
            .stdin_provided(stdin_provided)
            .working_dir_used(working_dir_used)
            .build(),
    );
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
        ct,
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

    // Phase 1: Validate working_dir and resolve cd-prefix
    let (command, working_dir_path) = match validate_working_dir_phase(&params, &span) {
        Ok((cmd, wd)) => {
            span.record("command", &cmd);
            (cmd, wd)
        }
        Err((result, subtype)) => {
            send_invalid_params_metric(
                &metrics_tx,
                &subtype,
                &param_path,
                stdin_provided,
                working_dir_used,
                sid,
                seq,
                t_start,
            );
            return Ok(result);
        }
    };

    // Phase 2: Validate pre-spawn requirements
    if let Err((result, subtype)) = validate_pre_spawn_phase(&params, &command, &span) {
        send_invalid_params_metric(
            &metrics_tx,
            &subtype,
            &param_path,
            stdin_provided,
            working_dir_used,
            sid,
            seq,
            t_start,
        );
        return Ok(result);
    }

    // Phase 3: Spawn and collect
    let resolved_path_str = resolved_path.as_deref();
    let (mut output, interleaved, interleaved_path, raw_stdout_bytes, raw_stderr_bytes) =
        match spawn_and_collect_phase(
            command.clone(),
            working_dir_path.clone(),
            &params,
            seq,
            resolved_path_str,
            &filter_table,
            ct,
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
                        .working_dir_used(working_dir_used)
                        .build(),
                );
                return Ok(result);
            }
        };

    let exit_code = output.exit_code;
    let output_truncated = output.output_truncated;

    // Record execution results on span
    if let Some(code) = exit_code {
        span.record("exit_code", code);
    }

    // Phase 4: Format output
    let text = format_shell_output_phase(
        &output,
        &interleaved,
        interleaved_path.as_ref(),
        &params,
        raw_stdout_bytes,
        raw_stderr_bytes,
    );

    // Sync output_truncated to the struct before serialization (fix #1266)
    output.output_truncated = output_truncated;

    span.record("output_truncated", output_truncated);

    // Emit debug event for truncation
    if output_truncated {
        tracing::debug!(truncated = true, message = "output truncated");
    }

    let mut content_blocks = vec![ContentBlock::Text(
        TextContent::new(text.clone()).with_annotations(Annotations::default().with_priority(0.0)),
    )];

    // MCP resource links for overflow slot files: emitted when output was
    // truncated OR a filter cap persisted pre-filter output (filter-capped
    // slot files exist even when output_truncated is false). URIs use the
    // aptu-overflow:// scheme, readable via resources/read; the truncation
    // notice still prints the filesystem path. Links are valid until the slot
    // is overwritten (last-run-wins retention).
    if output_truncated || output.filter_capped {
        if output.stdout_path.is_some() {
            content_blocks.push(ContentBlock::resource_link(
                Resource::new(
                    format!("aptu-overflow://slot-{seq}/stdout"),
                    "stdout capture (last-run-wins; readable via resources/read)".to_string(),
                )
                .with_mime_type("text/plain"),
            ));
        }
        if output.stderr_path.is_some() {
            content_blocks.push(ContentBlock::resource_link(
                Resource::new(
                    format!("aptu-overflow://slot-{seq}/stderr"),
                    "stderr capture (last-run-wins; readable via resources/read)".to_string(),
                )
                .with_mime_type("text/plain"),
            ));
        }
        if interleaved_path.is_some() {
            content_blocks.push(ContentBlock::resource_link(
                Resource::new(
                    format!("aptu-overflow://slot-{seq}/interleaved"),
                    "interleaved output capture (last-run-wins; readable via resources/read)"
                        .to_string(),
                )
                .with_mime_type("text/plain"),
            ));
        }
    }

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

    let structured = match serde_json::to_value(ShellOutputMetadata::from(&output)).map_err(|e| {
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
                    .working_dir_used(working_dir_used)
                    .build(),
            );
            return Ok(err_to_tool_result(e));
        }
    };

    result.structured_content = Some(structured);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    let mut metric_builder = crate::metrics::MetricEventBuilder::new("exec_command", "ok", dur)
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
        .working_dir_used(working_dir_used);
    // Emit approximate raw byte counts only when output_truncated is true and the
    // truncation was not caused by a timeout or drain-abort. output_truncated covers
    // both byte-budget overflow and drain-timeout cases; the drain-abort path forces
    // counters to 0 (unknown), so output_collection_error guards against emitting
    // a misleading zero.
    if output_truncated && !output.timed_out && output.output_collection_error.is_none() {
        metric_builder = metric_builder
            .stdout_bytes_raw(raw_stdout_bytes)
            .stderr_bytes_raw(raw_stderr_bytes);
    }
    metrics_tx.send(metric_builder.build());
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
    fn validate_working_dir_phase_not_a_directory_sets_working_dir_not_dir_subtype() {
        // Arrange: working_dir points to an existing file, not a directory.
        let params = ExecCommandParams {
            command: "echo hello".to_string(),
            working_dir: Some(env!("CARGO_MANIFEST_DIR").to_string() + "/Cargo.toml"),
            stdin: None,
        };
        let span = tracing::Span::none();

        // Act
        let result = validate_working_dir_phase(&params, &span);

        // Assert
        match result {
            Err((_, subtype)) => {
                assert_eq!(subtype, ExecCommandErrorSubtype::WorkingDirNotDir);
                assert_eq!(subtype.as_str(), "working_dir_not_dir");
            }
            Ok(_) => panic!("expected an error for a non-directory working_dir"),
        }
    }

    #[test]
    fn validate_working_dir_phase_cd_prefix_path_missing_sets_cd_path_not_found_subtype() {
        // Arrange: no explicit working_dir; cd prefix targets a plain absolute path that
        // does not exist, so it cannot be promoted to working_dir.
        let params = ExecCommandParams {
            command: "cd /definitely-nonexistent-dir-abcxyz123 && ls".to_string(),
            working_dir: None,
            stdin: None,
        };
        let span = tracing::Span::none();

        // Act
        let result = validate_working_dir_phase(&params, &span);

        // Assert
        match result {
            Err((_, subtype)) => {
                assert_eq!(subtype, ExecCommandErrorSubtype::CdPathNotFound);
                assert_eq!(subtype.as_str(), "cd_path_not_found");
            }
            Ok(_) => panic!("expected an error for a nonexistent cd prefix path"),
        }
    }

    #[test]
    fn validate_pre_spawn_phase_stdin_over_limit_sets_stdin_too_large_subtype() {
        // Arrange: stdin content exceeds the 1 MB cap.
        let params = ExecCommandParams {
            command: "cat".to_string(),
            working_dir: None,
            stdin: Some("a".repeat(STDIN_MAX_BYTES + 1)),
        };
        let span = tracing::Span::none();

        // Act
        let result = validate_pre_spawn_phase(&params, "cat", &span);

        // Assert
        match result {
            Err((_, subtype)) => {
                assert_eq!(subtype, ExecCommandErrorSubtype::StdinTooLarge);
                assert_eq!(subtype.as_str(), "stdin_too_large");
            }
            Ok(_) => panic!("expected an error for oversized stdin"),
        }
    }

    #[test]
    fn test_format_shell_output_stdout_path_hint() {
        // Arrange: ShellOutput with stdout_path set, no stderr or interleaved paths
        let output = ShellOutput {
            stdout: "hello".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            output_truncated: true,
            output_collection_error: None,
            stdout_path: Some("/tmp/aptu-coder-overflow/slot-0/stdout".to_string()),
            stderr_path: None,
            filter_applied: None,
            filter_capped: false,
            filter_effect: None,
            timed_out: false,
        };
        let params = ExecCommandParams {
            command: "echo hello".to_string(),
            working_dir: None,
            stdin: None,
        };

        // Act
        let result = format_shell_output_phase(&output, "", None, &params, 0, 0);

        // Assert
        assert!(
            result.contains("Full output available at: /tmp/aptu-coder-overflow/slot-0/stdout"),
            "should contain stdout path hint: {result}"
        );
        assert!(
            !result.contains("Full stderr available at:"),
            "should not contain stderr hint when stderr_path is None"
        );
    }

    #[test]
    fn test_format_shell_output_stderr_path_hint() {
        // Arrange: ShellOutput with only stderr_path set
        let output = ShellOutput {
            stdout: String::new(),
            stderr: "error".to_string(),
            exit_code: Some(1),
            output_truncated: true,
            output_collection_error: None,
            stdout_path: None,
            stderr_path: Some("/tmp/aptu-coder-overflow/slot-1/stderr".to_string()),
            filter_applied: None,
            filter_capped: false,
            filter_effect: None,
            timed_out: false,
        };
        let params = ExecCommandParams {
            command: "false".to_string(),
            working_dir: None,
            stdin: None,
        };

        // Act
        let result = format_shell_output_phase(&output, "", None, &params, 0, 0);

        // Assert
        assert!(
            result.contains("Full stderr available at: /tmp/aptu-coder-overflow/slot-1/stderr"),
            "should contain stderr path hint: {result}"
        );
        assert!(
            !result.contains("Full output available at:"),
            "should not contain stdout hint when stdout_path is None"
        );
    }

    #[test]
    fn test_format_shell_output_interleaved_path_hint() {
        // Arrange: ShellOutput with only interleaved_path set
        let interleaved = "output".to_string();
        let interleaved_path = Some("/tmp/aptu-coder-overflow/slot-2/interleaved".to_string());
        let output = ShellOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            output_truncated: true,
            output_collection_error: None,
            stdout_path: None,
            stderr_path: None,
            filter_applied: None,
            filter_capped: false,
            filter_effect: None,
            timed_out: false,
        };
        let params = ExecCommandParams {
            command: "echo test".to_string(),
            working_dir: None,
            stdin: None,
        };

        // Act
        let result = format_shell_output_phase(
            &output,
            &interleaved,
            interleaved_path.as_ref(),
            &params,
            0,
            0,
        );

        // Assert
        assert!(
            result
                .contains("Full output available at: /tmp/aptu-coder-overflow/slot-2/interleaved"),
            "should contain interleaved path hint: {result}"
        );
    }

    #[test]
    fn test_capture_hint_with_bytes() {
        // Arrange: non-zero byte count must be included in the hint
        // Act
        let hint = super::capture_hint("/tmp/slot-0/stdout", 1234);

        // Assert
        assert_eq!(
            hint,
            "output truncated; full capture at /tmp/slot-0/stdout (1234 bytes captured)\n"
        );
    }

    #[test]
    fn exec_command_error_subtype_as_str_golden_list() {
        // Arrange: every current variant, matched exhaustively so adding a new
        // variant without updating this arm fails to compile.
        for subtype in [
            ExecCommandErrorSubtype::WorkingDirNotDir,
            ExecCommandErrorSubtype::WorkingDirNotFound,
            ExecCommandErrorSubtype::CdPathNotDir,
            ExecCommandErrorSubtype::CdPathNotFound,
            ExecCommandErrorSubtype::StdinTooLarge,
            ExecCommandErrorSubtype::HeredocError,
        ] {
            // Act
            let expected = match subtype {
                ExecCommandErrorSubtype::WorkingDirNotDir => "working_dir_not_dir",
                ExecCommandErrorSubtype::WorkingDirNotFound => "working_dir_not_found",
                ExecCommandErrorSubtype::CdPathNotDir => "cd_path_not_dir",
                ExecCommandErrorSubtype::CdPathNotFound => "cd_path_not_found",
                ExecCommandErrorSubtype::StdinTooLarge => "stdin_too_large",
                ExecCommandErrorSubtype::HeredocError => "heredoc_error",
            };

            // Assert
            assert_eq!(subtype.as_str(), expected);
        }
    }
}
