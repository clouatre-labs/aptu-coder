use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

use crate::CodeAnalyzer;

pub fn make_analyzer() -> CodeAnalyzer {
    let peer = Arc::new(TokioMutex::new(None));
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    CodeAnalyzer::new(peer, crate::metrics::MetricsSender(metrics_tx))
}
