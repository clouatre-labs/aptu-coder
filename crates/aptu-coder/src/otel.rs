// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use opentelemetry::global;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::warn;

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

    // Create resource with service metadata
    let resource = Resource::builder()
        .with_attribute(opentelemetry::KeyValue::new("service.name", "aptu-coder"))
        .with_attribute(opentelemetry::KeyValue::new(
            "service.version",
            env!("CARGO_PKG_VERSION"),
        ))
        .build();

    // Build provider with batch exporter for async export
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    // Register globally
    global::set_tracer_provider(provider.clone());

    Some(provider)
}
