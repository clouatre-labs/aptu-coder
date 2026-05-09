// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder::{
    CodeAnalyzer,
    logging::McpLoggingLayer,
    metrics::{MetricEvent, MetricsSender, MetricsWriter},
};
use rmcp::serve_server;
use rmcp::transport::stdio;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as TokioMutex;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

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

    // Build layered subscriber: fmt + MCP logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(mcp_logging_layer)
        .init();

    // Create metrics channel and spawn writer
    let (metrics_tx, metrics_rx) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
    tokio::spawn(MetricsWriter::new(metrics_rx, None).run());

    let analyzer = CodeAnalyzer::new(peer, log_level_filter, event_rx, MetricsSender(metrics_tx));
    let (stdin, stdout) = stdio();

    let service = serve_server(analyzer, (stdin, stdout)).await?;
    service.waiting().await?;

    Ok(())
}
