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

/// analyze_file default response (no fields): the text block must include the
/// classes and functions sections; the response carries no structuredContent.
#[tokio::test]
async fn analyze_file_default_response_includes_semantic_sections() {
    // Arrange: a real source file in the repo; no fields projection requested.
    let path = "src/lib.rs";

    // Act: call analyze_file with only the path.
    let resp = call_tool_raw("analyze_file", serde_json::json!({ "path": path })).await;

    // Assert: success, sections present in the text block, structuredContent
    // absent.
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success but got error: {resp}"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("result should carry text content");
    assert!(
        text.contains("\nC:\n"),
        "classes section must be present by default: {text}"
    );
    assert!(
        text.contains("\nF:\n"),
        "functions section must be present by default: {text}"
    );
    assert!(
        resp["result"].get("structuredContent").is_none(),
        "structuredContent must be absent: {resp}"
    );
}

/// analyze_file with fields=[references, calls]: the requested fields are
/// accepted and the response carries a text block only (no structuredContent).
#[tokio::test]
async fn analyze_file_fields_references_and_calls_project_to_text() {
    // Arrange: a real source file and an explicit references+calls projection.
    let path = "src/lib.rs";

    // Act: call analyze_file requesting references and calls sections.
    let resp = call_tool_raw(
        "analyze_file",
        serde_json::json!({ "path": path, "fields": ["references", "calls"] }),
    )
    .await;

    // Assert: success, and the response carries a text block only.
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success but got error: {resp}"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("result should carry text content");
    assert!(
        text.starts_with("FILE: "),
        "expected details-mode output header, got: {text}"
    );
    assert!(
        resp["result"].get("structuredContent").is_none(),
        "structuredContent must be absent: {resp}"
    );
}
