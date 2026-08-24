// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the MCP Resource surface pagination
//! (`resources/list` and `resources/templates/list`).
//!
//! These are server-push resources rather than tools, so `call_tool_raw` from
//! `common/mod.rs` cannot reach them. We hand-roll JSON-RPC over a duplex pipe
//! mirroring the `make_test_analyzer` harness pattern: initialize handshake,
//! initialized notification, then the method call.

use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;

use aptu_coder_core::pagination::{CursorData, PaginationMode, encode_cursor};
use serde_json::json;

fn make_analyzer() -> aptu_coder::CodeAnalyzer {
    let peer = Arc::new(TokioMutex::new(None));
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    aptu_coder::CodeAnalyzer::new(peer, aptu_coder::MetricsSender(metrics_tx))
}

/// Send a single JSON-RPC method call after the initialize handshake and
/// return the response whose id matches.
async fn send_request(method: &str, params: serde_json::Value) -> serde_json::Value {
    let analyzer = make_analyzer();
    let (client, server) = tokio::io::duplex(65536);

    let mut server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = rmcp::serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

    // Step 1: initialize
    let init = json!({
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
    let _resp = reader
        .next_line()
        .await
        .expect("IO error reading initialize response")
        .expect("server closed before sending initialize response");

    // Step 2: initialized notification
    let notif = json!({
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

    // Step 3: method call
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": method,
        "params": params
    })
    .to_string()
        + "\n";
    client_tx
        .write_all(call.as_bytes())
        .await
        .expect("failed to write request");
    client_tx.flush().await.expect("failed to flush request");

    // Step 4: race response loop against server handle to surface server panics
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
                if v.get("id") == Some(&json!(2)) {
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

/// Happy path: resources/list without a cursor returns an empty list and no
/// nextCursor.
#[tokio::test]
async fn test_list_resources_no_cursor() {
    let resp = send_request("resources/list", json!({})).await;
    assert!(
        resp.get("error").is_none(),
        "unexpected error response: {resp}"
    );
    assert_eq!(resp["result"]["resources"], json!([]));
    assert!(
        resp["result"].get("nextCursor").is_none(),
        "expected no nextCursor on a single-page result, got: {resp}"
    );
}

/// Happy path: resources/templates/list without a cursor returns both
/// templates and no nextCursor.
#[tokio::test]
async fn test_list_resource_templates_no_cursor() {
    let resp = send_request("resources/templates/list", json!({})).await;
    assert!(
        resp.get("error").is_none(),
        "unexpected error response: {resp}"
    );
    let templates = resp["result"]["resourceTemplates"]
        .as_array()
        .unwrap_or_else(|| panic!("expected resourceTemplates array, got: {resp}"));
    assert_eq!(
        templates.len(),
        2,
        "expected two advertised templates, got: {resp}"
    );
    assert!(
        resp["result"].get("nextCursor").is_none(),
        "expected no nextCursor on a single-page result, got: {resp}"
    );
}

/// Edge case: a malformed cursor yields a JSON-RPC error with
/// INVALID_PARAMS (-32602).
#[tokio::test]
async fn test_list_resource_templates_malformed_cursor() {
    let resp = send_request(
        "resources/templates/list",
        json!({"cursor": "not-valid-base64!!"}),
    )
    .await;
    let error = resp
        .get("error")
        .unwrap_or_else(|| panic!("expected error response, got: {resp}"));
    assert_eq!(
        error["code"].as_i64().unwrap(),
        -32602,
        "expected INVALID_PARAMS code, got: {resp}"
    );
}

/// Edge case: a valid cursor pointing past the end of the three-template
/// catalog returns an empty page with no nextCursor.
#[tokio::test]
async fn test_list_resource_templates_out_of_range_cursor() {
    let cursor = encode_cursor(&CursorData {
        mode: PaginationMode::Default,
        offset: 9999,
    })
    .expect("cursor encoding must succeed");
    let resp = send_request("resources/templates/list", json!({"cursor": cursor})).await;
    assert!(
        resp.get("error").is_none(),
        "unexpected error response: {resp}"
    );
    assert_eq!(
        resp["result"]["resourceTemplates"],
        json!([]),
        "expected empty page for out-of-range offset, got: {resp}"
    );
    assert!(
        resp["result"].get("nextCursor").is_none(),
        "expected no nextCursor on the last page, got: {resp}"
    );
}
