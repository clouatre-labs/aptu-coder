// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use aptu_coder::logging::LogEvent;
use common::call_tool_raw;
use rmcp::model::{CallToolResult, Content, LoggingLevel, Meta};

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
    let resp = call_tool_raw(
        "analyze_directory",
        serde_json::json!({
            "path": "/etc/passwd",
            "max_depth": 0,
            "page_size": 10
        }),
    )
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
