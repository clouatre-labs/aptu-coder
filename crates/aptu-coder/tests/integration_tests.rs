// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder::logging::LogEvent;
use rmcp::model::{CallToolResult, Content, LoggingLevel, Meta};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;
use tracing_subscriber::filter::LevelFilter;

fn make_test_analyzer() -> aptu_coder::CodeAnalyzer {
    let peer = Arc::new(TokioMutex::new(None));
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<LogEvent>();
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    aptu_coder::CodeAnalyzer::new(
        peer,
        log_level_filter,
        rx,
        aptu_coder::metrics::MetricsSender(metrics_tx),
    )
}

async fn call_analyze_directory_raw(params: serde_json::Value) -> serde_json::Value {
    let analyzer = make_test_analyzer();
    let (client, server) = tokio::io::duplex(65536);

    let mut server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = rmcp::serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

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

    let _resp = reader.next_line().await.unwrap().unwrap();

    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    })
    .to_string()
        + "\n";
    client_tx.write_all(notif.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "analyze_directory",
            "arguments": params
        }
    })
    .to_string()
        + "\n";
    client_tx.write_all(call.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

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
#[tokio::test]
async fn test_batch_draining_with_multiple_events() {
    use serde_json::json;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<LogEvent>();

    for i in 0..5 {
        let log_event = LogEvent {
            level: LoggingLevel::Info,
            logger: format!("logger_{i}"),
            data: json!({"index": i}),
        };
        let _ = event_tx.send(log_event);
    }

    let mut buffer = Vec::with_capacity(64);
    event_rx.recv_many(&mut buffer, 64).await;

    assert_eq!(buffer.len(), 5);
    for (i, event) in buffer.iter().enumerate() {
        assert_eq!(event.logger, format!("logger_{i}"));
        assert_eq!(event.data, json!({"index": i}));
    }
}

#[test]
fn test_call_tool_result_cache_hint_metadata() {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "cache_hint".to_string(),
        serde_json::Value::String("no-cache".to_string()),
    );

    let result =
        CallToolResult::success(vec![Content::text("test output")]).with_meta(Some(Meta(meta)));

    let json_val = serde_json::to_value(&result).expect("should serialize");

    assert_eq!(
        json_val
            .get("_meta")
            .and_then(|m| m.get("cache_hint"))
            .and_then(|v| v.as_str()),
        Some("no-cache"),
        "Expected _meta.cache_hint to be 'no-cache' in serialized JSON: {json_val}"
    );
}

#[tokio::test]
async fn test_path_outside_cwd_rejected() {
    // Arrange: path=/etc/passwd is outside the server's CWD
    let resp = call_analyze_directory_raw(serde_json::json!({
        "path": "/etc/passwd",
        "max_depth": 0,
        "page_size": 10
    }))
    .await;

    // Assert: handler must reject with isError=true and mention 'outside'
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true for path outside CWD: {resp}"
    );
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        content_text.contains("outside"),
        "error message should contain 'outside': {content_text}"
    );
}
