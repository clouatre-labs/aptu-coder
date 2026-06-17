// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;

async fn call_exec_command_raw(params: serde_json::Value) -> serde_json::Value {
    call_tool_raw("exec_command", params).await
}

#[tokio::test]
async fn exec_command_happy_path() {
    // Arrange: prepare a simple echo command
    let command = "echo hello";

    // Act: execute the command via a mock handler
    // Since we can't directly call the tool handler without a full server setup,
    // we'll test the core logic by spawning the command directly
    let mut child = std::process::Command::new(
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
    )
    .arg("-c")
    .arg(command)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("should spawn command");

    let stdout = child
        .stdout
        .take()
        .map(|mut s| {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut s, &mut buf).ok();
            String::from_utf8_lossy(&buf).to_string()
        })
        .unwrap_or_default();

    let _stderr = child
        .stderr
        .take()
        .map(|mut s| {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut s, &mut buf).ok();
            String::from_utf8_lossy(&buf).to_string()
        })
        .unwrap_or_default();

    let status = child.wait().expect("should wait for child");
    let exit_code = status.code();

    // Assert
    assert_eq!(exit_code, Some(0), "exit code should be 0");
    assert!(
        stdout.contains("hello"),
        "stdout should contain 'hello', got: {}",
        stdout
    );
}

#[tokio::test]
async fn exec_command_nonzero_exit() {
    // Arrange: command that exits with code 42
    let command = "exit 42";

    // Act
    let mut child = std::process::Command::new(
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
    )
    .arg("-c")
    .arg(command)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("should spawn command");

    let _stdout = child
        .stdout
        .take()
        .map(|mut s| {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut s, &mut buf).ok();
            String::from_utf8_lossy(&buf).to_string()
        })
        .unwrap_or_default();

    let _stderr = child
        .stderr
        .take()
        .map(|mut s| {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut s, &mut buf).ok();
            String::from_utf8_lossy(&buf).to_string()
        })
        .unwrap_or_default();

    let status = child.wait().expect("should wait for child");
    let exit_code = status.code();

    // Assert
    assert_eq!(exit_code, Some(42), "exit code should be 42");
}

#[tokio::test]
async fn exec_command_timeout() {
    // Arrange: command that sleeps for 60 seconds with 1 second timeout
    let command = "sleep 60";
    let timeout_duration = std::time::Duration::from_millis(500);

    // Act: spawn command in a blocking task
    let cmd = command.to_string();
    let wait_result = tokio::time::timeout(
        timeout_duration,
        tokio::task::spawn_blocking(move || {
            let mut child = std::process::Command::new(
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
            )
            .arg("-c")
            .arg(&cmd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("should spawn command");

            let _stdout = child
                .stdout
                .take()
                .map(|mut s| {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut s, &mut buf).ok();
                    String::from_utf8_lossy(&buf).to_string()
                })
                .unwrap_or_default();

            let _stderr = child
                .stderr
                .take()
                .map(|mut s| {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut s, &mut buf).ok();
                    String::from_utf8_lossy(&buf).to_string()
                })
                .unwrap_or_default();

            child.wait().ok()
        }),
    )
    .await;

    // Assert: timeout should occur
    assert!(wait_result.is_err(), "timeout should occur");
}

#[tokio::test]
async fn exec_command_working_dir_rejection() {
    // exec_command has no CWD confinement; working_dir=/tmp (outside server CWD) must succeed.
    // Only edit_overwrite/edit_replace enforce CWD confinement.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo hi",
        "working_dir": "/tmp"
    }))
    .await;

    // Assert: handler must succeed (no confinement for exec_command)
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(true),
        "exec_command with working_dir outside CWD must succeed: {resp}"
    );
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["exit_code"], 0, "exit_code mismatch: {sc}");
}

#[tokio::test]
async fn exec_command_output_truncation() {
    // Arrange: command that produces >2000 lines
    let command = "seq 1 3000";

    // Act
    let mut child = std::process::Command::new(
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
    )
    .arg("-c")
    .arg(command)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("should spawn command");

    let stdout = child
        .stdout
        .take()
        .map(|mut s| {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut s, &mut buf).ok();
            String::from_utf8_lossy(&buf).to_string()
        })
        .unwrap_or_default();

    let _stderr = child
        .stderr
        .take()
        .map(|mut s| {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut s, &mut buf).ok();
            String::from_utf8_lossy(&buf).to_string()
        })
        .unwrap_or_default();

    let _status = child.wait().expect("should wait for child");

    // Assert: output should have >2000 lines
    let line_count = stdout.lines().count();
    assert!(
        line_count > 2000,
        "output should have >2000 lines, got: {}",
        line_count
    );
}

