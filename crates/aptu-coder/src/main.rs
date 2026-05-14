// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder::{
    CodeAnalyzer,
    logging::McpLoggingLayer,
    metrics::{MetricEvent, MetricsSender, MetricsWriter},
};
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

async fn run_http(analyzer: CodeAnalyzer, port: u16) -> Result<(), Box<dyn std::error::Error>> {
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

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    eprintln!("Listening on http://127.0.0.1:{port}/mcp");

    axum::serve(listener, router)
        .with_graceful_shutdown(async move { ct.cancelled().await })
        .await?;
    Ok(())
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
                Some(port_str) => match port_str.parse::<u16>() {
                    Ok(p) => port = Some(p),
                    Err(_) => {
                        eprintln!("error: --port requires a valid u16 value");
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
