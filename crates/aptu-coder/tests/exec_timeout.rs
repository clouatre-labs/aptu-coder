// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;
use std::time::Duration;

/// Regression test: the no-timeout path (timeout_secs omitted or None) must
/// not hang indefinitely when the child process holds stdout open (e.g. a
/// macOS login shell profile that blocks).
///
/// The fix wraps drain_task with a 30-second timeout in the `_ =>` catch-all
/// arm of `run_with_timeout`. When the drain timeout fires, the child is
/// killed and the drain task aborted so the caller does not hang on rx.recv().
///
/// This test spawns a child that writes one line then sleeps (keeping stdout
/// open), and asserts the call returns within 35 seconds.
#[tokio::test]
async fn test_exec_drain_bounded_when_pipe_held_open() {
    let fut = async {
        let resp = call_tool_raw(
            "exec_command",
            serde_json::json!({
                "command": "sh -c 'printf \"line\\n\"; sleep 300'"
            }),
        )
        .await;

        // The call should return some result (the key invariant is that it
        // returns at all within 35s, not that any particular field is set).
        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "expected no tool error from bounded drain: {resp}"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(!text.is_empty(), "expected non-empty output text: {resp}");
    };

    tokio::time::timeout(Duration::from_secs(35), fut)
        .await
        .expect("drain did not complete within 35s: the no-timeout path may still hang");
}