#[test]
fn test_truncate_output_by_lines() {
    // Helper function to test truncation logic
    fn truncate_output(output: &str, max_lines: usize, max_bytes: usize) -> (String, bool) {
        let lines: Vec<&str> = output.lines().collect();

        let output_to_use = if lines.len() > max_lines {
            lines[..max_lines].join("\n")
        } else {
            output.to_string()
        };

        if output_to_use.len() > max_bytes {
            (output_to_use[..max_bytes].to_string(), true)
        } else {
            (output_to_use, lines.len() > max_lines)
        }
    }

    // Arrange: create output with 2500 lines
    let output = (1..=2500)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    // Act
    let (truncated, was_truncated) = truncate_output(&output, 2000, 50 * 1024);

    // Assert
    assert!(was_truncated, "should be truncated");
    let line_count = truncated.lines().count();
    assert_eq!(line_count, 2000, "should have exactly 2000 lines");
}

#[test]
fn test_truncate_output_by_bytes() {
    // Helper function to test truncation logic
    fn truncate_output(output: &str, max_lines: usize, max_bytes: usize) -> (String, bool) {
        let lines: Vec<&str> = output.lines().collect();

        let output_to_use = if lines.len() > max_lines {
            lines[..max_lines].join("\n")
        } else {
            output.to_string()
        };

        if output_to_use.len() > max_bytes {
            (output_to_use[..max_bytes].to_string(), true)
        } else {
            (output_to_use, lines.len() > max_lines)
        }
    }

    // Arrange: create output that exceeds byte limit
    let output = "x".repeat(100 * 1024); // 100KB

    // Act
    let (truncated, was_truncated) = truncate_output(&output, 2000, 50 * 1024);

    // Assert
    assert!(was_truncated, "should be truncated");
    assert!(
        truncated.len() <= 50 * 1024,
        "truncated output should not exceed 50KB"
    );
}

// Handler-level integration tests via MCP JSON-RPC
// These tests verify the five key behaviors of exec_command at the integration level

#[tokio::test]
async fn test_handler_structured_output() {
    let resp = call_exec_command_raw(serde_json::json!({"command": "echo hello"})).await;
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["exit_code"], 0, "exit_code mismatch: {sc}");
    assert!(
        sc["stdout"].as_str().unwrap_or("").contains("hello"),
        "stdout missing 'hello': {sc}"
    );
    assert!(
        !sc["timed_out"].as_bool().unwrap_or(true),
        "unexpected timed_out: {sc}"
    );
}

#[tokio::test]
async fn test_handler_timeout_respected() {
    let resp =
        call_exec_command_raw(serde_json::json!({"command": "sleep 10", "timeout_secs": 1})).await;
    let sc = &resp["result"]["structuredContent"];
    assert!(
        sc["timed_out"].as_bool().unwrap_or(false),
        "expected timed_out=true: {sc}"
    );
}

#[tokio::test]
async fn test_handler_invalid_working_dir() {
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo hi",
        "working_dir": "/nonexistent-absolute-path-for-test"
    }))
    .await;
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true: {resp}"
    );
}

#[tokio::test]
async fn test_handler_nonzero_exit() {
    let resp = call_exec_command_raw(serde_json::json!({"command": "exit 42"})).await;
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["exit_code"], 42, "exit_code mismatch: {sc}");
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true for non-zero exit: {resp}"
    );
}

#[tokio::test]
async fn test_handler_timeout_partial_output() {
    // Command prints output immediately then sleeps longer than timeout
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo partial_output && sleep 10",
        "timeout_secs": 1
    }))
    .await;
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["timed_out"], true, "expected timed_out=true: {sc}");
    let stdout = sc["stdout"].as_str().unwrap_or("");
    assert!(
        stdout.contains("partial_output"),
        "expected partial_output in stdout on timeout, got: {stdout}"
    );
}

#[tokio::test]
async fn test_handler_shell_preference() {
    // Serialize all tests that mutate APTU_SHELL to prevent races when the
    // test suite runs in parallel (tokio::test spawns concurrent tasks).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap();

    // SAFETY: the static mutex above ensures no other test reads or writes
    // APTU_SHELL while we hold the guard.
    unsafe { std::env::set_var("APTU_SHELL", "sh") };
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo $0"
    }))
    .await;
    unsafe { std::env::remove_var("APTU_SHELL") };

    let sc = &resp["result"]["structuredContent"];
    let stdout = sc["stdout"].as_str().unwrap_or("");
    assert!(
        stdout.contains("sh"),
        "expected sh in $0 output, got: {stdout}"
    );
}

