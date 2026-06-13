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

    let mut server_handle = tokio::spawn(async move {
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
            "protocolVersion": "2025-11-25",
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

/// Return the `cache_tier` string from a successful analyze_symbol response's structuredContent.
fn extract_cache_tier(resp: &serde_json::Value) -> Option<String> {
    resp["result"]["structuredContent"]
        .get("cache_tier")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
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
        "follow_depth": 1
    });

    // Act: two sequential calls sharing the same CodeAnalyzer (and call_graph_cache).
    // Second call is sent only after first response, so the cache is populated for the lookup.
    let (resp1, resp2) = call_tool_twice_sequential("analyze_symbol", params).await;

    assert!(is_success(&resp1), "first call must succeed; got: {resp1}");
    assert!(is_success(&resp2), "second call must succeed; got: {resp2}");

    let tier1 = extract_cache_tier(&resp1);
    assert_eq!(
        tier1.as_deref(),
        Some("miss"),
        "first call must be a cache miss; got: {tier1:?}"
    );

    // Assert: second call returns L1Memory tier (same analyzer instance, unchanged directory).
    let tier2 = extract_cache_tier(&resp2);
    assert_eq!(
        tier2.as_deref(),
        Some("l1_memory"),
        "second call on unchanged input must be an L1 cache hit; got: {tier2:?}; resp: {resp2}"
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
        "follow_depth": 1
    });

    // First pair: populate cache, confirm L1 hit on second call.
    let (resp1, resp2) = call_tool_twice_sequential("analyze_symbol", params.clone()).await;
    assert!(is_success(&resp1), "pair1 call1 must succeed");
    assert!(is_success(&resp2), "pair1 call2 must succeed");
    assert_eq!(
        extract_cache_tier(&resp1).as_deref(),
        Some("miss"),
        "pair1 call1 must be a miss"
    );
    assert_eq!(
        extract_cache_tier(&resp2).as_deref(),
        Some("l1_memory"),
        "pair1 call2 must be an L1 hit (cache populated)"
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

    let tier3 = extract_cache_tier(&resp3);
    assert_eq!(
        tier3.as_deref(),
        Some("miss"),
        "after mtime change, first call on fresh analyzer must be a miss; got: {tier3:?}"
    );
    let tier4 = extract_cache_tier(&resp4);
    assert_eq!(
        tier4.as_deref(),
        Some("l1_memory"),
        "after mtime change, second call on same analyzer must be L1 hit; got: {tier4:?}"
    );
}
