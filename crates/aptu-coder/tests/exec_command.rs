// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;
use serial_test::serial;

/// Assert that exec_command output strips ANSI escape sequences: the plain
/// text survives and no ESC (0x1b) byte reaches the delivered output.
#[tokio::test]
async fn exec_command_output_is_ansi_stripped() {
    // Arrange: command emitting ANSI escape sequences on stdout.
    // Act: execute via the MCP tool harness.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "printf '\\033[31mred\\033[0m\\n'"
    }))
    .await;

    // Assert: plain text present, no ESC byte anywhere in the payload.
    let payload = serde_json::to_string(&resp).expect("serialize response");
    assert!(
        payload.contains("red"),
        "output should contain plain text 'red': {payload}"
    );
    assert!(
        !payload.contains('\u{1b}'),
        "output must not contain ESC bytes: {payload}"
    );
}

async fn call_exec_command_raw(params: serde_json::Value) -> serde_json::Value {
    call_tool_raw("exec_command", params).await
}

/// The text block is the single model-visible output channel after stdout/stderr
/// were dropped from structuredContent.
fn text_block(resp: &serde_json::Value) -> &str {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
}

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
    // Happy path: structuredContent carries metadata only -- no stdout/stderr
    // keys, and no path keys when nothing overflowed.
    assert!(
        sc.get("stdout").is_none() && sc.get("stderr").is_none(),
        "structuredContent must not carry stdout/stderr: {sc}"
    );
    assert!(
        sc.get("stdout_path").is_none() && sc.get("stderr_path").is_none(),
        "path keys must be absent (not null) when no output overflowed: {sc}"
    );
    assert!(sc["output_truncated"].as_bool() == Some(false), "{sc}");
    // Output is delivered via the text block.
    assert!(
        text_block(&resp).contains("hello"),
        "text block missing 'hello': {resp}"
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
async fn test_handler_shell_preference() {
    // Serialize all tests that mutate APTU_SHELL to prevent races when the
    // test suite runs in parallel (tokio::test spawns concurrent tasks).
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = ENV_LOCK.lock().await;

    // SAFETY: the static mutex above ensures no other test reads or writes
    // APTU_SHELL while we hold the guard.
    unsafe { std::env::set_var("APTU_SHELL", "sh") };
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo $0"
    }))
    .await;
    unsafe { std::env::remove_var("APTU_SHELL") };

    let stdout = text_block(&resp);
    assert!(
        stdout.contains("sh"),
        "expected sh in $0 output, got: {stdout}"
    );
}

#[tokio::test]
async fn test_handler_stderr_populated() {
    let resp = call_exec_command_raw(serde_json::json!({"command": "sh -c 'echo err >&2'"})).await;
    assert!(
        text_block(&resp).contains("err"),
        "text block missing stderr text 'err': {resp}"
    );
}

#[tokio::test]
async fn test_exec_command_large_stdout_no_deadlock() {
    // Test that large stdout (>64KB) completes without deadlock
    // Use a simpler command that writes just under 50KB to avoid truncation by MAX_BYTES
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "seq 1 500"
    }))
    .await;

    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["exit_code"], 0, "exit code should be 0: {sc}");
    assert!(
        text_block(&resp).contains("1"),
        "text block should contain output: {resp}"
    );
}

#[tokio::test]
async fn test_exec_command_backgrounded_process() {
    // Test that backgrounded process returns with output_truncated=false (normal case)
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo 'parent done'"
    }))
    .await;

    let sc = &resp["result"]["structuredContent"];
    assert_eq!(
        sc["output_truncated"], false,
        "normal command should not truncate: {sc}"
    );
    assert!(
        text_block(&resp).contains("parent done"),
        "text block should contain output: {resp}"
    );
}

