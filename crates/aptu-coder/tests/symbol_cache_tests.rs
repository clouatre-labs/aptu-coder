// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::make_test_analyzer;
use rmcp::serve_server;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Send two sequential `tools/call` requests on the same MCP connection and return both responses.
/// The second request is sent only after the first response is received, ensuring the first
/// result has been stored in the `call_graph_cache` before the second lookup runs.
async fn call_tool_twice_sequential(
    tool_name: &str,
    params: serde_json::Value,
) -> (serde_json::Value, serde_json::Value) {
    let analyzer = make_test_analyzer();
    let (client, server) = tokio::io::duplex(65536);

    let server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

    // Initialize
    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": rmcp::model::ProtocolVersion::LATEST.as_str(),
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "0.1.0"}
        }
    })
    .to_string()
        + "\n";
    client_tx.write_all(init.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();
    // Discard initialize response
    reader.next_line().await.unwrap().unwrap();

    // notifications/initialized
    let notif = serde_json::json!({
        "jsonrpc": "2.0", "method": "notifications/initialized", "params": {}
    })
    .to_string()
        + "\n";
    client_tx.write_all(notif.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    /// Read lines until a response with the given id arrives.
    async fn read_response(
        reader: &mut tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        id: u64,
    ) -> Value {
        loop {
            let line = reader.next_line().await.unwrap().unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return v;
            }
        }
    }

    // First call (id=2) -- send and wait for response.
    let msg1 = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": tool_name, "arguments": &params}
    })
    .to_string()
        + "\n";
    client_tx.write_all(msg1.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();
    let resp1 = read_response(&mut reader, 2).await;

    // Second call (id=3) -- sent only after first response; cache is populated.
    let msg2 = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": tool_name, "arguments": &params}
    })
    .to_string()
        + "\n";
    client_tx.write_all(msg2.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();
    let resp2 = read_response(&mut reader, 3).await;

    server_handle.abort();
    (resp1, resp2)
}

/// Access `structuredContent` if present.
///
/// analyze_symbol responses must not carry a structuredContent payload
/// (alpha policy); assertions below lock in its absence.
fn structured_content(resp: &serde_json::Value) -> Option<&serde_json::Value> {
    resp["result"].get("structuredContent")
}

fn is_success(resp: &serde_json::Value) -> bool {
    !resp["result"]["isError"].as_bool().unwrap_or(false)
}

#[tokio::test]
async fn test_analyze_symbol_call_graph_cache_hit() {
    // Arrange: temp Rust fixture inside CWD so validate_path accepts the path.
    let cwd = std::env::current_dir().expect("must have cwd");
    let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
    std::fs::write(
        dir.path().join("lib.rs"),
        "fn inner() {}\n\nfn outer() {\n    inner();\n}\n",
    )
    .expect("write fixture");

    let params = serde_json::json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "inner",
        "max_depth": 1
    });

    // Act: two sequential calls sharing the same CodeAnalyzer (and call_graph_cache).
    // Second call is sent only after first response, so the cache is populated for the lookup.
    let (resp1, resp2) = call_tool_twice_sequential("analyze_symbol", params).await;

    assert!(is_success(&resp1), "first call must succeed; got: {resp1}");
    assert!(is_success(&resp2), "second call must succeed; got: {resp2}");

    assert!(
        structured_content(&resp1).is_none(),
        "structuredContent must be absent"
    );

    // Assert: second call succeeds and carries no structuredContent payload.
    assert!(
        structured_content(&resp2).is_none(),
        "structuredContent must be absent"
    );
}

#[tokio::test]
async fn test_analyze_symbol_cache_invalidates_on_file_change() {
    // Arrange: temp Rust fixture inside CWD.
    let cwd = std::env::current_dir().expect("must have cwd");
    let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
    let fixture = dir.path().join("lib.rs");
    let source = "fn inner() {}\n\nfn outer() {\n    inner();\n}\n";
    std::fs::write(&fixture, source).expect("write fixture");

    let params = serde_json::json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "inner",
        "max_depth": 1
    });

    // First pair: populate cache, confirm L1 hit on second call.
    let (resp1, resp2) = call_tool_twice_sequential("analyze_symbol", params.clone()).await;
    assert!(is_success(&resp1), "pair1 call1 must succeed");
    assert!(is_success(&resp2), "pair1 call2 must succeed");
    assert!(
        structured_content(&resp1).is_none(),
        "structuredContent must be absent"
    );
    assert!(
        structured_content(&resp2).is_none(),
        "structuredContent must be absent"
    );

    // Advance mtime: sleep >= 1 s so the filesystem registers a new mtime.
    std::thread::sleep(std::time::Duration::from_secs(1));
    std::fs::write(&fixture, source).expect("touch fixture");

    // Second pair on a fresh analyzer: the mtime has changed, so the key is different.
    // Both calls in this pair must be misses because the new mtime is reflected in the key.
    // Call 1: miss (new mtime -> new key, empty cache).
    // Call 2: L1 hit (same mtime as call 1 -- no further file changes between them).
    let (resp3, resp4) = call_tool_twice_sequential("analyze_symbol", params.clone()).await;
    assert!(is_success(&resp3), "pair2 call1 must succeed");
    assert!(is_success(&resp4), "pair2 call2 must succeed");

    assert!(
        structured_content(&resp3).is_none(),
        "structuredContent must be absent"
    );
    assert!(
        structured_content(&resp4).is_none(),
        "structuredContent must be absent"
    );
}
