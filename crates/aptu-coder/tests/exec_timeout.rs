// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::{call_tool_cancel_and_check, call_tool_raw};
use std::time::Duration;

/// Happy path: with no client timeout parameter, the server default
/// (DEFAULT_EXEC_TIMEOUT_SECS = 300) still bounds a runaway child. The full
/// 300s bound is exercised by unit tests via injectable timeouts; this
/// integration test verifies the cancel-kill path end to end instead, because
/// rmcp drops the response for cancelled requests.
#[tokio::test]
async fn test_exec_command_respects_server_defaults() {
    let fut = async {
        let resp = call_tool_raw(
            "exec_command",
            serde_json::json!({
                "command": "echo quick"
            }),
        )
        .await;

        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "expected success under server default timeout: {resp}"
        );
    };

    tokio::time::timeout(Duration::from_secs(15), fut)
        .await
        .expect("fast command should finish promptly");
}

/// Edge case: a client-sent notifications/cancelled during a long sleep kills
/// the child process. rmcp suppresses the response for cancelled requests, so
/// the kill is observed via side effects: the marker file proves the command
/// started, and pgrep confirms the process is gone afterwards.
#[tokio::test]
async fn test_cancellation_kills_runaway_child() {
    let marker = std::env::temp_dir().join(format!("aptu-cancel-test-{}", std::process::id()));
    let marker_str = marker.display().to_string();
    let marker_clone = marker.clone();
    let command = format!(
        "touch {marker_str}; i=0; while [ $i -lt 6000 ]; do sleep 0.1; i=$((i+1)); done # cancel-test-{}",
        std::process::id()
    );

    let check = move || {
        // Only judge "killed" once the command demonstrably started.
        if !marker_clone.exists() {
            return false;
        }
        let output = std::process::Command::new("pgrep")
            .arg("-f")
            .arg(format!("cancel-test-{}", std::process::id()))
            .output()
            .expect("pgrep failed");
        output.stdout.is_empty()
    };

    let killed = call_tool_cancel_and_check(
        "exec_command",
        serde_json::json!({ "command": command }),
        Duration::from_millis(1500),
        check,
        Duration::from_secs(15),
    )
    .await;

    let _ = std::fs::remove_file(&marker);
    assert!(
        killed,
        "child process should be killed after notifications/cancelled"
    );
}
