// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use opentelemetry::global;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::warn;

/// Session and client metadata recorded as span attributes on every tool call.
pub struct ClientMetadata {
    pub session_id: Option<String>,
    pub client_name: Option<String>,
    pub client_version: Option<String>,
}

/// Extract W3C Trace Context from MCP request _meta field and set as parent span context.
///
/// Attempts to extract traceparent and tracestate from the request's _meta field.
/// If successful, calls `set_parent` on the current tracing span so the OTel layer
/// re-parents it to the caller's trace. This must be called after the `#[instrument]`
/// span has been entered (i.e., inside the function body) for `set_parent` to take effect.
/// If extraction fails or _meta is absent, silently proceeds with root context (no panic).
pub fn extract_and_set_trace_context(
    meta: Option<&rmcp::model::Meta>,
    client_meta: ClientMetadata,
) {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let span = tracing::Span::current();

    // Record session and client attributes
    if let Some(sid) = client_meta.session_id {
        span.record("mcp.session.id", &sid);
    }
    if let Some(cn) = client_meta.client_name {
        span.record("client.name", &cn);
    }
    if let Some(cv) = client_meta.client_version {
        span.record("client.version", &cv);
    }

    // Extract agent-session-id from _meta if present (opportunistic; silent no-op if absent)
    if let Some(asi_str) = meta.and_then(|m| m.0.get("agent-session-id").and_then(|v| v.as_str())) {
        span.record("mcp.client.session.id", asi_str);
    }

    let Some(meta) = meta else { return };

    let mut propagation_map = std::collections::HashMap::new();

    // Extract traceparent if present
    if let Some(traceparent) = meta.0.get("traceparent")
        && let Some(tp_str) = traceparent.as_str()
    {
        propagation_map.insert("traceparent".to_string(), tp_str.to_string());
    }

    // Extract tracestate if present
    if let Some(tracestate) = meta.0.get("tracestate")
        && let Some(ts_str) = tracestate.as_str()
    {
        propagation_map.insert("tracestate".to_string(), ts_str.to_string());
    }

    // Only attempt extraction if we have at least traceparent
    if propagation_map.is_empty() {
        return;
    }

    // Extract context via the globally registered propagator (TraceContextPropagator by default)
    let parent_cx = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&ExtractMap(&propagation_map))
    });

    // Re-parent the current tracing span (already entered via #[instrument]) to the
    // extracted OTel context. set_parent is a no-op if the OTel layer is not installed.
    let _ = span.set_parent(parent_cx);
}

/// Helper struct for W3C Trace Context extraction from HashMap
struct ExtractMap<'a>(&'a std::collections::HashMap<String, String>);

impl<'a> opentelemetry::propagation::Extractor for ExtractMap<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

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