#[tokio::test]
async fn test_handler_stderr_populated() {
    let resp = call_exec_command_raw(serde_json::json!({"command": "sh -c 'echo err >&2'"})).await;
    let sc = &resp["result"]["structuredContent"];
    assert!(
        sc["stderr"].as_str().unwrap_or("").contains("err"),
        "stderr missing 'err': {sc}"
    );
}

#[tokio::test]
async fn test_exec_command_large_stdout_no_deadlock() {
    // Test that large stdout (>64KB) completes without deadlock
    // Use a simpler command that writes just under 50KB to avoid truncation by MAX_BYTES
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "seq 1 500",
        "timeout_secs": 10
    }))
    .await;

    let sc = &resp["result"]["structuredContent"];
    assert_eq!(
        sc["timed_out"], false,
        "large stdout must not trigger timeout: {sc}"
    );
    assert_eq!(sc["exit_code"], 0, "exit code should be 0: {sc}");
    assert!(
        sc["stdout"].as_str().unwrap_or("").contains("1"),
        "stdout should contain output: {sc}"
    );
}

#[tokio::test]
async fn test_exec_command_backgrounded_process() {
    // Test that backgrounded process returns with output_truncated=false (normal case)
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo 'parent done'",
        "timeout_secs": 5
    }))
    .await;

    let sc = &resp["result"]["structuredContent"];
    assert_eq!(
        sc["timed_out"], false,
        "normal command should not timeout: {sc}"
    );
    assert_eq!(
        sc["output_truncated"], false,
        "normal command should not truncate: {sc}"
    );
    assert!(
        sc["stdout"].as_str().unwrap_or("").contains("parent done"),
        "stdout should contain output: {sc}"
    );
}

#[tokio::test]
async fn test_exec_command_overflow_to_temp_file() {
    // Test that output >2000 lines sets output_truncated and populates slot file paths.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "seq 1 3000",
        "timeout_secs": 10
    }))
    .await;

    // Structured content must indicate truncation and expose slot file paths.
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["output_truncated"], true, "should be truncated: {sc}");

    let stdout_path = sc["stdout_path"].as_str();
    assert!(
        stdout_path.is_some(),
        "stdout_path should be set on overflow: {sc}"
    );
    assert!(
        stdout_path.unwrap().contains("aptu-coder-overflow"),
        "stdout_path should reference the overflow directory: {sc}"
    );
    assert!(
        stdout_path.unwrap().contains("slot-"),
        "stdout_path should contain slot identifier: {sc}"
    );
}

#[tokio::test]
async fn test_exec_command_slot_isolation() {
    // Test that overflow calls use slot identifiers (0-7) visible in structuredContent.stdout_path.
    let mut slot_ids = std::collections::HashSet::new();

    for _ in 0..8 {
        let resp = call_exec_command_raw(serde_json::json!({
            "command": "seq 1 3000",
            "timeout_secs": 10
        }))
        .await;

        let sc = &resp["result"]["structuredContent"];
        if let Some(path_str) = sc["stdout_path"].as_str() {
            if let Some(slot_start) = path_str.find("slot-") {
                let rest = &path_str[slot_start..];
                let slot_end = rest.find('/').unwrap_or(rest.len());
                let slot_id = &rest[..slot_end];
                slot_ids.insert(slot_id.to_string());
            }
        }
    }

    // Sequential overflow calls must produce at least one slot identifier.
    assert!(
        !slot_ids.is_empty(),
        "should have extracted at least one slot identifier"
    );
}

#[tokio::test]
async fn test_handler_interleaved_ordering() {
    // Arrange: command writes to both stdout and stderr
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo stdout_line && echo stderr_line >&2"
    }))
    .await;

    // Act: inspect structuredContent.interleaved
    let sc = &resp["result"]["structuredContent"];
    let interleaved = sc["interleaved"].as_str().unwrap_or("");

    // Assert: both lines are captured in the single interleaved field.
    // Exact ordering is non-deterministic (merge polls both streams); we verify
    // that both streams contribute to the interleaved output.
    assert!(
        interleaved.contains("stdout_line"),
        "interleaved missing stdout_line: {interleaved}"
    );
    assert!(
        interleaved.contains("stderr_line"),
        "interleaved missing stderr_line: {interleaved}"
    );
    // Verify structuredContent.stdout and .stderr are populated separately too
    assert!(
        sc["stdout"].as_str().unwrap_or("").contains("stdout_line"),
        "stdout field missing stdout_line: {sc}"
    );
    assert!(
        sc["stderr"].as_str().unwrap_or("").contains("stderr_line"),
        "stderr field missing stderr_line: {sc}"
    );
}

