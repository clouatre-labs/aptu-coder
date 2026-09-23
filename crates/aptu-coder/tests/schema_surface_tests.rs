// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Integration tests for the one-time `schema_surface` startup metric event.

mod common;

#[tokio::test]
async fn schema_surface_emitted_once_per_server_start() {
    let (_analyzer, mut metrics_rx) = common::make_test_analyzer_with_metrics();

    let mut schema_events = 0;
    while let Ok(event) = metrics_rx.try_recv() {
        if event.tool == "schema_surface" {
            schema_events += 1;
            let map = event.schema_chars.expect("schema_chars populated");
            assert!(
                !map.is_empty(),
                "per-tool schema_chars map must be non-empty"
            );
            assert!(map.values().all(|v| *v > 0), "every schema char count > 0");
            let total: usize = map.values().sum();
            assert_eq!(event.output_chars, total, "output_chars holds the total");
            assert!(total > 0, "total schema chars must be > 0");
        }
    }
    assert_eq!(
        schema_events, 1,
        "exactly one schema_surface event per start"
    );
}
