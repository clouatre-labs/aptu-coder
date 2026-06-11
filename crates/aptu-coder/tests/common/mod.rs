// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;
use tracing_subscriber::filter::LevelFilter;

pub fn make_test_analyzer() -> aptu_coder::CodeAnalyzer {
    let peer = Arc::new(TokioMutex::new(None));
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<aptu_coder::logging::LogEvent>();
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    aptu_coder::CodeAnalyzer::new(
        peer,
        log_level_filter,
        rx,
        aptu_coder::metrics::MetricsSender(metrics_tx),
    )
}

pub async fn call_tool_raw(tool_name: &str, params: serde_json::Value) -> serde_json::Value {
    let analyzer = make_test_analyzer();
    let (client, server) = tokio::io::duplex(65536);

    // Spawn the analyzer server on the server half
    let mut server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = rmcp::serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

    // Step 1: Send initialize request
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
    client_tx.write_all(init.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 2: Read initialize response (discard)
    let _resp = reader.next_line().await.unwrap().unwrap();

    // Step 3: Send initialized notification (no id)
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    })
    .to_string()
        + "\n";
    client_tx.write_all(notif.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 4: Send tools/call
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": params
        }
    })
    .to_string()
        + "\n";
    client_tx.write_all(call.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 5: Race response loop against server handle to surface server panics
    tokio::select! {
        result = async {
            loop {
                let line = reader.next_line().await.unwrap().unwrap();
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
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
                Ok(_) => panic!("server task exited unexpectedly before tool response"),
                Err(e) => panic!("server task panicked: {e}"),
            }
        }
    }
}