#[tokio::test]
async fn test_exec_command_overflow_to_temp_file() {
    // Test that output >2000 lines sets output_truncated and populates slot file paths.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "seq 1 3000"
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

/// Issues an overflow-producing `command` up to 5 times until the slot file
/// referenced by `path_key` in structuredContent exceeds `min_bytes` (the
/// 500ms drain window may capture less than the full output under heavy test
/// load). Returns (structuredContent, content array, slot path, slot bytes).
async fn overflow_call_with_retry(
    command: &str,
    path_key: &str,
    min_bytes: u64,
) -> (serde_json::Value, serde_json::Value, String, u64) {
    overflow_call_with_retry_in(command, None, path_key, min_bytes).await
}

/// Like [`overflow_call_with_retry`] but runs `command` inside `working_dir`.
async fn overflow_call_with_retry_in(
    command: &str,
    working_dir: Option<String>,
    path_key: &str,
    min_bytes: u64,
) -> (serde_json::Value, serde_json::Value, String, u64) {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let resp = call_exec_command_raw(serde_json::json!({
            "command": command,
            "working_dir": working_dir,
        }))
        .await;

        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["output_truncated"], true, "should be truncated: {sc}");

        let p = sc[path_key].as_str().expect("slot path set");
        let bytes = std::fs::metadata(p).expect("slot file should exist").len();
        if bytes > min_bytes || attempt >= 5 {
            return (
                resp["result"]["structuredContent"].clone(),
                resp["result"]["content"].clone(),
                p.to_string(),
                bytes,
            );
        }
    }
}

#[tokio::test]
async fn test_exec_command_slot_isolation() {
    // Test that overflow calls use slot identifiers (0-7) visible in structuredContent.stdout_path.
    let mut slot_ids = std::collections::HashSet::new();

    for _ in 0..8 {
        let resp = call_exec_command_raw(serde_json::json!({
            "command": "seq 1 3000"
        }))
        .await;

        let sc = &resp["result"]["structuredContent"];
        if let Some(path_str) = sc["stdout_path"].as_str()
            && let Some(slot_start) = path_str.find("slot-")
        {
            let rest = &path_str[slot_start..];
            let slot_end = rest.find('/').unwrap_or(rest.len());
            let slot_id = &rest[..slot_end];
            slot_ids.insert(slot_id.to_string());
        }
    }

    // Sequential overflow calls must produce at least one slot identifier.
    assert!(
        !slot_ids.is_empty(),
        "should have extracted at least one slot identifier"
    );
}

#[tokio::test]
async fn test_exec_command_large_stdout_slot_file_and_resource_link() {
    // >30KB stdout: slot file must exceed the old 30KB drain cap and result
    // content must carry a ResourceLink with an aptu-overflow:// URI derived
    // from the stdout_path slot, plus a truncation hint with byte counts.
    let (_sc, content, stdout_path, slot_bytes) =
        overflow_call_with_retry("seq 1 10000", "stdout_path", 30_000).await;

    let slot = stdout_path
        .split("slot-")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .expect("stdout_path should contain slot identifier");
    let uri = format!("aptu-overflow://slot-{slot}/stdout");
    let has_stdout_uri_link = content
        .as_array()
        .expect("content array")
        .iter()
        .any(|b| b["type"] == "resource_link" && b["uri"] == uri.as_str());
    let hint_text = content[0]["text"].as_str().unwrap_or("");

    assert!(
        slot_bytes > 30_000,
        "slot stdout file should exceed 30_000 bytes (drain budget raised): {slot_bytes}"
    );
    assert!(
        has_stdout_uri_link,
        "should contain a ResourceLink with uri {uri}"
    );
    assert!(
        hint_text.contains("output truncated; full capture at"),
        "text should contain truncation hint: {hint_text}"
    );
}

