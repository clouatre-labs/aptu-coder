// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Runtime execution helpers for the `exec_command` tool.
//!
//! Extracted functions for building commands, running with timeout, and handling output persistence.

use std::sync::Arc;

use crate::ShellOutput;
use crate::filters::CompiledRule;

/// Default drain timeout in milliseconds for post-exit pipe drain (500ms).
pub(crate) const DEFAULT_DRAIN_TIMEOUT_MS: u64 = 500;

/// Max bytes to buffer from child stdout during drain (matches handle_output_persist cap).
pub(crate) const MAX_DRAIN_STDOUT_BYTES: usize = 30_000;

/// Max bytes to buffer from child stderr during drain (matches handle_output_persist cap).
pub(crate) const MAX_DRAIN_STDERR_BYTES: usize = 10_000;

/// Result of a timed command execution.
pub(crate) struct ExecutionResult {
    pub(crate) exit_code: Option<i32>,
    pub(crate) output_truncated: bool,
    pub(crate) output_collection_error: Option<String>,
    pub(crate) timed_out: bool,
    pub(crate) byte_truncated: bool,
    pub(crate) raw_stdout_bytes: u64,
    pub(crate) raw_stderr_bytes: u64,
}

/// Builds a tokio::process::Command with the given parameters.
pub(crate) fn build_exec_command(
    command: &str,
    working_dir_path: Option<&std::path::PathBuf>,
    stdin_present: bool,
    resolved_path: Option<&str>,
) -> tokio::process::Command {
    let shell = crate::shell::resolve_shell();
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

/// Runs a child process with optional timeout and drains output with a grace period.
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

        let mut byte_budget_hit = false;
        let mut so_bytes = 0usize;
        let mut se_bytes = 0usize;
        let mut raw_so = 0usize;
        let mut raw_se = 0usize;

        match (so_stream, se_stream) {
            (Some(so), Some(se)) => {
                let mut merged = so.merge(se);
                while let Some(Ok((is_stderr, line))) = merged.next().await {
                    // entry_len approximates the on-wire byte count: the line content
                    // plus the newline stripped by LinesStream. This matches the
                    // byte_budget_hit accounting below.
                    let entry_len = line.len() + 1; // +1 for newline
                    if is_stderr {
                        raw_se += entry_len;
                        if se_bytes + entry_len > MAX_DRAIN_STDERR_BYTES {
                            byte_budget_hit = true;
                            // Continue reading to drain child pipe; do not send.
                            continue;
                        }
                        se_bytes += entry_len;
                    } else {
                        raw_so += entry_len;
                        if so_bytes + entry_len > MAX_DRAIN_STDOUT_BYTES {
                            byte_budget_hit = true;
                            continue;
                        }
                        so_bytes += entry_len;
                    }
                    let _ = tx.send((is_stderr, line));
                }
            }
            (Some(so), None) => {
                let mut stream = so;
                while let Some(Ok((_, line))) = stream.next().await {
                    // entry_len approximates the on-wire byte count: the line content
                    // plus the newline stripped by LinesStream. This matches the
                    // byte_budget_hit accounting below.
                    let entry_len = line.len() + 1;
                    raw_so += entry_len;
                    if so_bytes + entry_len > MAX_DRAIN_STDOUT_BYTES {
                        byte_budget_hit = true;
                        continue;
                    }
                    so_bytes += entry_len;
                    let _ = tx.send((false, line));
                }
            }
            (None, Some(se)) => {
                let mut stream = se;
                while let Some(Ok((_, line))) = stream.next().await {
                    // entry_len approximates the on-wire byte count: the line content
                    // plus the newline stripped by LinesStream. This matches the
                    // byte_budget_hit accounting below.
                    let entry_len = line.len() + 1;
                    raw_se += entry_len;
                    if se_bytes + entry_len > MAX_DRAIN_STDERR_BYTES {
                        byte_budget_hit = true;
                        continue;
                    }
                    se_bytes += entry_len;
                    let _ = tx.send((true, line));
                }
            }
            (None, None) => {}
        }

        (byte_budget_hit, raw_so, raw_se)
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
            let (drain_truncated, byte_truncated, raw_so, raw_se) = if timed_out {
                drain_abort.abort();
                (false, false, 0, 0)
            } else {
                match tokio::time::timeout(drain_timeout, drain_task).await {
                    Ok(Ok((budget_hit, rso, rse))) => (false, budget_hit, rso, rse),
                    Ok(Err(join_err)) => {
                        // Task panicked: treat as no budget truncation, log warning.
                        tracing::warn!("drain_task panicked: {join_err}");
                        (false, false, 0, 0)
                    }
                    Err(_) => {
                        drain_abort.abort();
                        (true, false, 0, 0)
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
                byte_truncated,
                raw_stdout_bytes: raw_so as u64,
                raw_stderr_bytes: raw_se as u64,
            }
        }
        _ => {
            // No user timeout: wait for child exit first, then drain buffered output
            // with a short grace period (drain_timeout) for background subprocesses.
            let exit_status = child.wait().await.ok();
            let drain_result = tokio::time::timeout(drain_timeout, drain_task).await;

            let (drain_truncated, byte_truncated, raw_so, raw_se) = match drain_result {
                Ok(Ok((budget_hit, rso, rse))) => (false, budget_hit, rso, rse),
                Ok(Err(join_err)) => {
                    tracing::warn!("drain_task panicked: {join_err}");
                    (false, false, 0, 0)
                }
                Err(_) => {
                    drain_abort.abort();
                    (true, false, 0, 0)
                }
            };
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
                byte_truncated,
                raw_stdout_bytes: raw_so as u64,
                raw_stderr_bytes: raw_se as u64,
            }
        }
    }
}

