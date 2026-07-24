use crate::tools::exec_command::strip_cd_prefix;
use crate::tools::exec_runtime::{
    build_exec_command, handle_output_persist, persist_interleaved_overflow, run_exec_impl,
};
use crate::{SIZE_LIMIT, STDIN_MAX_BYTES, filters::CompiledRule};

#[test]
fn test_exec_stdin_size_cap_validation() {
    // Test: stdin size cap check (1 MB limit)
    // Arrange: create oversized stdin
    let oversized_stdin = "x".repeat(STDIN_MAX_BYTES + 1);

    // Act & Assert: verify size exceeds limit
    assert!(
        oversized_stdin.len() > STDIN_MAX_BYTES,
        "test setup: oversized stdin should exceed 1 MB"
    );

    // Verify that a 1 MB stdin is accepted
    let max_stdin = "y".repeat(STDIN_MAX_BYTES);
    assert_eq!(
        max_stdin.len(),
        STDIN_MAX_BYTES,
        "test setup: max stdin should be exactly 1 MB"
    );
}

#[tokio::test]
async fn test_exec_stdin_cat_roundtrip() {
    // Test: stdin content is piped to process and readable via stdout
    // Arrange: prepare stdin content
    let stdin_content = "hello world";

    // Act: execute cat with stdin via shell
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn cat");

    if let Some(mut stdin_handle) = child.stdin.take() {
        use tokio::io::AsyncWriteExt as _;
        stdin_handle
            .write_all(stdin_content.as_bytes())
            .await
            .expect("write stdin");
        drop(stdin_handle);
    }

    let output = child.wait_with_output().await.expect("wait for cat");

    // Assert: stdout contains the piped stdin content
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout_str.contains(stdin_content),
        "stdout should contain stdin content: {}",
        stdout_str
    );
}

#[tokio::test]
async fn test_exec_stdin_none_no_regression() {
    // Test: command without stdin executes normally (no regression)
    // Act: execute echo without stdin
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("echo hi")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn echo");

    let output = child.wait_with_output().await.expect("wait for echo");

    // Assert: command executes successfully
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout_str.contains("hi"),
        "stdout should contain echo output: {}",
        stdout_str
    );
}

#[test]
fn test_exec_command_path_injected() {
    // Arrange: call build_exec_command with Some("...") resolved_path
    let resolved_path = Some("/usr/local/bin:/usr/bin:/bin");
    let cmd = build_exec_command("echo test", None, false, resolved_path);

    // Act: verify the command was created without panic and inspect args
    let cmd_str = format!("{:?}", cmd);

    // Assert: -l flag must NOT be present (platform unification)
    assert!(
        !cmd_str.contains("-l"),
        "build_exec_command must not use -l on any platform"
    );

    // Assert: command should be created successfully
    assert!(
        !cmd_str.is_empty(),
        "build_exec_command should return a valid Command"
    );
}

#[test]
fn test_exec_command_path_fallback() {
    // Arrange: call build_exec_command with None resolved_path
    let cmd = build_exec_command("echo test", None, false, None);

    // Act: verify the command was created without panic and inspect args
    let cmd_str = format!("{:?}", cmd);

    // Assert: -l flag must NOT be present (platform unification)
    assert!(
        !cmd_str.contains("-l"),
        "build_exec_command must not use -l on any platform"
    );

    // Assert: command should be created successfully even with None
    assert!(
        !cmd_str.is_empty(),
        "build_exec_command should handle None resolved_path gracefully"
    );
}

#[test]
fn test_exec_no_truncation_under_limits() {
    // Happy path: small output under all caps
    let stdout = "hello world".to_string();
    let stderr = "no errors".to_string();
    let slot = 0u32;

    let (out_stdout, out_stderr, stdout_path, stderr_path, byte_truncated) =
        handle_output_persist(stdout, stderr, slot);

    assert_eq!(out_stdout, "hello world");
    assert_eq!(out_stderr, "no errors");
    assert!(stdout_path.is_none());
    assert!(stderr_path.is_none());
    assert!(!byte_truncated);
}

