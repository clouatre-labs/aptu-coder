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

/// analyze_file default response (no fields): structuredContent semantic data
/// must not carry references or calls keys; only functions/classes/imports.
#[tokio::test]
async fn analyze_file_default_response_omits_references_and_calls() {
    // Arrange: a real source file in the repo; no fields projection requested.
    let path = "src/lib.rs";

    // Act: call analyze_file with only the path.
    let resp = call_tool_raw("analyze_file", serde_json::json!({ "path": path })).await;

    // Assert: success, and no references/calls keys in structuredContent.
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success but got error: {resp}"
    );
    let structured = &resp["result"]["structuredContent"]["semantic"];
    assert!(
        structured.get("references").is_none(),
        "references must be absent by default: {structured}"
    );
    assert!(
        structured.get("calls").is_none(),
        "calls must be absent by default: {structured}"
    );
}

/// analyze_file with fields=[references, calls]: the requested sections appear.
#[tokio::test]
async fn analyze_file_fields_references_and_calls_are_projected() {
    // Arrange: a real source file and an explicit references+calls projection.
    let path = "src/lib.rs";

    // Act: call analyze_file requesting references and calls sections.
    let resp = call_tool_raw(
        "analyze_file",
        serde_json::json!({ "path": path, "fields": ["references", "calls"] }),
    )
    .await;

    // Assert: success, and both requested keys are present in structuredContent.
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success but got error: {resp}"
    );
    let structured = &resp["result"]["structuredContent"]["semantic"];
    assert!(
        structured.get("references").is_some(),
        "references must be present when requested: {structured}"
    );
    assert!(
        structured.get("calls").is_some(),
        "calls must be present when requested: {structured}"
    );
}
