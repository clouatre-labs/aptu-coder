// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;
use tracing_subscriber::filter::LevelFilter;

/// Helper: create a server and make N sequential tool calls (each with its own (tool_name, params)),
/// returning all responses in order.
async fn call_tool_raw_seq(calls: Vec<(&str, serde_json::Value)>) -> Vec<serde_json::Value> {
    let peer = Arc::new(TokioMutex::new(None));
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<aptu_coder::logging::LogEvent>();
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    let analyzer = aptu_coder::CodeAnalyzer::new(
        peer,
        log_level_filter,
        rx,
        aptu_coder::metrics::MetricsSender(metrics_tx),
    );

    let (client, server) = tokio::io::duplex(65536);

    let server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = rmcp::serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

    // Initialize
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "0.1.0"}
        }
    })
    .to_string()
        + "\n";
    client_tx
        .write_all(init.as_bytes())
        .await
        .expect("failed to write initialize request");
    client_tx
        .flush()
        .await
        .expect("failed to flush initialize request");
    let _resp = reader
        .next_line()
        .await
        .expect("IO error reading initialize response")
        .expect("server closed before sending initialize response");

    // Initialized notification
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    })
    .to_string()
        + "\n";
    client_tx
        .write_all(notif.as_bytes())
        .await
        .expect("failed to write initialized notification");
    client_tx
        .flush()
        .await
        .expect("failed to flush initialized notification");

    // Send tool calls
    let mut responses = Vec::with_capacity(calls.len());
    for (i, (tool_name, params)) in calls.into_iter().enumerate() {
        let call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": (i + 2) as u64,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": params
            }
        })
        .to_string()
            + "\n";
        client_tx
            .write_all(call.as_bytes())
            .await
            .expect("failed to write tools/call request");
        client_tx
            .flush()
            .await
            .expect("failed to flush tools/call request");

        loop {
            let line = reader
                .next_line()
                .await
                .expect("IO error reading tool response")
                .expect("server closed before sending tool response");
            let v: serde_json::Value =
                serde_json::from_str(&line).expect("tool response is not valid JSON");
            if v.get("id") == Some(&serde_json::json!((i + 2) as u64)) {
                responses.push(v);
                break;
            }
        }
    }

    server_handle.abort();
    responses
}

/// When old_text is not found, the error message includes "The file begins:"
/// with a preview of the first 20 lines of the file.
#[tokio::test]
async fn test_edit_replace_not_found_shows_file_preview() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "line one\nline two\nline three\n";
    std::fs::write(&file_path, content).expect("should write file");

    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "nonexistent text",
            "new_text": "replacement",
            "working_dir": working_dir
        }),
    )
    .await;

    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(msg.contains("The file begins:"));
    assert!(msg.contains("line one"));
    assert!(msg.contains("Nearest match:"));
    assert!(!msg.contains(working_dir));
    assert!(!msg.contains(file_name));
}

/// When old_text matches multiple locations, the error message includes
/// "Occurrences at lines:" with the 1-based line numbers of each match.
#[tokio::test]
async fn test_edit_replace_ambiguous_shows_line_numbers() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "alpha\nbeta\nalpha\n";
    std::fs::write(&file_path, content).expect("should write file");

    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "alpha",
            "new_text": "replacement",
            "working_dir": working_dir
        }),
    )
    .await;

    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(msg.contains("Occurrences at lines:"));
    assert!(msg.contains("2 locations"));
    assert!(!msg.contains(working_dir));
    assert!(!msg.contains(file_name));
}

/// Circuit breaker trips after EDIT_STALE_THRESHOLD (5) consecutive not_found
/// errors on the same (session_id, canonical_path) pair.
#[tokio::test]
async fn test_circuit_breaker_trips_at_threshold() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "unique content here\n";
    std::fs::write(&file_path, content).expect("should write file");

    let bad_params = serde_json::json!({
        "path": file_name,
        "old_text": "nonexistent text",
        "new_text": "replacement",
        "working_dir": working_dir
    });
    let calls: Vec<(&str, serde_json::Value)> = vec![("edit_replace", bad_params); 6];

    let responses = call_tool_raw_seq(calls).await;

    for (i, resp) in responses.iter().enumerate().take(5) {
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "call {} expected error: {resp}",
            i + 1
        );
    }

    let fifth_msg = responses[4]["result"]["content"][0]["text"]
        .as_str()
        .expect("call 5 should have text");
    assert!(
        fifth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 5 should contain EDIT_STALE_CONTEXT but got: {fifth_msg}"
    );
    assert!(
        fifth_msg.contains("5 consecutive"),
        "call 5 should mention 5 consecutive but got: {fifth_msg}"
    );

    let sixth_msg = responses[5]["result"]["content"][0]["text"]
        .as_str()
        .expect("call 6 should have text");
    assert!(
        sixth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 6 should still contain stale-context but got: {sixth_msg}"
    );
}