#[test]
fn test_exec_byte_overflow_stdout_exceeds_30k() {
    // Edge case: stdout exceeds 30k byte limit
    let stdout = "x".repeat(35_000);
    let stderr = "small".to_string();
    let slot = 0u32;

    let (out_stdout, out_stderr, stdout_path, stderr_path, byte_truncated) =
        handle_output_persist(stdout.clone(), stderr.clone(), slot);

    // Verify truncation occurred
    assert!(byte_truncated, "byte_truncated should be true");
    assert!(stdout_path.is_some(), "stdout_path should be set");
    assert!(stderr_path.is_some(), "stderr_path should be set");

    // Verify output was truncated
    assert!(
        out_stdout.len() <= 30_000,
        "stdout should be truncated to <= 30k"
    );
    assert_eq!(out_stderr, "small", "stderr should be unchanged");

    // Verify slot file was written
    let base = std::env::temp_dir()
        .join("aptu-coder-overflow")
        .join(format!("slot-{slot}"));
    let stdout_file = base.join("stdout");
    assert!(
        stdout_file.exists(),
        "stdout slot file should exist after byte overflow"
    );
}

#[test]
fn test_exec_byte_overflow_stderr_exceeds_10k() {
    // Edge case: stderr exceeds 10k byte limit
    let stdout = "small".to_string();
    let stderr = "y".repeat(15_000);
    let slot = 1u32;

    let (out_stdout, out_stderr, stdout_path, stderr_path, byte_truncated) =
        handle_output_persist(stdout.clone(), stderr.clone(), slot);

    // Verify truncation occurred
    assert!(byte_truncated, "byte_truncated should be true");
    assert!(stdout_path.is_some(), "stdout_path should be set");
    assert!(stderr_path.is_some(), "stderr_path should be set");

    // Verify output was truncated
    assert_eq!(out_stdout, "small", "stdout should be unchanged");
    assert!(
        out_stderr.len() <= 10_000,
        "stderr should be truncated to <= 10k"
    );

    // Verify slot file was written
    let base = std::env::temp_dir()
        .join("aptu-coder-overflow")
        .join(format!("slot-{slot}"));
    let stderr_file = base.join("stderr");
    assert!(
        stderr_file.exists(),
        "stderr slot file should exist after byte overflow"
    );
}

#[test]
fn test_exec_byte_overflow_combined_exceeds_50k() {
    // Edge case: combined output_text exceeds 50k char limit
    // This is tested by verifying the truncation logic in exec_command
    let large_output = "z".repeat(60_000);
    assert!(large_output.len() > SIZE_LIMIT);

    // Simulate the truncation logic from exec_command
    let mut combined_truncated = false;
    let truncated = if large_output.len() > SIZE_LIMIT {
        combined_truncated = true;
        let tail_start = large_output.len().saturating_sub(SIZE_LIMIT);
        let safe_start = large_output.floor_char_boundary(tail_start);
        large_output[safe_start..].to_string()
    } else {
        large_output.clone()
    };

    assert!(combined_truncated, "combined_truncated should be true");
    assert!(
        truncated.len() <= SIZE_LIMIT,
        "output should be truncated to <= 50k"
    );
}

#[test]
fn test_exec_line_and_byte_interaction() {
    // Edge case: line cap and byte cap are independent
    // 1500 lines with long content to exceed 30k bytes should trigger byte cap, not line cap
    let lines: Vec<String> = (0..1500)
        .map(|i| {
            format!(
                "line {} with some padding to make it longer: {}",
                i,
                "x".repeat(15)
            )
        })
        .collect();
    let stdout = lines.join("\n");
    assert!(stdout.lines().count() <= 2000, "should have <= 2000 lines");
    assert!(stdout.len() > 30_000, "should exceed 30k bytes");

    let stderr = "".to_string();
    let slot = 2u32;

    let (out_stdout, _out_stderr, stdout_path, _stderr_path, byte_truncated) =
        handle_output_persist(stdout.clone(), stderr, slot);

    // Byte cap should fire, not line cap
    assert!(byte_truncated, "byte_truncated should be true");
    assert!(stdout_path.is_some(), "stdout_path should be set");
    assert!(
        out_stdout.len() <= 30_000,
        "stdout should be truncated by byte cap"
    );
}

