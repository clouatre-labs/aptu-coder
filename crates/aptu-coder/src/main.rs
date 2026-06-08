// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder::{
    CodeAnalyzer,
    logging::McpLoggingLayer,
    metrics::{MetricEvent, MetricsSender, MetricsWriter},
};
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::IntoResponse;
use rmcp::serve_server;
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rustls::crypto::aws_lc_rs;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod otel;

/// Authentication middleware that validates Bearer tokens using constant-time comparison.
///
/// `expected` is the blake3 hash of the token computed once at startup. Per-request, only the
/// incoming token is hashed and compared via `blake3::Hash`'s `PartialEq`, which uses
/// `constant_time_eq_32` internally, preventing timing side-channels.
async fn auth_middleware(
    State(expected): State<blake3::Hash>,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let Some(incoming) = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    };

    if expected == blake3::hash(incoming.as_bytes()) {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }
}

async fn run_http(analyzer: CodeAnalyzer, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let bearer_token = std::env::var("APTU_CODER_BEARER_TOKEN").ok();

    let ct = CancellationToken::new();
    let ct_signal = ct.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = sigterm.recv() => {},
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        ct_signal.cancel();
    });

    let config = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_sse_retry(None)
        .with_cancellation_token(ct.child_token());

    let service: StreamableHttpService<CodeAnalyzer, NeverSessionManager> =
        StreamableHttpService::new(
            move || Ok(analyzer.clone()),
            Arc::new(NeverSessionManager::default()),
            config,
        );

    let base_router = axum::Router::new().nest_service("/mcp", service);
    let router = if let Some(token) = bearer_token {
        if token.len() < 32 {
            tracing::warn!(
                token_len = token.len(),
                "APTU_CODER_BEARER_TOKEN is shorter than 32 characters; \
                 use a random token of at least 32 characters for production deployments"
            );
        }
        let expected_hash = blake3::hash(token.as_bytes());
        base_router.layer(axum::middleware::from_fn_with_state(
            expected_hash,
            auth_middleware,
        ))
    } else {
        base_router
    };

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    tracing::info!(port, "Listening on http://127.0.0.1:{port}/mcp");

    axum::serve(listener, router)
        .with_graceful_shutdown(async move { ct.cancelled().await })
        .await?;
    Ok(())
}

/// Parse a port string into a non-zero u16.  Returns `Err` with a human-readable message on
/// failure so callers share identical validation logic regardless of whether the value came
/// from a CLI flag or an environment variable.
fn parse_port(s: &str) -> Result<u16, String> {
    match s.parse::<u16>() {
        Ok(0) => Err("must be a non-zero u16 value".to_string()),
        Ok(p) => Ok(p),
        Err(_) => Err(format!("{s:?} is not a valid u16 value")),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls CryptoProvider");

    let mut port: Option<u16> = None;
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--port" => match args.next() {
                Some(val) => match parse_port(&val) {
                    Ok(p) => port = Some(p),
                    Err(msg) => {
                        eprintln!("error: --port {msg}");
                        std::process::exit(1);
                    }
                },
                None => {
                    eprintln!("error: --port requires a value");
                    std::process::exit(1);
                }
            },
            _ => {}
        }
    }

    // Fall back to APTU_CODER_PORT env var when --port was not passed.
    if port.is_none()
        && let Ok(val) = std::env::var("APTU_CODER_PORT")
    {
        match parse_port(&val) {
            Ok(p) => port = Some(p),
            Err(msg) => {
                eprintln!("error: APTU_CODER_PORT {msg}");
                std::process::exit(1);
            }
        }
    }

    // Initialize OpenTelemetry (returns None if OTEL_EXPORTER_OTLP_ENDPOINT is unset)
    let otel_provider = otel::init_otel();
    let log_provider = otel::init_log_appender();
    let meter_provider = otel::init_meter();

    // Create shared peer Arc for logging layer
    // Migrate legacy metrics directory if needed
    if let Err(e) = aptu_coder::metrics::migrate_legacy_metrics_dir() {
        tracing::warn!("Failed to migrate legacy metrics directory: {e}");
    }
    let peer = Arc::new(TokioMutex::new(None));

    // Create shared level filter for dynamic control (std::sync::Mutex for Copy type)
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::WARN));

    // Create unbounded channel for log events
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();

    // Create MCP logging layer with event sender
    let mcp_logging_layer = McpLoggingLayer::new(event_tx, log_level_filter.clone());

    // Build layered subscriber: fmt + MCP logging + optional OTel tracing + optional log bridge.
    // tracing_subscriber accepts Option<impl Layer>; None is a no-op, so all combinations
    // collapse to a single linear chain without branching.
    use opentelemetry::trace::TracerProvider as _;

    let otel_trace_layer = otel_provider
        .as_ref()
        .map(|p| tracing_opentelemetry::layer().with_tracer(p.tracer("aptu-coder")));

    let otel_log_layer = log_provider
        .as_ref()
        .map(opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new);

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(mcp_logging_layer)
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .init();

    // Create metrics channel and spawn writer
    let (metrics_tx, metrics_rx) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
    tokio::spawn(MetricsWriter::new(metrics_rx, None).run());

    let analyzer = CodeAnalyzer::new(peer, log_level_filter, event_rx, MetricsSender(metrics_tx));

    if let Some(p) = port {
        run_http(analyzer, p).await?;
    } else {
        let (stdin, stdout) = stdio();
        let service = serve_server(analyzer, (stdin, stdout)).await?;
        service.waiting().await?;
    }

    // Shutdown OpenTelemetry providers to flush spans, logs, and metrics
    if let Some(provider) = otel_provider
        && let Err(e) = provider.shutdown()
    {
        tracing::warn!("Failed to shutdown OpenTelemetry trace provider: {e}");
    }

    if let Some(log_prov) = log_provider
        && let Err(e) = log_prov.shutdown()
    {
        tracing::warn!("Failed to shutdown OpenTelemetry log provider: {e}");
    }

    if let Some(meter_prov) = meter_provider
        && let Err(e) = meter_prov.shutdown()
    {
        tracing::warn!("Failed to shutdown OpenTelemetry meter provider: {e}");
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn make_test_router(token: &str) -> axum::Router {
        axum::Router::new()
            .route("/ping", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                blake3::hash(token.as_bytes()),
                auth_middleware,
            ))
    }

    #[tokio::test]
    async fn test_auth_middleware_valid_token() {
        // Arrange
        let router = make_test_router("secret");
        let request = Request::builder()
            .uri("/ping")
            .header("Authorization", "Bearer secret")
            .body(Body::empty())
            .unwrap();

        // Act
        let response = router.oneshot(request).await.unwrap();

        // Assert
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_middleware_invalid_token() {
        // Arrange
        let router = make_test_router("secret");
        let request = Request::builder()
            .uri("/ping")
            .header("Authorization", "Bearer wrong")
            .body(Body::empty())
            .unwrap();

        // Act
        let response = router.oneshot(request).await.unwrap();

        // Assert
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_middleware_no_header() {
        // Arrange
        let router = make_test_router("secret");
        let request = Request::builder().uri("/ping").body(Body::empty()).unwrap();

        // Act
        let response = router.oneshot(request).await.unwrap();

        // Assert
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
