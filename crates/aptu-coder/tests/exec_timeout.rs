// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;
use std::time::Duration;

/// Regression test: the user-timeout path (timeout_secs = 30) must
/// not hang indefinitely when the child process holds stdout open (e.g. a
/// macOS login shell profile that blocks).
///
/// After the wait/drain order inversion, the no-timeout path waits for
/// child exit before draining, so a child that sleeps indefinitely must
/// be bounded by an explicit timeout_secs. This test uses 30s to match
/// the old DEFAULT_DRAIN_TIMEOUT_SECS.
///
/// This test spawns a child that writes one line then sleeps (keeping stdout
/// open), and asserts the call returns within 35 seconds.
#[tokio::test]
async fn test_exec_drain_bounded_when_pipe_held_open() {
    let fut = async {
        let resp = call_tool_raw(
            "exec_command",
            serde_json::json!({
                "command": "sh -c 'printf \"line\\n\"; sleep 300'",
                "timeout_secs": 30
            }),
        )
        .await;

        // The call should return some result (the key invariant is that it
        // returns at all within 35s, not that any particular field is set).
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "expected tool timeout error from bounded drain timeout: {resp}"
        );
    };

    tokio::time::timeout(Duration::from_secs(35), fut)
        .await
        .expect("drain did not complete within 35s: the timeout path may still hang");
}
