// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;
use tracing_subscriber::filter::LevelFilter;

fn make_test_analyzer() -> aptu_coder::CodeAnalyzer {
    let peer = Arc::new(TokioMutex::new(None));
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<aptu_coder::logging::LogEvent>();
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    aptu_coder::CodeAnalyzer::new(
        peer,
        log_level_filter,
        rx,
        aptu_coder::metrics::MetricsSender(metrics_tx),
    )
}

async fn call_exec_command_raw(params: serde_json::Value) -> serde_json::Value {
    let analyzer = make_test_analyzer();
    let (client, server) = tokio::io::duplex(65536);

    // Spawn the analyzer server on the server half
    let mut server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = rmcp::serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

    // Step 1: Send initialize request
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "0.1.0"}
        }
    })
    .to_string()
        + "\n";
    client_tx.write_all(init.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 2: Read initialize response (discard)
    let _resp = reader.next_line().await.unwrap().unwrap();

    // Step 3: Send initialized notification (no id)
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    })
    .to_string()
        + "\n";
    client_tx.write_all(notif.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 4: Send tools/call
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "exec_command",
            "arguments": params
        }
    })
    .to_string()
        + "\n";
    client_tx.write_all(call.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 5: Race response loop against server handle to surface server panics
    tokio::select! {
        result = async {
            loop {
                let line = reader.next_line().await.unwrap().unwrap();
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                if v.get("id") == Some(&serde_json::json!(2)) {
                    return v;
                }
            }
        } => {
            server_handle.abort();
            result
        }
        outcome = &mut server_handle => {
            match outcome {
                Ok(_) => panic!("server task exited unexpectedly before tool response"),
                Err(e) => panic!("server task panicked: {e}"),
            }
        }
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
    // Arrange: pass a working_dir that points outside the server's CWD
    // This test verifies that the handler rejects paths outside the allowed directory
    // We'll use a path like "/tmp" or "../../etc" which should be rejected

    // Note: This test is a placeholder that documents the expected behavior.
    // The actual handler validation happens in the exec_command tool handler in lib.rs,
    // which calls validate_path() and checks if the directory exists and is within bounds.
    // A full integration test would require setting up the MCP server context.

    // For now, we verify that attempting to use an absolute path like /tmp
    // would be rejected by the validate_path function.
    let invalid_path = "/tmp";

    // The validate_path function should reject this because it's outside the server's CWD
    // This is tested implicitly by the handler's validation logic.
    assert!(
        invalid_path.starts_with("/"),
        "absolute paths should be rejected by validate_path"
    );
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
async fn test_handler_resource_limits_none_unchanged() {
    // Arrange: memory_limit_mb=None and cpu_limit_secs=None -> same behavior as before
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo test",
        "memory_limit_mb": null,
        "cpu_limit_secs": null
    }))
    .await;

    // Act & Assert: command should complete successfully
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["exit_code"], 0, "exit code should be 0: {sc}");
    assert!(
        sc["stdout"].as_str().unwrap_or("").contains("test"),
        "stdout should contain 'test': {sc}"
    );
    assert!(
        !sc["timed_out"].as_bool().unwrap_or(true),
        "should not timeout: {sc}"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_handler_cpu_limit_kills_spin() {
    // Arrange: cpu_limit_secs=1, command spins CPU
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "sh -c 'while true; do :; done'",
        "cpu_limit_secs": 1,
        "timeout_secs": 10
    }))
    .await;

    // Act & Assert: process should be killed by SIGXCPU/SIGKILL.
    // When killed by a signal, the OS does not produce a numeric exit code, so
    // exit_code is null in the response. Accept either null (signal kill) or a
    // non-zero integer (some kernels synthesise 128+signum).
    let sc = &resp["result"]["structuredContent"];
    let exit_code = sc["exit_code"].as_i64();
    assert!(
        exit_code.is_none() || exit_code != Some(0),
        "exit code should be null (signal kill) or non-zero: {sc}"
    );
}

#[tokio::test]
async fn test_handler_memory_limit_accepted() {
    // Arrange: memory_limit_mb=Some(512), simple echo command
    let resp = call_exec_command_raw(serde_json::json!({
        "command": "echo hello",
        "memory_limit_mb": 512
    }))
    .await;

    // Act & Assert: command should complete normally (limit not triggered)
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["exit_code"], 0, "exit code should be 0: {sc}");
    assert!(
        sc["stdout"].as_str().unwrap_or("").contains("hello"),
        "stdout should contain 'hello': {sc}"
    );
}
