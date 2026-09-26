// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder::{
    CodeAnalyzer, MetricEvent, MetricsSender, MetricsWriter, init_log_appender, init_meter,
    init_otel,
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
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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
            // SAFETY: SIGTERM handling is a critical part of graceful shutdown; failure to install
            // the handler propagates as a panic since the process cannot function without it.
            #[allow(clippy::expect_used)]
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
        .with_legacy_session_mode(false)
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

/// Terminal action requested via CLI flags.
#[derive(Debug, PartialEq, Eq)]
enum CliAction {
    /// Serve over stdio (default).
    Stdio,
    /// Serve over streamable HTTP on the given port.
    Http(u16),
    /// Print the package version and exit.
    PrintVersion,
    /// Print usage help and exit.
    PrintHelp,
}

/// Parse a CLI argument iterator into a [`CliAction`], using `env_port` as the
/// fallback port source (mirrors the `APTU_CODER_PORT` environment variable).
fn parse_cli_args_with_env<I: IntoIterator<Item = String>>(
    args: I,
    env_port: Option<String>,
) -> Result<CliAction, String> {
    let mut port: Option<u16> = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--version" => return Ok(CliAction::PrintVersion),
            "--help" => {
                println!(
                    "Usage: aptu-coder [OPTIONS]\n\nFlags:\n  --port <PORT>  Serve over streamable HTTP on 127.0.0.1:<PORT> (non-zero u16)\n  --version      Print the package version and exit\n  --help         Print this help and exit\n\nTransports:\n  stdio (default when --port is omitted) or streamable HTTP when --port is set.\nThe APTU_CODER_PORT environment variable is used when --port is not passed."
                );
                return Ok(CliAction::PrintHelp);
            }
            "--port" => match iter.next() {
                Some(val) => match parse_port(&val) {
                    Ok(p) => port = Some(p),
                    Err(msg) => return Err(format!("--port {msg}")),
                },
                None => return Err("--port requires a value".to_string()),
            },
            other => return Err(format!("unknown flag: {other}")),
        }
    }

    // Fall back to the provided env port when --port was not passed.
    if port.is_none()
        && let Some(val) = env_port
    {
        match parse_port(&val) {
            Ok(p) => port = Some(p),
            Err(msg) => return Err(format!("APTU_CODER_PORT {msg}")),
        }
    }

    Ok(port.map_or(CliAction::Stdio, CliAction::Http))
}

/// Parse CLI arguments and resolve the requested action.
fn parse_cli_args() -> Result<CliAction, String> {
    parse_cli_args_with_env(
        std::env::args().skip(1),
        std::env::var("APTU_CODER_PORT").ok(),
    )
}

/// Install the global tracing subscriber with optional OpenTelemetry layers.
///
/// Builds a layered subscriber: stderr fmt layer + optional OTel tracing layer +
/// optional OTel log bridge.  OTel layers are no-ops when
/// `OTEL_EXPORTER_OTLP_ENDPOINT` is unset.  Callers retain the original
/// providers so they can be shut down after the service exits.
fn setup_tracing(
    otel_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    log_provider: Option<opentelemetry_sdk::logs::SdkLoggerProvider>,
) {
    use opentelemetry::trace::TracerProvider as _;

    let otel_trace_layer = otel_provider
        .as_ref()
        .map(|p| tracing_opentelemetry::layer().with_tracer(p.tracer("aptu-coder")));

    let otel_log_layer = log_provider
        .as_ref()
        .map(opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new);

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[allow(clippy::expect_used)]
    aws_lc_rs::default_provider()
        .install_default()
        // SAFETY: CryptoProvider installation is required for TLS; main() cannot proceed without it.
        .expect("failed to install rustls CryptoProvider: TLS is required for server startup and cannot be recovered");

    let action = match parse_cli_args() {
        Ok(CliAction::PrintVersion) => {
            println!("aptu-coder {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Ok(CliAction::PrintHelp) => return Ok(()),
        Ok(action) => action,
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
    };

    // Initialize OpenTelemetry (returns None if OTEL_EXPORTER_OTLP_ENDPOINT is unset)
    let otel_provider = init_otel();
    let log_provider = init_log_appender();
    let meter_provider = init_meter();

    // Create shared peer for CodeAnalyzer::new (used by Streamable HTTP session manager)
    let peer = Arc::new(TokioMutex::new(None));

    // Install global tracing subscriber with optional OTel layers
    setup_tracing(otel_provider.clone(), log_provider.clone());

    // Create metrics channel and spawn writer
    let (metrics_tx, metrics_rx) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
    tokio::spawn(MetricsWriter::new(metrics_rx, None).run());

    let analyzer = CodeAnalyzer::new(peer, MetricsSender(metrics_tx));

    if let CliAction::Http(p) = action {
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
        tracing::warn!("Failed to shutdown OpenTelemetry meter provider: {e}");
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

    #[test]
    fn test_parse_cli_args_version() {
        assert_eq!(
            parse_cli_args_with_env(["--version".to_string()], None),
            Ok(CliAction::PrintVersion)
        );
    }

    #[test]
    fn test_parse_cli_args_help() {
        assert_eq!(
            parse_cli_args_with_env(["--help".to_string()], None),
            Ok(CliAction::PrintHelp)
        );
    }

    #[test]
    fn test_parse_cli_args_unknown_flag() {
        let err = parse_cli_args_with_env(["--bogus".to_string()], None).unwrap_err();
        assert!(err.contains("--bogus"), "error must name the flag: {err}");
    }

    #[test]
    fn test_parse_cli_args_port() {
        assert_eq!(
            parse_cli_args_with_env(["--port".to_string(), "8080".to_string()], None),
            Ok(CliAction::Http(8080))
        );
    }

    #[test]
    fn test_parse_cli_args_no_args() {
        assert_eq!(
            parse_cli_args_with_env(Vec::new(), None),
            Ok(CliAction::Stdio)
        );
    }

    #[test]
    fn test_parse_cli_args_env_port_fallback() {
        assert_eq!(
            parse_cli_args_with_env(Vec::new(), Some("9000".to_string())),
            Ok(CliAction::Http(9000))
        );
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