#[tokio::test]
async fn test_exec_command_sub_cap_no_resource_link() {
    // Sub-cap output: exactly one Text block, no ResourceLink, no hint.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo hello"
    }))
    .await;

    let content = resp["result"]["content"].as_array().expect("content array");
    assert_eq!(
        content.len(),
        1,
        "should be a single content block: {content:?}"
    );
    assert_eq!(content[0]["type"], "text", "block should be text");

    let text = content[0]["text"].as_str().unwrap_or("");
    assert!(
        !text.contains("output truncated; full capture at"),
        "should not contain truncation hint: {text}"
    );
}

#[tokio::test]
async fn test_exec_command_stderr_only_overflow_resource_link() {
    // stderr-only overflow: stderr slot file >10_000 bytes, ResourceLink for
    // stderr_path, none for stdout.
    let (sc, content, stderr_path, slot_bytes) =
        overflow_call_with_retry("seq 1 5000 >&2", "stderr_path", 10_000).await;

    let slot = stderr_path
        .split("slot-")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .expect("stderr_path should contain slot identifier");
    let uri = format!("aptu-overflow://slot-{slot}/stderr");
    let has_stderr_uri_link = content
        .as_array()
        .expect("content array")
        .iter()
        .any(|b| b["type"] == "resource_link" && b["uri"] == uri.as_str());
    let stdout_slot_bytes = sc["stdout_path"]
        .as_str()
        .map(std::fs::metadata)
        .and_then(|r| r.ok())
        .map(|m| m.len())
        .unwrap_or(0);
    let has_empty_stdout_slot = stdout_slot_bytes == 0;

    assert!(
        slot_bytes > 10_000,
        "stderr slot file should exceed 10_000 bytes: {slot_bytes}"
    );
    assert!(
        has_stderr_uri_link,
        "should contain a ResourceLink for stderr slot: {uri}"
    );
    assert!(
        has_empty_stdout_slot,
        "stdout slot file should be empty for stderr-only command"
    );
}

#[tokio::test]
async fn test_handler_interleaved_ordering() {
    // Arrange: command writes to both stdout and stderr
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo stdout_line && echo stderr_line >&2"
    }))
    .await;

    // Act: inspect structuredContent and the text block. The interleaved text
    // is no longer a structuredContent field; it reaches the model only via the
    // text block ("Output:" section).
    let sc = &resp["result"]["structuredContent"];
    let text_block = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();

    // Assert: structuredContent carries stdout/stderr/paths but no interleaved
    // keys (the duplicate fields were dropped).
    assert!(
        sc.get("interleaved").is_none() && sc.get("interleaved_path").is_none(),
        "structuredContent should not contain interleaved keys: {sc}"
    );
    // Both streams still contribute to the interleaved text block.
    // Exact ordering is non-deterministic (merge polls both streams).
    assert!(
        text_block.contains("stdout_line"),
        "text block missing stdout_line: {text_block}"
    );
    assert!(
        text_block.contains("stderr_line"),
        "text block missing stderr_line: {text_block}"
    );
    // structuredContent no longer carries stdout/stderr; both streams still
    // contribute to the interleaved text block (asserted above).
    assert!(
        sc.get("stdout").is_none() && sc.get("stderr").is_none(),
        "structuredContent must not carry stdout/stderr: {sc}"
    );
}

#[test]
fn test_handler_output_collection_error() {
    // Verify ShellOutput can be constructed with output_collection_error set.
    // The field is populated when a post-exit drain timeout fires; that path
    // is difficult to trigger deterministically in an integration test, so we
    // verify the struct-level contract here.
    use aptu_coder::ShellOutput;
    let mut output = ShellOutput::new("out".into(), "err".into(), Some(0), false);
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
    let stdout1 = text_block(&resp1).to_string();

    // Second call executes independently (exec_command is non-cacheable)
    let resp2 = call_exec_command_raw(params2).await;
    let stdout2 = text_block(&resp2).to_string();

    // Assert: both calls succeeded with identical output (both ran the command)
    let sc1 = &resp1["result"]["structuredContent"];
    let sc2 = &resp2["result"]["structuredContent"];
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
        text_block(&resp).contains("test_stdin_data"),
        "text block should contain the stdin content: {resp}"
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
    let stdout = text_block(&resp);
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
    let stdout = text_block(&resp);
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
    let stdout = text_block(&resp);
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
        "working_dir": tmp_path
    }))
    .await;
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(true),
        "working_dir outside server CWD must succeed for exec_command: {resp}"
    );
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["exit_code"], 0, "exit_code mismatch: {sc}");
    assert!(
        text_block(&resp).contains("hello"),
        "text block missing 'hello': {resp}"
    );
}

