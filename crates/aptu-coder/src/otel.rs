// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use opentelemetry::global;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::warn;

/// Builds the standard service resource attached to all three signal providers.
fn service_resource() -> Resource {
    Resource::builder()
        .with_attribute(opentelemetry::KeyValue::new("service.name", "aptu-coder"))
        .with_attribute(opentelemetry::KeyValue::new(
            "service.version",
            env!("CARGO_PKG_VERSION"),
        ))
        .build()
}

/// Initializes OpenTelemetry with OTLP export if OTEL_EXPORTER_OTLP_ENDPOINT is set.
///
/// Returns `Some(provider)` if initialization succeeds, or `None` if:
/// - The env var is unset (noop provider, zero overhead)
/// - The exporter fails to build (logs warning, graceful failure)
///
/// The provider is registered globally via `opentelemetry::global::set_tracer_provider`.
pub fn init_otel() -> Option<SdkTracerProvider> {
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;

    // Build the OTLP exporter with HTTP proto transport
    let exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(&endpoint)
        .build()
    {
        Ok(exp) => exp,
        Err(e) => {
            warn!("Failed to build OTLP exporter: {}", e);
            return None;
        }
    };

    // Build provider with batch exporter for async export
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(service_resource())
        .build();

    // Register globally
    global::set_tracer_provider(provider.clone());

    Some(provider)
}

/// Initializes OpenTelemetry log appender if OTEL_EXPORTER_OTLP_ENDPOINT is set.
///
/// Returns `Some(provider)` if initialization succeeds, or `None` if:
/// - The env var is unset (noop, zero overhead)
/// - The exporter fails to build (logs warning, graceful failure)
///
/// The provider is returned for use with OpenTelemetryTracingBridge layer.
pub fn init_log_appender() -> Option<SdkLoggerProvider> {
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;

    // Build the OTLP log exporter with HTTP proto transport
    let exporter = match opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(&endpoint)
        .build()
    {
        Ok(exp) => exp,
        Err(e) => {
            warn!("Failed to build OTLP log exporter: {}", e);
            return None;
        }
    };

    // Build provider with batch processor for async export
    let provider = SdkLoggerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(service_resource())
        .build();

    Some(provider)
}

/// Initializes OpenTelemetry metrics SDK if OTEL_EXPORTER_OTLP_ENDPOINT is set.
///
/// Returns `Some(provider)` if initialization succeeds, or `None` if:
/// - The env var is unset (noop, zero overhead)
/// - The exporter fails to build (logs warning, graceful failure)
///
/// The provider is registered globally via `opentelemetry::global::set_meter_provider`.
pub fn init_meter() -> Option<SdkMeterProvider> {
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;

    // Build the OTLP metrics exporter with HTTP proto transport
    let exporter = match opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(&endpoint)
        .build()
    {
        Ok(exp) => exp,
        Err(e) => {
            warn!("Failed to build OTLP metrics exporter: {}", e);
            return None;
        }
    };

    // Build provider with periodic reader for async export
    let provider = SdkMeterProvider::builder()
        .with_reader(opentelemetry_sdk::metrics::PeriodicReader::builder(exporter).build())
        .with_resource(service_resource())
        .build();

    // Register globally
    global::set_meter_provider(provider.clone());

    Some(provider)
}
