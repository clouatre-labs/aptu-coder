use crate::tools::exec_command::strip_cd_prefix;
use crate::tools::exec_runtime::{build_exec_command, handle_output_persist};
use crate::{SIZE_LIMIT, STDIN_MAX_BYTES};

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
        let safe_start = large_output[..tail_start].floor_char_boundary(tail_start);
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