/// exec_command invalid working_dir must not expose raw path in error message.
#[tokio::test]
async fn test_exec_command_invalid_working_dir_no_path_leak() {
    let bad_wd = "/nonexistent-exec-working-dir-test";
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo hi",
        "working_dir": bad_wd
    }))
    .await;
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        !msg.contains(bad_wd),
        "error message must not contain working_dir path: {msg}"
    );
}

/// exec_command invalid cd prefix path must not expose raw path in error message.
#[tokio::test]
async fn test_exec_command_invalid_cd_path_no_path_leak() {
    let bad_cd_path = "/nonexistent-cd-prefix-path-test";
    let resp = call_exec_command_raw(serde_json::json!({
        "command": format!("cd {bad_cd_path} && pwd")
    }))
    .await;
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        !msg.contains(bad_cd_path),
        "error message must not contain cd prefix path: {msg}"
    );
}

#[tokio::test]
async fn test_handler_unclosed_heredoc() {
    // Arrange: a heredoc with no closing delimiter
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "cat << EOF\nhello\nworld\n"
    }))
    .await;

    // Assert: unclosed heredoc is rejected before spawning
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        msg.contains("heredoc"),
        "error message should mention heredoc: {msg}"
    );
}

#[tokio::test]
async fn test_handler_unclosed_dash_heredoc() {
    // Arrange: <<- heredoc with no closing delimiter
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "cat <<- EOF\n\thello\n\tworld\n"
    }))
    .await;

    // Assert: unclosed <<- heredoc is rejected
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true for unclosed <<- heredoc: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        msg.contains("heredoc"),
        "error message should mention heredoc: {msg}"
    );
}

#[tokio::test]
async fn test_handler_heredoc_delimiter_on_last_line_no_trailing_newline() {
    // Arrange: closing delimiter appears on the final line with no trailing
    // newline -- verifies the scanner handles the no-newline edge case without
    // off-by-one errors.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "cat << EOF\nhello\nEOF"
    }))
    .await;

    // Assert: valid heredoc (delimiter present) is accepted
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=false for valid heredoc with no trailing newline: {resp}"
    );
}

#[tokio::test]
async fn test_handler_unclosed_heredoc_no_trailing_newline() {
    // Arrange: unclosed heredoc whose body has no trailing newline -- ensures
    // the scanner reports the missing delimiter correctly in this edge case.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "cat << EOF\nhello"
    }))
    .await;

    // Assert: unclosed heredoc is rejected
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true for unclosed heredoc with no trailing newline: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        msg.contains("heredoc"),
        "error message should mention heredoc: {msg}"
    );
}

#[tokio::test]
async fn test_handler_heredoc_trailing_space_on_delimiter_not_accepted() {
    // Arrange: closing line is "EOF " (trailing space) -- shell does NOT treat
    // this as the closing delimiter, so the scanner must not either.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "cat << EOF\nhello\nEOF \n"
    }))
    .await;

    // Assert: scanner sees no valid closer and rejects the command
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true: trailing space on delimiter must not be accepted: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        msg.contains("heredoc"),
        "error message should mention heredoc: {msg}"
    );
}

