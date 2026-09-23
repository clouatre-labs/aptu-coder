// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
#![allow(dead_code)]

use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;

/// Build a test analyzer together with the receiving half of its metrics channel,
/// so tests can assert on emitted events (including the one-time `schema_surface`).
pub fn make_test_analyzer_with_metrics() -> (
    aptu_coder::CodeAnalyzer,
    tokio::sync::mpsc::UnboundedReceiver<aptu_coder::MetricEvent>,
) {
    let peer = Arc::new(TokioMutex::new(None));
    let (metrics_tx, metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    let analyzer = aptu_coder::CodeAnalyzer::new(peer, aptu_coder::MetricsSender(metrics_tx));
    (analyzer, metrics_rx)
}

pub fn make_test_analyzer() -> aptu_coder::CodeAnalyzer {
    make_test_analyzer_with_metrics().0
}

/// Send a single MCP request over a fresh in-process connection.
///
/// Performs the initialize/initialized handshake, sends one request with
/// `id=2`, and races the response loop against the server task to surface
/// panics.  This is the canonical helper for one-shot MCP method dispatch;
/// both `call_tool_raw` (tools) and resource-method tests build on it.
pub async fn send_raw_request(
    analyzer: aptu_coder::CodeAnalyzer,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let (client, server) = tokio::io::duplex(65536);

    let mut server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = rmcp::serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

    // Initialize (id=1).
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": rmcp::model::ProtocolVersion::LATEST.as_str(),
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
    let _init_resp = reader
        .next_line()
        .await
        .expect("IO error reading initialize response")
        .expect("server closed before sending initialize response");

    // notifications/initialized (no id).
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

    // The actual request (id=2).
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": method,
        "params": params
    })
    .to_string()
        + "\n";
    client_tx
        .write_all(request.as_bytes())
        .await
        .expect("failed to write request");
    client_tx.flush().await.expect("failed to flush request");

    // Race response loop vs server task to surface panics.
    tokio::select! {
        result = async {
            loop {
                let line = reader
                    .next_line()
                    .await
                    .expect("IO error reading response")
                    .expect("server closed before sending response");
                let v: serde_json::Value =
                    serde_json::from_str(&line).expect("response is not valid JSON");
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
                Ok(_) => panic!("server task exited unexpectedly before response"),
                Err(e) => panic!("server task panicked: {e}"),
            }
        }
    }
}

/// Call a single `tools/call` request. Thin wrapper over `send_raw_request`.
pub async fn call_tool_raw(tool_name: &str, params: serde_json::Value) -> serde_json::Value {
    let analyzer = make_test_analyzer();
    send_raw_request(
        analyzer,
        "tools/call",
        serde_json::json!({
            "name": tool_name,
            "arguments": params
        }),
    )
    .await
}

/// Send a `tools/call` request, then cancel it via `notifications/cancelled`
/// after `delay`. rmcp drops the tool response for cancelled requests, so the
/// caller observes the kill via side effects: `check` is polled every 100 ms
/// (up to `poll_timeout`) after cancellation is sent. Returns true when `check`
/// reports success.
pub async fn call_tool_cancel_and_check(
    tool_name: &str,
    params: serde_json::Value,
    delay: std::time::Duration,
    check: impl Fn() -> bool + Send + 'static,
    poll_timeout: std::time::Duration,
) -> bool {
    let analyzer = make_test_analyzer();
    let (client, server) = tokio::io::duplex(65536);

    let server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = rmcp::serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

    // Initialize (id=1) + notifications/initialized.
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": rmcp::model::ProtocolVersion::LATEST.as_str(),
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
    let _init_resp = reader
        .next_line()
        .await
        .expect("IO error reading initialize response")
        .expect("server closed before sending initialize response");
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

    // The tools/call request (id=2).
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": tool_name, "arguments": params}
    })
    .to_string()
        + "\n";
    client_tx
        .write_all(request.as_bytes())
        .await
        .expect("failed to write request");
    client_tx.flush().await.expect("failed to flush request");

    // Give the tool time to start, then cancel the request.
    tokio::time::sleep(delay).await;
    let cancel = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 2, "reason": "test cancellation"}
    })
    .to_string()
        + "\n";
    client_tx
        .write_all(cancel.as_bytes())
        .await
        .expect("failed to write cancellation notification");
    client_tx
        .flush()
        .await
        .expect("failed to flush cancellation notification");

    // Poll the side-effect check, discarding any server traffic in between.
    let deadline = tokio::time::Instant::now() + poll_timeout;
    let mut result = false;
    while tokio::time::Instant::now() < deadline {
        // Drain one pending server line if any (ignore response-drop behavior).
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(50), reader.next_line()).await;
        if check() {
            result = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    server_handle.abort();
    result
}

/// Call multiple `tools/call` requests sequentially on the same MCP connection.
///
/// Each request is sent only after the previous response is received, which is
/// required for cache-ordering tests (e.g. `symbol_cache_tests`).
pub async fn call_tool_raw_seq(calls: Vec<(&str, serde_json::Value)>) -> Vec<serde_json::Value> {
    let analyzer = make_test_analyzer();
    let (client, server) = tokio::io::duplex(65536);

    let server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = rmcp::serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

    // Initialize (id=1).
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": rmcp::model::ProtocolVersion::LATEST.as_str(),
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
    let _init_resp = reader
        .next_line()
        .await
        .expect("IO error reading initialize response")
        .expect("server closed before sending initialize response");

    // notifications/initialized (no id).
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

    // Sequential tool calls: send each only after the previous response.
    let mut responses = Vec::with_capacity(calls.len());
    for (i, (tool_name, params)) in calls.into_iter().enumerate() {
        let id = (i + 2) as u64;
        let call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
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
            if v.get("id") == Some(&serde_json::json!(id)) {
                responses.push(v);
                break;
            }
        }
    }

    server_handle.abort();
    responses
}
