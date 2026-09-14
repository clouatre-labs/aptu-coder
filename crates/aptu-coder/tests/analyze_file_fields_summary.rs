// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;

/// analyze_file with fields=[functions] and summary=true: today the handler
/// takes the summary=true branch and silently ignores `fields`, returning
/// compact summary output. This locks the behavior that the response_format
/// mapping table in docs/audit/2026-09-14-response-format-experiment-design.md
/// documents as behavior-preserving narrowing (not lossy).
#[tokio::test]
async fn analyze_file_fields_with_summary_true_silently_ignores_fields() {
    // Arrange: a real source file in the repo and a fields projection request
    // combined with summary=true.
    let path = "src/lib.rs";

    // Act: call analyze_file with both fields and summary=true.
    let resp = call_tool_raw(
        "analyze_file",
        serde_json::json!({
            "path": path,
            "fields": ["functions"],
            "summary": true
        }),
    )
    .await;

    // Assert: success, and the output is summary-mode (fields silently ignored).
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success but got error: {resp}"
    );

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("result should carry text content");

    // Summary-mode output starts with the compact FILE: header and includes
    // the "N L, M F, K C" tallies line; it does not include the per-function
    // detail listing that a fields=[functions] projection would produce.
    assert!(
        text.starts_with("FILE:\n"),
        "expected summary-mode output header, got: {text}"
    );
    let tally = text
        .lines()
        .find(|l| l.trim().ends_with('C') && l.contains('F'))
        .expect("summary output should contain the L/F/C tallies line");
    assert!(
        tally.contains("L, ") && tally.contains("F, ") && tally.ends_with('C'),
        "unexpected tallies line format: {tally}"
    );
}