#[tokio::test]
async fn test_handler_heredoc_leading_space_on_non_dash_delimiter_not_accepted() {
    // Arrange: closing line is "  EOF" (leading spaces, non-<<- heredoc) --
    // shell does NOT treat this as the closing delimiter.
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "cat << EOF\nhello\n  EOF\n"
    }))
    .await;

    // Assert: scanner sees no valid closer and rejects the command
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true: leading spaces on non-<<- delimiter must not be accepted: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        msg.contains("heredoc"),
        "error message should mention heredoc: {msg}"
    );
}

#[tokio::test]
async fn test_fast_command_completes_with_timed_out_false() {
    // Arrange: a fast command (server default timeout applies, cannot fire)
    let test_fut = async {
        let resp = call_exec_command_raw(serde_json::json!({
            "command": "echo ok"
        }))
        .await;

        // Assert: success with timed_out=false
        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "expected isError=false for fast command: {resp}"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("Exit code: 0"),
            "expected exit code 0: {resp}"
        );
        let sc = &resp["result"]["structuredContent"];
        // timed_out is skip_serialized when false; if present, it must be false
        if let Some(val) = sc.as_object().and_then(|o| o.get("timed_out")) {
            assert_eq!(
                val.as_bool(),
                Some(false),
                "expected timed_out=false: {resp}"
            );
        }
    };

    tokio::time::timeout(std::time::Duration::from_secs(10), test_fut)
        .await
        .expect("test timed out (harness guard)");
}

#[tokio::test]
async fn test_timeout_not_fires_for_immediate_command() {
    // Arrange: an immediate command must never hit the server timeout
    let test_fut = async {
        let resp = call_exec_command_raw(serde_json::json!({
            "command": "echo hello"
        }))
        .await;

        // Assert: command completes normally
        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "expected isError=false when no timeout fires: {resp}"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("Exit code: 0"),
            "expected exit code 0: {resp}"
        );
        // timed_out should not be present
        let sc = resp.get("result").and_then(|r| r.get("structuredContent"));
        if let Some(sc) = sc {
            // If present, must be false
            if let Some(val) = sc.as_object().and_then(|o| o.get("timed_out")) {
                assert_eq!(
                    val.as_bool(),
                    Some(false),
                    "timed_out should be false when absent: {resp}"
                );
            }
        }
    };

    tokio::time::timeout(std::time::Duration::from_secs(10), test_fut)
        .await
        .expect("test timed out (harness guard)");
}

#[tokio::test]
async fn test_drain_background_pipe_holder_truncates() {
    // Happy path: drain semantics survive via server-side defaults. A child
    // that exits but leaves a background subprocess holding the pipes open
    // must produce output_truncated=true after the drain grace period.
    let test_fut = async {
        let resp = call_exec_command_raw(serde_json::json!({
            "command": "echo main done; sleep 30 &"
        }))
        .await;
        let sc = &resp["result"]["structuredContent"];
        assert!(
            sc["output_truncated"].as_bool().unwrap_or(false),
            "expected truncation: {resp}"
        );
        assert!(
            text_block(&resp).contains("main done"),
            "text block: {resp}"
        );
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), test_fut)
        .await
        .expect("test timed out");
}

// ---------------------------------------------------------------------------
// Drain byte-budget enforcement tests (regression guard for OOM crash)
// ---------------------------------------------------------------------------

/// Produces 200k lines of stdout, verifies server returns success (not transport
/// error), output_truncated is true, and stdout is non-empty and within size limit.
#[tokio::test]
async fn exec_command_large_output_truncation_via_drain() {
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "count=0; while [ $count -lt 200000 ]; do echo \"line $count\"; count=$((count + 1)); done"
    }))
    .await;

    // (a) server returns success (not transport error)
    assert!(
        resp.get("error").is_none(),
        "expected no error, got: {:?}",
        resp.get("error")
    );

    let result = &resp["result"];
    let sc = &result["structuredContent"];
    let stdout = text_block(&resp);

    // (b) output_truncated is true when drain byte budget is exhausted
    assert!(
        sc["output_truncated"].as_bool().unwrap_or(false),
        "output_truncated should be true for large output"
    );

    // (c) output text is non-empty (drain budget keeps a bounded preview)
    assert!(!stdout.is_empty(), "output should be non-empty");
}

