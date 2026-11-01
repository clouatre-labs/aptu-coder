// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Integration tests for the `verify_anchors` MCP tool handler.

mod common;

use common::call_tool_raw;
use serde_json::json;

fn temp_workspace(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(tag)
        .tempdir()
        .expect("temp workspace dir")
}

/// Happy path: existing file, line in range, symbol found at the target line.
#[tokio::test]
async fn verify_anchors_happy_path() {
    // Arrange
    let ws = temp_workspace("va-happy");
    std::fs::write(ws.path().join("lib.rs"), "fn one() {}\nfn foo() {}\n").expect("fixture");
    let response = call_tool_raw(
        "verify_anchors",
        json!({
            "workspace_root": ws.path().to_str().expect("utf8 path"),
            "anchors": [{ "path": "lib.rs", "line": 2, "symbol": "foo" }]
        }),
    )
    .await;

    // Act
    let structured = response["result"]["structuredContent"]["results"]
        .as_array()
        .expect("structuredContent.results array");

    // Assert
    assert_eq!(structured.len(), 1);
    assert_eq!(structured[0]["exists"].as_bool(), Some(true));
    assert_eq!(structured[0]["in_range"].as_bool(), Some(true));
    assert_eq!(structured[0]["symbol_found"].as_bool(), Some(true));
    assert_eq!(structured[0]["window_line"].as_u64(), Some(2));
}

/// Edge case: an anchor path escaping the workspace root is rejected via
/// validation before any fs access.
#[tokio::test]
async fn verify_anchors_rejects_path_escaping_workspace_root() {
    // Arrange
    let ws = temp_workspace("va-escape");
    let outside = temp_workspace("va-outside");
    std::fs::write(outside.path().join("secret.txt"), "foo\n").expect("fixture");
    let response = call_tool_raw(
        "verify_anchors",
        json!({
            "workspace_root": ws.path().to_str().expect("utf8 path"),
            "anchors": [{ "path": "../va-outside/secret.txt", "line": 1, "symbol": "foo" }]
        }),
    )
    .await;

    // Act
    let is_error = response["result"]["isError"].as_bool().unwrap_or(false);

    // Assert
    assert!(
        is_error,
        "escaping anchor path must be rejected: {response}"
    );
}

/// Edge case: an unreadable file fails closed (exists=false), never
/// fabricating a successful verdict.
#[cfg(unix)]
#[tokio::test]
async fn verify_anchors_unreadable_file_fails_closed() {
    // Arrange
    let ws = temp_workspace("va-unreadable");
    let path = ws.path().join("locked.txt");
    std::fs::write(&path, "fn foo() {}\n").expect("fixture");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let response = call_tool_raw(
        "verify_anchors",
        json!({
            "workspace_root": ws.path().to_str().expect("utf8 path"),
            "anchors": [{ "path": "locked.txt", "line": 1, "symbol": "foo" }]
        }),
    )
    .await;

    // Act
    let structured = response["result"]["structuredContent"]["results"]
        .as_array()
        .expect("structuredContent.results array");

    // Assert
    assert_eq!(structured[0]["exists"].as_bool(), Some(false));
    assert_eq!(structured[0]["symbol_found"].as_bool(), None);
}

/// Edge case: a batch over the MAX_ANCHORS cap is rejected with an error before
/// any filesystem access.
#[tokio::test]
async fn verify_anchors_over_cap_batch_fails_fast() {
    // Arrange: 101 anchors pointing at files that would otherwise resolve.
    let ws = temp_workspace("va-overcap");
    std::fs::write(ws.path().join("lib.rs"), "fn foo() {}\n").expect("fixture");
    let anchor = serde_json::json!({ "path": "lib.rs", "line": 1, "symbol": "foo" });
    let anchors: Vec<serde_json::Value> = std::iter::repeat_n(anchor, 101).collect();
    let response = call_tool_raw(
        "verify_anchors",
        json!({
            "workspace_root": ws.path().to_str().expect("utf8 path"),
            "anchors": anchors
        }),
    )
    .await;

    // Act
    let is_error = response["result"]["isError"].as_bool().unwrap_or(false);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();

    // Assert: fast-fail names the cap before any per-file work.
    assert!(is_error, "over-cap batch must be rejected: {response}");
    assert!(text.contains("maximum"), "error text: {text}");
}

/// Schema carries the maxItems constraint for the anchors array.
#[test]
fn verify_anchors_schema_has_max_items() {
    use schemars::JsonSchema;
    let mut generator = schemars::SchemaGenerator::default();
    let schema = aptu_coder_core::types::VerifyAnchorsParams::json_schema(&mut generator);
    let anchors = schema
        .get("properties")
        .and_then(|p| p.get("anchors"))
        .expect("anchors property");
    assert_eq!(anchors.get("maxItems"), Some(&serde_json::json!(100)));
}
