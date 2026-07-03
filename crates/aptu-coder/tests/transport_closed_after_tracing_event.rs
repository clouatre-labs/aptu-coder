// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Transport stability: a tracing event must not close the transport.
//!
//! Before fix (308d66f dropped enable_logging() but left the log-consumer task running),
//! the first tracing event emitted during a tool call caused the log-consumer task to
//! send a logging/message notification to the peer. Since the logging capability was not
//! advertised, the transport write failed and closed stdout. Every subsequent tool call
//! then returned -32603 Transport closed.

mod common;

/// Transport must stay open after a tool call that emits a tracing event.
///
/// Before the fix, exec_command (id:3) would return a JSON-RPC error with code -32603
/// ("Transport closed") because the log-consumer task closed stdout on the first
/// tracing event (analyze_file cache-miss path). Any tool emitting a tracing event
/// before the session's first response would trigger the same failure.
#[tokio::test]
async fn transport_stays_open_after_tracing_event() {
    let responses = common::call_tool_raw_seq(vec![
        (
            "analyze_file",
            serde_json::json!({
                "path": "Cargo.toml",
                "page_size": 100
            }),
        ),
        (
            "exec_command",
            serde_json::json!({
                "command": "echo ok"
            }),
        ),
    ])
    .await;

    assert_eq!(responses.len(), 2, "expected exactly 2 responses");

    // analyze_file must succeed (no JSON-RPC protocol error)
    let af = &responses[0];
    assert!(
        af.get("result").is_some(),
        "analyze_file must return a result, got: {af}"
    );
    assert!(
        af.get("error").is_none(),
        "analyze_file must not return a JSON-RPC error, got: {af}"
    );

    // exec_command must succeed -- before the fix this returned -32603 Transport closed
    let ec = &responses[1];
    assert!(
        ec.get("result").is_some(),
        "exec_command must return a result after analyze_file (transport must stay open), got: {ec}"
    );
    assert!(
        ec.get("error").is_none(),
        "exec_command must not return -32603 Transport closed after analyze_file, got: {ec}"
    );
}
