use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::Mutex as TokioMutex;
use tracing_subscriber::filter::LevelFilter;

use crate::CodeAnalyzer;

pub fn make_analyzer() -> CodeAnalyzer {
    let peer = Arc::new(TokioMutex::new(None));
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    CodeAnalyzer::new(
        peer,
        log_level_filter,
        crate::metrics::MetricsSender(metrics_tx),
    )
}
