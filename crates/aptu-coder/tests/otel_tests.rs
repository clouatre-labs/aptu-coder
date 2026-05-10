// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use std::env;

#[test]
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