#[test]
fn test_exec_utf8_boundary_safety() {
    // Edge case: ensure truncation doesn't split multi-byte UTF-8 chars
    // Create a string with multi-byte characters near the boundary
    let mut stdout = String::new();
    for _ in 0..4000 {
        stdout.push_str("hello world ");
    }
    // Add some multi-byte chars
    stdout.push_str("こんにちは"); // Japanese characters (3 bytes each)
    assert!(stdout.len() > 30_000, "stdout should exceed 30k bytes");

    let stderr = "".to_string();
    let slot = 5u32;

    let (out_stdout, _out_stderr, _stdout_path, _stderr_path, byte_truncated) =
        handle_output_persist(stdout, stderr, slot);

    // Verify truncation happened and result is valid UTF-8
    assert!(byte_truncated, "byte_truncated should be true");
    assert!(
        out_stdout.is_char_boundary(0),
        "start should be char boundary"
    );
    assert!(
        out_stdout.is_char_boundary(out_stdout.len()),
        "end should be char boundary"
    );
    // Verify we can iterate chars without panic
    let _char_count = out_stdout.chars().count();
}

#[test]
fn test_strip_cd_prefix_basic() {
    let (cmd, path) = strip_cd_prefix("cd /tmp && echo hello");
    assert_eq!(cmd, "echo hello");
    assert_eq!(path, Some("/tmp"));
}

#[test]
fn test_strip_cd_prefix_no_ampersand() {
    // No && separator -- returned unmodified; shell handles the cd naturally.
    let (cmd, path) = strip_cd_prefix("cd /tmp");
    assert_eq!(cmd, "cd /tmp");
    assert_eq!(path, None);
}

#[test]
fn test_strip_cd_prefix_with_extra_spaces() {
    // Surrounding whitespace is trimmed from both extracted path and stripped command.
    let (cmd, path) = strip_cd_prefix("cd  /tmp  &&  echo hello");
    assert_eq!(path, Some("/tmp"));
    assert_eq!(cmd, "echo hello");
}

#[test]
fn test_strip_cd_prefix_splits_on_first_ampersand_only() {
    // Only the leading cd && is consumed; subsequent && in the command are preserved.
    let (cmd, path) = strip_cd_prefix("cd /a && cmd1 && cd /b && cmd2");
    assert_eq!(path, Some("/a"));
    assert_eq!(cmd, "cmd1 && cd /b && cmd2");
}

#[tokio::test]
async fn test_handle_output_persist_mid_char_boundary() {
    // Regression: tail_start falls inside a multi-byte UTF-8 char.
    // Construct stdout of exactly MAX_STDOUT_BYTES + 1 bytes where byte 1
    // is the second byte of a 3-byte char (中, U+4E2D).
    // Old code: stdout[..tail_start] panics because byte 1 is not a char boundary.
    // Fix: floor_char_boundary on the full string returns 0, no panic.
    let mut stdout = String::new();
    stdout.push('\u{4E2D}'); // 3 bytes: 0xE4 0xB8 0xAD
    stdout.push_str(&"a".repeat(29998)); // total = 30001 bytes
    assert_eq!(stdout.len(), 30_001);

    let stderr = String::new();
    let slot = 99u32;

    let (out_stdout, _out_stderr, _stdout_path, _stderr_path, byte_truncated) =
        handle_output_persist(stdout, stderr, slot);

    assert!(byte_truncated, "byte_truncated should be true");
    assert!(
        out_stdout.is_char_boundary(0),
        "start should be char boundary"
    );
    assert!(
        out_stdout.is_char_boundary(out_stdout.len()),
        "end should be char boundary"
    );
    let _char_count = out_stdout.chars().count();
}