/// Runs the command with stdin, timeout, and output collection.
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
) -> (ShellOutput, u64, u64) {
    let mut cmd = build_exec_command(
        &command,
        working_dir_path.as_ref(),
        stdin.is_some(),
        resolved_path,
    );

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                ShellOutput::new(
                    String::new(),
                    format!("failed to spawn command: {e}"),
                    format!("failed to spawn command: {e}"),
                    None,
                    false,
                ),
                0,
                0,
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
    let mut output_truncated = exec_result.output_truncated || exec_result.byte_truncated;
    let output_collection_error = exec_result.output_collection_error;
    let timed_out = exec_result.timed_out;
    let raw_stdout_bytes = exec_result.raw_stdout_bytes;
    let raw_stderr_bytes = exec_result.raw_stderr_bytes;

    rx.close();

    let mut lines: Vec<(bool, String)> = Vec::new();
    while let Some(item) = rx.recv().await {
        lines.push(item);
    }

    // Split tagged lines into stdout, stderr, interleaved post-facto (no locks needed).
    const MAX_BYTES: usize = 50 * 1024;
    const INTERLEAVED_MAX_BYTES: usize = 60 * 1024;
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

    let slot = seq;
    let (stdout, stderr, stdout_path, stderr_path, byte_truncated) =
        handle_output_persist(stdout_str, stderr_str, slot);
    output_truncated = output_truncated || stdout_path.is_some() || byte_truncated;

    // Handle interleaved overflow: cap at INTERLEAVED_MAX_BYTES and write to slot file if needed
    let (interleaved_preview, interleaved_path) =
        persist_interleaved_overflow(interleaved_str, INTERLEAVED_MAX_BYTES, slot).await;
    if interleaved_path.is_some() {
        output_truncated = true;
    }

    let mut output = ShellOutput::new(
        stdout,
        stderr,
        interleaved_preview,
        exit_code,
        output_truncated,
    );
    output.output_collection_error = output_collection_error;
    output.stdout_path = stdout_path;
    output.stderr_path = stderr_path;
    output.interleaved_path = interleaved_path;
    output.timed_out = timed_out;

    // Apply filter if exit_code == 0
    if exit_code == Some(0) {
        for compiled_rule in filter_table.iter() {
            if compiled_rule.pattern.is_match(&command) {
                let filtered_stdout = crate::filters::apply_filter(compiled_rule, &output.stdout);
                output.stdout = filtered_stdout;
                // Also filter interleaved: the response handler prefers interleaved when
                // non-empty (which it always is for commands that write to both streams),
                // so filtering only stdout would leave the LLM-visible output unfiltered.
                // apply_filter is called separately on each field; there is no double-filtering
                // because stdout and interleaved are independent strings assembled from the
                // same source lines -- updating one does not affect the other.
                output.interleaved =
                    crate::filters::apply_filter(compiled_rule, &output.interleaved);
                output.filter_applied = compiled_rule
                    .rule
                    .description
                    .clone()
                    .or_else(|| Some(compiled_rule.rule.match_command.clone()));
                break;
            }
        }
    }

    (output, raw_stdout_bytes, raw_stderr_bytes)
}

/// Handles output persistence by writing to slot files only when output overflows the line limit.
/// Writes full stdout/stderr to:
///   {temp_dir}/aptu-coder-overflow/slot-{slot}/{stdout,stderr}
/// Persists interleaved output to a slot file when it exceeds `max_bytes`.
/// Returns `(preview, path)`: on overflow, `preview` is a tail of `max_bytes` chars and
/// `path` is `Some(slot_file_path)`; under limit, returns the original string and `None`.
pub(crate) async fn persist_interleaved_overflow(
    interleaved: String,
    max_bytes: usize,
    slot: u32,
) -> (String, Option<String>) {
    if interleaved.len() <= max_bytes {
        return (interleaved, None);
    }
    let base = std::env::temp_dir()
        .join("aptu-coder-overflow")
        .join(format!("slot-{slot}"));
    let _ = tokio::fs::create_dir_all(&base).await;
    let interleaved_file = base.join("interleaved");
    let _ = tokio::fs::write(&interleaved_file, interleaved.as_bytes()).await;
    let path = interleaved_file.display().to_string();
    // Tail preview: show the most recent output, respecting char boundaries.
    let tail_start = interleaved.len().saturating_sub(max_bytes);
    let safe_start = interleaved.floor_char_boundary(tail_start);
    (interleaved[safe_start..].to_string(), Some(path))
}

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
        let safe_start = stdout.floor_char_boundary(tail_start);
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
        let safe_start = stderr.floor_char_boundary(tail_start);
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
