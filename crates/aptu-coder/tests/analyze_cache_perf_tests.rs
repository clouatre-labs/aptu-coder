// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! Tests for analyze-path cache reuse: L1 hits must not re-walk or re-hash,
//! and `_meta.content_hash` must be identical between a miss and a subsequent
//! L1 hit (golden comparison).

mod common;

use aptu_coder::MetricEvent;
use common::make_test_analyzer_with_metrics;
use rmcp::serve_server;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::UnboundedReceiver;

/// Shared MCP connection with a metrics tap for sequential calls.
struct Harness {
    client_tx: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    reader: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    metrics_rx: UnboundedReceiver<MetricEvent>,
}

impl Harness {
    async fn new() -> Self {
        let (analyzer, metrics_rx) = make_test_analyzer_with_metrics();
        let (client, server) = tokio::io::duplex(65536);
        tokio::spawn(async move {
            let (server_rx, server_tx) = tokio::io::split(server);
            if let Ok(service) = serve_server(analyzer, (server_rx, server_tx)).await {
                let _ = service.waiting().await;
            }
        });
        let (client_rx, mut client_tx) = tokio::io::split(client);
        let mut reader = BufReader::new(client_rx).lines();

        let init = json!({
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
        reader.next_line().await.unwrap().unwrap();

        let notif = json!({
            "jsonrpc": "2.0", "method": "notifications/initialized", "params": {}
        })
        .to_string()
            + "\n";
        client_tx.write_all(notif.as_bytes()).await.unwrap();
        client_tx.flush().await.unwrap();

        Self {
            client_tx,
            reader,
            metrics_rx,
        }
    }

    async fn call(&mut self, id: u64, tool: &str, params: &Value) -> Value {
        let msg = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": params}
        })
        .to_string()
            + "\n";
        self.client_tx.write_all(msg.as_bytes()).await.unwrap();
        self.client_tx.flush().await.unwrap();
        loop {
            let line = self.reader.next_line().await.unwrap().unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return v;
            }
        }
    }

    /// Drain metrics until the ok event for `tool` arrives.
    async fn ok_event(&mut self, tool: &str) -> MetricEvent {
        while let Some(event) = self.metrics_rx.recv().await {
            if event.tool == tool && event.result == "ok" {
                return event;
            }
        }
        panic!("no ok metric event for {tool}");
    }
}

fn is_success(resp: &Value) -> bool {
    !resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn result_text(resp: &Value) -> String {
    resp["result"]["content"]
        .as_array()
        .and_then(|c| c.first())
        .and_then(|c| c["text"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn content_hash(resp: &Value) -> Option<String> {
    resp["result"]["_meta"]
        .get("content_hash")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
}

#[tokio::test]
async fn test_analyze_directory_l1_hit_serves_without_rewalk() {
    // Arrange: temp Rust fixture inside CWD so validate_path accepts the path.
    let cwd = std::env::current_dir().expect("must have cwd");
    let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), "fn main() {}\n").expect("write fixture");
    let params = json!({ "path": dir.path().to_str().unwrap() });

    let mut h = Harness::new().await;

    // Act: first call is a miss, second call must be served from L1 memory.
    let resp1 = h.call(2, "analyze_directory", &params).await;
    let event1 = h.ok_event("analyze_directory").await;
    let resp2 = h.call(3, "analyze_directory", &params).await;
    let event2 = h.ok_event("analyze_directory").await;

    // Assert: both succeed with byte-identical text; second call is an L1 hit.
    assert!(is_success(&resp1), "first call must succeed: {resp1}");
    assert!(is_success(&resp2), "second call must succeed: {resp2}");
    assert_eq!(result_text(&resp1), result_text(&resp2));
    assert_eq!(event1.cache_tier, Some("miss"));
    assert_eq!(event2.cache_tier, Some("l1_memory"));
    assert_eq!(event2.cache_hit, Some(true));
    // A short-circuited L1 hit returns the cached output verbatim (same chars).
    assert_eq!(event1.output_chars, event2.output_chars);
}

#[tokio::test]
async fn test_analyze_symbol_l1_hit_reuses_content_hash() {
    // Arrange.
    let cwd = std::env::current_dir().expect("must have cwd");
    let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
    std::fs::write(
        dir.path().join("lib.rs"),
        "fn inner() {}\n\nfn outer() {\n    inner();\n}\n",
    )
    .expect("write fixture");
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "inner",
        "max_depth": 1
    });

    let mut h = Harness::new().await;

    // Act: call 1 (miss) then call 2 (L1 hit).
    let resp1 = h.call(2, "analyze_symbol", &params).await;
    let _ = h.ok_event("analyze_symbol").await;
    let resp2 = h.call(3, "analyze_symbol", &params).await;
    let _ = h.ok_event("analyze_symbol").await;

    // Assert: identical text and identical _meta.content_hash across tiers
    // (golden comparison: the put-time hash over pre-cursor text matches the
    // hash computed on the miss).
    assert!(is_success(&resp1), "first call must succeed: {resp1}");
    assert!(is_success(&resp2), "second call must succeed: {resp2}");
    assert_eq!(result_text(&resp1), result_text(&resp2));
    let hash1 = content_hash(&resp1).expect("content_hash on miss");
    let hash2 = content_hash(&resp2).expect("content_hash on L1 hit");
    assert!(!hash1.is_empty());
    assert_eq!(hash1, hash2, "L1 hit must serve the put-time content hash");
}