/// A successful edit_replace resets the circuit breaker counter for that path.
#[tokio::test]
async fn test_circuit_breaker_resets_on_edit_replace_success() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "Hello, world!\n";
    std::fs::write(&file_path, content).expect("should write file");

    let mut calls: Vec<(&str, serde_json::Value)> = Vec::new();
    for _ in 0..4 {
        calls.push((
            "edit_replace",
            serde_json::json!({
                "path": file_name,
                "old_text": "nonexistent",
                "new_text": "x",
                "working_dir": working_dir
            }),
        ));
    }
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "Hello, world!",
            "new_text": "Replaced!",
            "working_dir": working_dir
        }),
    ));
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "nonexistent",
            "new_text": "x",
            "working_dir": working_dir
        }),
    ));

    let responses = call_tool_raw_seq(calls).await;

    for (i, resp) in responses.iter().enumerate().take(4) {
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "call {} expected error: {resp}",
            i + 1
        );
    }
    let fifth_resp = &responses[4];
    assert!(
        !fifth_resp["result"]["isError"].as_bool().unwrap_or(true),
        "call 5 expected success but got error: {fifth_resp}"
    );
    let sixth_resp = &responses[5];
    assert!(
        sixth_resp["result"]["isError"].as_bool().unwrap_or(false),
        "call 6 expected error but got success: {sixth_resp}"
    );
    let sixth_msg = sixth_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("call 6 should have text");
    assert!(
        !sixth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 6 should NOT be stale_context after success reset but got: {sixth_msg}"
    );
}

/// Successful edit on the same path (via edit_replace after file rewrite) resets
/// the circuit breaker counter. Tests the same code path as edit_overwrite reset.
#[tokio::test]
async fn test_circuit_breaker_resets_on_successful_edit() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "some content\n";
    std::fs::write(&file_path, content).expect("should write file");

    let mut calls: Vec<(&str, serde_json::Value)> = Vec::new();
    for _ in 0..2 {
        calls.push((
            "edit_replace",
            serde_json::json!({
                "path": file_name,
                "old_text": "nonexistent",
                "new_text": "x",
                "working_dir": working_dir
            }),
        ));
    }
    // Rewrite file then do a successful edit_replace (same reset path as edit_overwrite)
    std::fs::write(&file_path, "replacement content\n").expect("should rewrite file");
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "replacement content\n",
            "new_text": "overwritten\n",
            "working_dir": working_dir
        }),
    ));
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "still nonexistent",
            "new_text": "x",
            "working_dir": working_dir
        }),
    ));

    let responses = call_tool_raw_seq(calls).await;

    for (i, resp) in responses.iter().enumerate().take(2) {
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "call {} expected error: {resp}",
            i + 1
        );
    }
    let third_resp = &responses[2];
    assert!(
        !third_resp["result"]["isError"].as_bool().unwrap_or(true),
        "call 3 (edit_replace) expected success but got error: {third_resp}"
    );
    let fourth_resp = &responses[3];
    assert!(
        fourth_resp["result"]["isError"].as_bool().unwrap_or(false),
        "call 4 expected error: {fourth_resp}"
    );
    let fourth_msg = fourth_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("call 4 should have text");
    assert!(
        !fourth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 4 should NOT be stale_context after success reset but got: {fourth_msg}"
    );
    assert!(
        fourth_msg.contains("not found"),
        "call 4 should be normal not_found after success reset but got: {fourth_msg}"
    );
}

/// Path isolation: 5 failures on path A do not affect the counter for path B.
#[tokio::test]
async fn test_circuit_breaker_path_isolation() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_a = "file_a.txt";
    let file_b = "file_b.txt";
    std::fs::write(temp_dir.path().join(file_a), "content a\n").expect("should write file a");
    std::fs::write(temp_dir.path().join(file_b), "content b\n").expect("should write file b");

    let mut calls: Vec<(&str, serde_json::Value)> = Vec::new();
    for _ in 0..5 {
        calls.push((
            "edit_replace",
            serde_json::json!({
                "path": file_a,
                "old_text": "nonexistent",
                "new_text": "x",
                "working_dir": working_dir
            }),
        ));
    }
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_b,
            "old_text": "nonexistent",
            "new_text": "x",
            "working_dir": working_dir
        }),
    ));

    let responses = call_tool_raw_seq(calls).await;

    for (i, resp) in responses.iter().enumerate().take(4) {
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "call {} on file_a expected error: {resp}",
            i + 1
        );
    }
    let fifth_msg = responses[4]["result"]["content"][0]["text"]
        .as_str()
        .expect("call 5 on file_a should have text");
    assert!(
        fifth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 5 on file_a should contain EDIT_STALE_CONTEXT but got: {fifth_msg}"
    );
    let sixth_resp = &responses[5];
    assert!(
        sixth_resp["result"]["isError"].as_bool().unwrap_or(false),
        "call 6 on file_b expected error: {sixth_resp}"
    );
    let sixth_msg = sixth_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("call 6 on file_b should have text");
    assert!(
        !sixth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 6 on file_b should NOT be stale_context (path isolation) but got: {sixth_msg}"
    );
    assert!(
        sixth_msg.contains("not found"),
        "call 6 on file_b should be normal not_found but got: {sixth_msg}"
    );
}
