// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder::{LogEvent, McpLoggingLayer};
use rmcp::model::LoggingLevel;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;

fn make_layer(filter: LevelFilter) -> (McpLoggingLayer, mpsc::UnboundedReceiver<LogEvent>) {
    let (tx, rx) = mpsc::unbounded_channel::<LogEvent>();
    let level_filter = Arc::new(Mutex::new(filter));
    let layer = McpLoggingLayer::new(tx, level_filter);
    (layer, rx)
}

#[test]
fn test_logging_layer_forwards_event() {
    let (layer, mut rx) = make_layer(LevelFilter::INFO);
    let subscriber = tracing_subscriber::Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "test_target", "hello world");
    });

    let event = rx.try_recv().expect("expected a LogEvent");
    assert_eq!(event.level, LoggingLevel::Info);
    assert!(
        event.logger.contains("test_target"),
        "logger should contain target; got: {}",
        event.logger
    );
    let msg = event
        .data
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        msg.contains("hello world"),
        "message field should contain 'hello world'; got: {msg}"
    );
}

#[test]
fn test_logging_layer_level_filter_warn_drops_info() {
    let (layer, mut rx) = make_layer(LevelFilter::WARN);
    let subscriber = tracing_subscriber::Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "test_filter", "this should be dropped");
        tracing::warn!(target: "test_filter", "this should pass");
    });

    let first = rx.try_recv().expect("expected exactly 1 event (WARN)");
    assert_eq!(first.level, LoggingLevel::Warning);
    assert!(
        rx.try_recv().is_err(),
        "expected no more events after the WARN one"
    );
}

#[test]
fn test_logging_layer_closed_receiver_no_panic() {
    let (tx, rx) = mpsc::unbounded_channel::<LogEvent>();
    // Drop receiver immediately to simulate closed channel
    drop(rx);

    let level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let layer = McpLoggingLayer::new(tx, level_filter);
    let subscriber = tracing_subscriber::Registry::default().with(layer);

    // Must not panic when sending to a closed channel
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "test_closed", "event with no receiver");
    });
}

#[test]
fn test_logging_layer_field_types_roundtrip() {
    let (layer, mut rx) = make_layer(LevelFilter::INFO);
    let subscriber = tracing_subscriber::Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            target: "test_fields",
            count = 42u64,
            offset = -7i64,
            active = true,
            label = "rust",
            "structured fields"
        );
    });

    let event = rx.try_recv().expect("expected a LogEvent");
    let data = &event.data;

    assert_eq!(
        data.get("count").and_then(|v| v.as_u64()),
        Some(42),
        "u64 field mismatch"
    );
    assert_eq!(
        data.get("offset").and_then(|v| v.as_i64()),
        Some(-7),
        "i64 field mismatch"
    );
    assert_eq!(
        data.get("active").and_then(|v| v.as_bool()),
        Some(true),
        "bool field mismatch"
    );
    assert_eq!(
        data.get("label").and_then(|v| v.as_str()),
        Some("rust"),
        "str field mismatch"
    );
}