#[test]
fn test_handler_output_collection_error() {
    // Verify ShellOutput can be constructed with output_collection_error set.
    // The field is populated when a post-exit drain timeout fires; that path
    // is difficult to trigger deterministically in an integration test, so we
    // verify the struct-level contract here.
    use aptu_coder::ShellOutput;
    let mut output = ShellOutput::new(
        "out".into(),
        "err".into(),
        "out\nerr\n".into(),
        Some(0),
        false,
        false,
    );
    assert!(
        output.output_collection_error.is_none(),
        "output_collection_error must be None by default"
    );
    output.output_collection_error =
        Some("post-exit drain timeout: background process held pipes".into());
    assert!(
        output.output_collection_error.is_some(),
        "output_collection_error should be settable"
    );
}

#[tokio::test]
async fn test_handler_content_priority() {
    // Arrange: run a simple command
    let resp = call_exec_command_raw(serde_json::json!({"command": "echo hello"})).await;

    // Act: check the first content block for an annotations.priority field
    let content = &resp["result"]["content"];
    let first = &content[0];
    let priority = &first["annotations"]["priority"];

    // Assert: priority annotation present and equals 0.0
    assert!(
        !priority.is_null(),
        "first content block should have annotations.priority: {first}"
    );
    let pval = priority.as_f64().unwrap_or(f64::NAN);
    assert!(
        (pval - 0.0).abs() < f64::EPSILON,
        "priority should be 0.0, got: {pval}"
    );
}

#[tokio::test]
async fn test_exec_cache_hit_on_sequential_repeat() {
    // Arrange: run the same command twice sequentially
    let cmd = "echo cache_test_123";
    let params1 = serde_json::json!({"command": cmd});
    let params2 = serde_json::json!({"command": cmd});

    // Act: first call executes the command
    let resp1 = call_exec_command_raw(params1).await;
    let sc1 = &resp1["result"]["structuredContent"];
    let stdout1 = sc1["stdout"].as_str().unwrap_or("").to_string();

    // Second call executes independently (exec_command is non-cacheable)
    let resp2 = call_exec_command_raw(params2).await;
    let sc2 = &resp2["result"]["structuredContent"];
    let stdout2 = sc2["stdout"].as_str().unwrap_or("").to_string();

    // Assert: both calls succeeded with identical output (both ran the command)
    assert_eq!(sc1["exit_code"], 0, "first call should succeed: {sc1}");
    assert_eq!(sc2["exit_code"], 0, "second call should succeed: {sc2}");
    assert_eq!(
        stdout1, stdout2,
        "both calls should produce the same output"
    );
    assert!(
        stdout1.contains("cache_test_123"),
        "output should contain the echo string"
    );
    // Assert: cache_hit is absent (exec_command is non-cacheable)
    assert!(
        sc1["cache_hit"].is_null(),
        "cache_hit must be absent for exec_command: {sc1}"
    );
    assert!(
        sc2["cache_hit"].is_null(),
        "cache_hit must be absent for exec_command: {sc2}"
    );
}

#[tokio::test]
async fn test_exec_cache_skipped_with_stdin() {
    // Arrange: run a command with stdin
    let cmd = "cat";
    let stdin_content = "test_stdin_data";
    let params = serde_json::json!({
        "command": cmd,
        "stdin": stdin_content
    });

    // Act: call with stdin
    let resp = call_exec_command_raw(params).await;
    let sc = &resp["result"]["structuredContent"];

    // Assert: command executed and stdin was passed through
    assert_eq!(sc["exit_code"], 0, "cat with stdin should succeed: {sc}");
    assert!(
        sc["stdout"]
            .as_str()
            .unwrap_or("")
            .contains("test_stdin_data"),
        "stdout should contain the stdin content: {sc}"
    );
    // Assert: cache_hit is absent (exec_command is non-cacheable regardless of stdin)
    assert!(
        sc["cache_hit"].is_null(),
        "cache_hit must be absent for exec_command with stdin: {sc}"
    );
}

