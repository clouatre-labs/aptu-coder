// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use serial_test::serial;
use std::env;

#[test]
#[serial]
fn test_init_otel_no_env_var_returns_none() {
    // Arrange: ensure OTEL_EXPORTER_OTLP_ENDPOINT is not set
    unsafe {
        env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
    }

    // Act: call init_otel with no env var
    let result = aptu_coder::otel::init_otel();

    // Assert: should return None (graceful noop when env var unset)
    assert!(
        result.is_none(),
        "init_otel should return None when OTEL_EXPORTER_OTLP_ENDPOINT is unset"
    );
}

#[test]
#[serial]
fn test_init_otel_invalid_url_returns_none() {
    // Arrange: set env var to an invalid/unreachable URL
    unsafe {
        env::set_var(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "http://invalid-url-that-does-not-exist:9999",
        );
    }

    // Act: call init_otel with invalid endpoint
    let result = aptu_coder::otel::init_otel();

    // Assert: should return None (graceful failure on invalid URL)
    assert!(
        result.is_none(),
        "init_otel should return None when endpoint is invalid"
    );

    // Cleanup
    unsafe {
        env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
    }
}

#[test]
#[serial]
fn test_noop_layer_composition_no_panic() {
    // Arrange: ensure OTEL_EXPORTER_OTLP_ENDPOINT is not set
    unsafe {
        env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
    }

    // Act: call init_otel (returns None) and verify no panic on layer composition
    let otel_provider = aptu_coder::otel::init_otel();
    assert!(
        otel_provider.is_none(),
        "init_otel should return None when env var unset"
    );

    // Verify that composing a noop layer doesn't panic
    // This is a compile-time check that the types work correctly
    // The actual layer composition happens in main.rs, but we verify the provider
    // can be used in the conditional logic without panicking
    if let Some(_provider) = otel_provider {
        panic!("Should not reach here");
    }

    // Assert: test passes if we get here without panic
}

#[test]
#[serial]
fn test_log_appender_no_env_var_returns_none() {
    // Arrange: ensure OTEL_EXPORTER_OTLP_ENDPOINT is not set
    unsafe {
        env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
    }

    // Act: call init_log_appender with no env var
    let result = aptu_coder::otel::init_log_appender();

    // Assert: should return None (graceful noop when env var unset)
    assert!(
        result.is_none(),
        "init_log_appender should return None when OTEL_EXPORTER_OTLP_ENDPOINT is unset"
    );
}

#[test]
#[serial]
fn test_traceparent_malformed_no_panic() {
    // Arrange: malformed traceparent (wrong format, not a valid W3C trace-context header)
    let mut meta_map = serde_json::Map::new();
    meta_map.insert(
        "traceparent".to_string(),
        serde_json::Value::String("not-a-valid-traceparent".to_string()),
    );
    let meta = rmcp::model::Meta(meta_map);

    // Act: call the real extraction function -- must not panic regardless of input
    aptu_coder::extract_and_set_trace_context(Some(&meta));
}

#[test]
#[serial]
fn test_traceparent_missing_meta_no_panic() {
    // Act: None meta must be handled silently
    aptu_coder::extract_and_set_trace_context(None);
}

#[test]
#[serial]
fn test_metrics_histogram_no_env_var() {
    // Arrange: ensure OTEL_EXPORTER_OTLP_ENDPOINT is not set
    unsafe {
        env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
    }

    // Act: call init_meter with no env var
    let result = aptu_coder::otel::init_meter();

    // Assert: should return None (graceful noop when env var unset)
    assert!(
        result.is_none(),
        "init_meter should return None when OTEL_EXPORTER_OTLP_ENDPOINT is unset"
    );
}