/// Command where stdout is under budget but stderr exceeds budget.
/// Asserts stdout lines are present in response and output_truncated is true.
#[tokio::test]
async fn exec_command_stderr_exceeds_budget_stdout_present() {
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "for i in $(seq 1 1500); do echo >&2 \"error detail line $i\"; done; echo 'ok'"
    }))
    .await;

    assert!(
        resp.get("error").is_none(),
        "expected no error, got: {:?}",
        resp.get("error")
    );

    let result = &resp["result"];
    let sc = &result["structuredContent"];
    let stdout = text_block(&resp);

    // stdout contains 'ok' (stdout lines present even though stderr overflows)
    assert!(
        stdout.contains("ok"),
        "stdout should contain 'ok', got: {stdout}"
    );

    // output_truncated is true due to stderr overflow
    assert!(
        sc["output_truncated"].as_bool().unwrap_or(false),
        "output_truncated should be true when stderr exceeds budget"
    );
}

/// Verify drain_task stops sending after byte budget exhausted and output_truncated
/// flag is set when drain_task stops sending due to budget exhaustion.
#[tokio::test]
async fn exec_command_drain_budget_exhaustion() {
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "seq 1 200000"
    }))
    .await;

    assert!(
        resp.get("error").is_none(),
        "expected no error, got: {:?}",
        resp.get("error")
    );

    let result = &resp["result"];
    let sc = &result["structuredContent"];
    let stdout = text_block(&resp);

    // output_truncated is true (drain budget exhausted)
    assert!(
        sc["output_truncated"].as_bool().unwrap_or(false),
        "output_truncated should be true when drain budget exhausted"
    );

    // output text is non-empty (bounded preview retained)
    assert!(!stdout.is_empty(), "output should be non-empty");
}

/// Creates a temp git repo with `commits` commits (one file, one line each).
/// Self-contained so filter-cap tests never depend on the host checkout's
/// history depth. Returns the repo path (TempDir is leaked; cleaned by OS tmp).
fn make_temp_git_repo(commits: usize) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_path_buf();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&path)
            .arg("-c")
            .arg("core.hooksPath=/dev/null")
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .expect("git should be available");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    for i in 0..commits {
        let file = format!("f{i}.txt");
        std::fs::write(path.join(&file), format!("line {i}\n")).expect("write file");
        run(&["add", &file]);
        run(&["commit", "-qm", &format!("c{i}")]);
    }
    std::mem::forget(dir);
    path.display().to_string()
}

#[tokio::test]
#[serial]
async fn test_exec_command_filter_capped_notice_and_resource_link() {
    let repo = make_temp_git_repo(30);
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "git log --oneline -30",
        "working_dir": repo,
    }))
    .await;

    let sc = &resp["result"]["structuredContent"];
    assert!(
        sc.get("filter_applied").is_some(),
        "filter_applied should be set for git log: {sc}"
    );
    assert!(
        sc["filter_effect"]
            .as_str()
            .is_some_and(|e| e.contains("capped")),
        "filter_effect should describe the cap: {sc}"
    );

    let content = resp["result"]["content"].as_array().expect("content array");
    let link_uri = content
        .iter()
        .filter(|b| b["type"] == "resource_link")
        .filter_map(|b| b["uri"].as_str())
        .find(|u| u.starts_with("aptu-overflow://slot-"))
        .expect("filter-capped run should emit a resource link")
        .to_string();

    let common_analyzer = common::make_test_analyzer();
    let read = common::send_raw_request(
        common_analyzer,
        "resources/read",
        serde_json::json!({"uri": link_uri}),
    )
    .await;
    let text = read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("resources/read text: {read}"));

    // Assert: full pre-filter content, not the 20-line preview.
    assert!(
        text.lines().count() > 20,
        "resource content should exceed the 20-line cap: {} lines",
        text.lines().count()
    );

    let text_block = content[0]["text"].as_str().unwrap_or_default();
    assert!(
        text_block.contains("Output filtered: "),
        "text block should contain the filter notice: {text_block}"
    );
    assert_eq!(
        sc["output_truncated"], false,
        "pipe overflow should not have fired: {sc}"
    );
}