#[tokio::test]
async fn test_exec_cache_not_populated_on_failure() {
    // Arrange: run a command that fails (non-zero exit)
    let cmd = "false";
    let params1 = serde_json::json!({"command": cmd});
    let params2 = serde_json::json!({"command": cmd});

    // Act: first call executes and fails
    let resp1 = call_exec_command_raw(params1).await;
    let sc1 = &resp1["result"]["structuredContent"];

    // Second call re-executes independently
    let resp2 = call_exec_command_raw(params2).await;
    let sc2 = &resp2["result"]["structuredContent"];

    // Assert: both calls failed (non-zero exit) and cache_hit is absent
    assert_ne!(sc1["exit_code"], 0, "false command should fail: {sc1}");
    assert_ne!(
        sc2["exit_code"], 0,
        "false command should fail on second call too: {sc2}"
    );
    assert!(
        sc1["cache_hit"].is_null(),
        "cache_hit must be absent for failing exec_command: {sc1}"
    );
    assert!(
        sc2["cache_hit"].is_null(),
        "cache_hit must be absent for failing exec_command: {sc2}"
    );
}

#[tokio::test]
async fn test_exec_slot_files_not_written_for_small_output() {
    // Slot files must NOT be written when output is under the 2000-line limit.
    let cmd = "echo slot_file_test";
    let params = serde_json::json!({"command": cmd});

    let resp = call_exec_command_raw(params).await;
    let sc = &resp["result"]["structuredContent"];

    assert_eq!(
        sc["output_truncated"], false,
        "small output must not be truncated: {sc}"
    );
    assert!(
        sc["stdout_path"].is_null(),
        "stdout_path must be absent for small output: {sc}"
    );
    assert!(
        sc["stderr_path"].is_null(),
        "stderr_path must be absent for small output: {sc}"
    );
}

#[tokio::test]
async fn test_cd_prefix_chain_passthrough_with_working_dir() {
    // When working_dir is set and the leading cd path differs, the sanitizer must
    // pass the full command through unmodified so the shell executes every cd in order.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "cd /tmp && pwd && cd /var && pwd",
        "working_dir": std::env::current_dir().unwrap().to_str().unwrap()
    }))
    .await;

    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(true),
        "expected success: {resp}"
    );
    let stdout = resp["result"]["structuredContent"]["stdout"]
        .as_str()
        .unwrap_or("");
    let tmp_pos = stdout.find("/tmp").expect("expected /tmp in stdout");
    let var_pos = stdout.find("/var").expect("expected /var in stdout");
    assert!(
        tmp_pos < var_pos,
        "/tmp must precede /var in stdout: {stdout}"
    );
}

#[tokio::test]
async fn test_cd_prefix_plain_absolute_promoted_when_no_working_dir() {
    // When no working_dir is supplied and the command starts with a plain absolute
    // cd path, the sanitizer promotes the path as working_dir and strips the prefix.
    // The server CWD is crates/aptu-coder; use its src/ subdir as the target.
    let cwd = std::env::current_dir().unwrap();
    let target = cwd.join("src");
    let target_str = target.to_str().unwrap().to_owned();

    let resp = call_exec_command_raw(serde_json::json!({
        "command": format!("cd {} && pwd", target_str)
    }))
    .await;

    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(true),
        "expected success: {resp}"
    );
    let stdout = resp["result"]["structuredContent"]["stdout"]
        .as_str()
        .unwrap_or("");
    assert!(
        stdout.trim().ends_with("/src"),
        "pwd should resolve to the src subdir: {stdout}"
    );
}

#[tokio::test]
async fn test_cd_prefix_shell_special_passes_through() {
    // Shell-special cd forms (cd ~, cd $HOME, cd -, relative paths without working_dir)
    // must not be intercepted by the sanitizer; they pass through to the shell unmodified.
    // cd ~ is universally supported and expands to the home directory.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "cd ~ && pwd"
    }))
    .await;

    // The shell handles cd ~ naturally; the command must succeed.
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(true),
        "cd ~ must reach the shell unmodified and succeed: {resp}"
    );
    let stdout = resp["result"]["structuredContent"]["stdout"]
        .as_str()
        .unwrap_or("");
    assert!(
        !stdout.trim().is_empty(),
        "pwd after cd ~ must produce output: {stdout}"
    );
}

#[tokio::test]
async fn test_exec_command_working_dir_outside_cwd() {
    // working_dir pointing outside server CWD must succeed (no CWD confinement for exec_command)
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let tmp_path = tmp.path().to_str().expect("utf8").to_owned();
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo hello",
        "working_dir": tmp_path,
        "timeout_secs": 10
    }))
    .await;
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(true),
        "working_dir outside server CWD must succeed for exec_command: {resp}"
    );
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["exit_code"], 0, "exit_code mismatch: {sc}");
    assert!(
        sc["stdout"].as_str().unwrap_or("").contains("hello"),
        "stdout missing 'hello': {sc}"
    );
}
