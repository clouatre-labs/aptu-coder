mod common;

use aptu_coder_core::analyze::FileAnalysisOutput;
use aptu_coder_core::graph::{GraphDiskStore, StructuralGraph};
use aptu_coder_core::types::{CallInfo, FunctionInfo, SemanticAnalysis};
use base64::Engine as _;
use common::send_raw_request;
use serial_test::serial;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

fn make_graph() -> StructuralGraph {
    let mut caller = FunctionInfo::default();
    caller.name = "caller".to_string();
    caller.line = 1;
    caller.end_line = 5;
    let mut callee = FunctionInfo::default();
    callee.name = "callee".to_string();
    callee.line = 10;
    callee.end_line = 15;
    let call: CallInfo =
        serde_json::from_str(r#"{"caller":"caller","callee":"callee","line":2,"column":0}"#)
            .expect("valid call JSON");
    let analysis = SemanticAnalysis::new(
        vec![caller, callee],
        vec![],
        vec![],
        vec![],
        Default::default(),
        vec![call],
        vec![],
    );
    StructuralGraph::build_from_analysis(&[FileAnalysisOutput::new(
        "test.rs".to_string(),
        "test.rs:1:1:1".to_string(),
        analysis,
        20,
        None,
    )])
}

async fn read_with_event(
    uri: &str,
    cache_dir: &std::path::Path,
) -> (serde_json::Value, aptu_coder::MetricEvent) {
    unsafe {
        std::env::set_var("APTU_CODER_DISK_CACHE_DIR", cache_dir);
    }
    let (tx, mut rx) = mpsc::unbounded_channel();
    let peer = Arc::new(Mutex::new(None));
    let analyzer = aptu_coder::CodeAnalyzer::new(peer, aptu_coder::MetricsSender(tx));
    let response =
        send_raw_request(analyzer, "resources/read", serde_json::json!({"uri": uri})).await;
    let event = rx.recv().await.expect("read_resource emits one event");
    (response, event)
}

#[tokio::test]
#[serial]
async fn test_resources_read_metric_emission_all_outcomes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let hash = "metric-test-repo";
    GraphDiskStore::new(dir.path().to_path_buf()).put(hash, &make_graph());

    let (response, event) = read_with_event(
        &format!("aptu-coder://graph/{hash}/blast-radius/caller"),
        dir.path(),
    )
    .await;
    assert!(response.get("error").is_none());
    assert_eq!(event.tool, "read_resource");
    assert_eq!(event.result, "ok");
    assert_eq!(event.uri_kind.as_deref(), Some("graph_blast_radius"));
    assert!(event.duration_ms > 0 || event.output_chars > 0);
    assert!(event.output_chars > 0);
    assert_eq!(event.cache_tier, Some("l2_disk"));
    assert!(event.session_id.is_some());
    assert_eq!(event.seq, Some(0));

    let (response, event) = read_with_event(
        "aptu-coder://graph/missing-repo/blast-radius/caller",
        dir.path(),
    )
    .await;
    assert!(response.get("error").is_some());
    assert_eq!(event.result, "error");
    assert_eq!(event.error_type.as_deref(), Some("RESOURCE_NOT_FOUND"));
    assert_eq!(event.cache_tier, Some("miss"));

    let (response, event) = read_with_event("not-a-resource-uri", dir.path()).await;
    assert!(response.get("error").is_some());
    assert_eq!(event.uri_kind.as_deref(), Some("unknown"));
    assert_eq!(event.cache_tier, None);
    let serialized = serde_json::to_string(&event).expect("event serializes");
    assert!(!serialized.contains("not-a-resource-uri"));

    let cursor = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"g":0}"#);
    let (response, event) = read_with_event(
        &format!("aptu-coder://graph/{hash}/subgraph/caller?cursor={cursor}"),
        dir.path(),
    )
    .await;
    assert!(response.get("error").is_none());
    assert!(event.is_paginated);
    assert_eq!(event.uri_kind.as_deref(), Some("graph_subgraph"));
}