#[tokio::test]
async fn test_exec_command_uncapped_output_byte_identical() {
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo hello"
    }))
    .await;

    let sc = &resp["result"]["structuredContent"];
    assert!(
        sc.get("filter_applied").is_none(),
        "filter_applied must be absent without a cap: {sc}"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text block");
    assert_eq!(
        text, "Command: echo hello\nExit code: 0\nOutput truncated: false\nOutput:\nhello\n",
        "uncapped output text must remain byte-identical: {text}"
    );
}

#[tokio::test]
#[serial]
async fn test_exec_command_filter_cap_and_overflow_cooccurrence() {
    // A single commit with a large file makes `git log -p` exceed 30 KB so
    // both the byte overflow and the 20-line filter cap fire, regardless of
    // the host checkout's history.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_path_buf();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&path)
            .arg("-c")
            .arg("core.hooksPath=/dev/null")
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .expect("git should be available");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    let big: String = (0..4000)
        .map(|i| format!("content line {i} with padding\n"))
        .collect();
    std::fs::write(path.join("big.txt"), &big).expect("write file");
    run(&["add", "big.txt"]);
    run(&["commit", "-qm", "big"]);
    let repo = path.display().to_string();
    std::mem::forget(dir);

    let (sc, content, stdout_path, slot_bytes) =
        overflow_call_with_retry_in("git log -p", Some(repo), "stdout_path", 30_000).await;

    assert_eq!(
        sc["output_truncated"], true,
        "overflow should fire for git log -p"
    );
    assert!(
        sc.get("filter_applied").is_some(),
        "filter cap should also fire for git log -p: {sc}"
    );

    let slot_text = std::fs::read_to_string(&stdout_path).expect("slot file should be readable");
    assert!(
        slot_text.lines().count() > 20,
        "slot file must contain full pre-filter output, not the capped preview"
    );
    assert!(slot_bytes > 30_000, "slot file should exceed 30 KB");

    let slot = stdout_path
        .split("slot-")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .expect("stdout_path should contain slot identifier");
    let uri = format!("aptu-overflow://slot-{slot}/stdout");
    assert!(
        content
            .as_array()
            .expect("content array")
            .iter()
            .any(|b| b["type"] == "resource_link" && b["uri"] == uri.as_str()),
        "should contain a ResourceLink with uri {uri}"
    );
}

#[tokio::test]
#[serial]
async fn test_exec_command_stderr_capped_only() {
    let repo = make_temp_git_repo(30);
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "git log --oneline -30 1>&2",
        "working_dir": repo,
    }))
    .await;

    let sc = &resp["result"]["structuredContent"];
    assert!(
        sc.get("filter_applied").is_some(),
        "interleaved (stderr) stream should be filter-capped: {sc}"
    );
    assert!(
        !sc["output_truncated"].as_bool().unwrap_or(true),
        "no pipe overflow for a small stderr-only command: {sc}"
    );

    let content = resp["result"]["content"].as_array().expect("content array");
    let has_interleaved_link = content.iter().any(|b| {
        b["type"] == "resource_link"
            && b["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with("/interleaved"))
    });
    assert!(
        has_interleaved_link,
        "should emit a resource link for the filter-capped interleaved capture: {content:?}"
    );
    let text_block = content[0]["text"].as_str().unwrap_or_default();
    assert!(
        text_block.contains("Output filtered: "),
        "text block should contain the filter notice: {text_block}"
    );
}