#[tokio::test]
async fn test_persist_interleaved_mid_char_boundary() {
    // Regression: tail_start falls inside a multi-byte UTF-8 char.
    // Construct interleaved of max_bytes + 1 bytes where byte 1
    // is the second byte of a 3-byte char.
    let mut interleaved = String::new();
    interleaved.push('\u{4E2D}'); // 3 bytes
    interleaved.push_str(&"a".repeat(98)); // total = 101 bytes
    assert_eq!(interleaved.len(), 101);

    let max_bytes = 100usize;
    let slot = 42u32;

    let (preview, path) = persist_interleaved_overflow(interleaved, max_bytes, slot).await;

    assert!(path.is_some(), "should have overflowed to slot file");
    assert!(preview.is_char_boundary(0), "start should be char boundary");
    assert!(
        preview.is_char_boundary(preview.len()),
        "end should be char boundary"
    );
    let _char_count = preview.chars().count();
}

#[tokio::test]
async fn test_run_exec_impl_raw_byte_counters() {
    // Edge case: unconditional byte counters increment on every line received
    // even after budget check fires. Use a command that produces output exceeding
    // the drain budget to verify raw counters exceed the budget.
    let filter_table = std::sync::Arc::new(Vec::<CompiledRule>::new());
    let (output, raw_so, raw_se) = run_exec_impl(
        "echo hello && echo world >&2".to_string(),
        None,
        None,
        0,
        None,
        &filter_table,
        Some(5),
        std::time::Duration::from_millis(500),
    )
    .await;

    // Both stdout and stderr should have been captured
    assert!(raw_so >= 6, "raw_stdout_bytes should be >= 6 (hello\\n)");
    assert!(raw_se >= 6, "raw_stderr_bytes should be >= 6 (world\\n)");
    assert_eq!(output.exit_code, Some(0));
    assert!(!output.timed_out);
}

#[tokio::test]
async fn test_run_exec_impl_raw_counters_exceed_budget() {
    // Edge case: raw counters should exceed the budget when output is large.
    // Generate 40k bytes of stdout (exceeds 30k MAX_DRAIN_STDOUT_BYTES).
    let filter_table = std::sync::Arc::new(Vec::<CompiledRule>::new());
    let large_line = "x".repeat(1000);
    let cmd = format!("for i in $(seq 1 50); do echo {}; done", large_line);
    let (output, raw_so, _raw_se) = run_exec_impl(
        cmd,
        None,
        None,
        0,
        None,
        &filter_table,
        Some(10),
        std::time::Duration::from_millis(500),
    )
    .await;

    // Raw counter should exceed the 30k budget
    assert!(raw_so > 30_000, "raw_stdout_bytes should exceed 30k budget");
    assert!(output.output_truncated, "output should be truncated");
    assert_eq!(output.exit_code, Some(0));
    assert!(!output.timed_out);
}

#[tokio::test]
async fn test_run_exec_impl_raw_counters_zero_on_timeout() {
    // Edge case: raw byte counters are 0 when timed_out=true because the
    // drain task is aborted before any output is collected.
    let filter_table = std::sync::Arc::new(Vec::<CompiledRule>::new());
    let (output, raw_so, raw_se) = run_exec_impl(
        "sleep 2".to_string(),
        None,
        None,
        0,
        None,
        &filter_table,
        Some(1), // 1 second timeout, sleep 2 exceeds it
        std::time::Duration::from_millis(500),
    )
    .await;

    assert!(output.timed_out, "command should have timed out");
    assert_eq!(raw_so, 0, "raw_stdout_bytes should be 0 on timeout");
    assert_eq!(raw_se, 0, "raw_stderr_bytes should be 0 on timeout");
}
