// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::{call_tool_raw, make_test_analyzer, send_raw_request};
use serde_json::json;
use std::io::Write as _;
use tempfile::NamedTempFile;

/// Test that analyze_symbol emits result=error with error_type=invalid_params
/// when passed a file path instead of directory (happy_path).
#[tokio::test]
async fn test_analyze_symbol_file_path_error_metrics() {
    // Arrange: create a temp file inside CWD so validate_path accepts it
    let cwd = std::env::current_dir().unwrap();
    let mut f = NamedTempFile::with_suffix_in(".rs", &cwd).unwrap();
    writeln!(f, "fn foo() {{}}").unwrap();
    f.flush().unwrap();

    // Act: call analyze_symbol with file path
    let params = json!({
        "path": f.path().to_str().unwrap(),
        "symbol": "foo",
        "max_depth": 1,
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: error response with invalid_params
    let result = response.get("result").unwrap();
    assert!(
        result.get("isError").unwrap().as_bool().unwrap(),
        "expected isError=true"
    );
    let content = result.get("content").unwrap().as_array().unwrap();
    assert!(!content.is_empty(), "expected content");
    let text = content[0].get("text").unwrap().as_str().unwrap();
    assert!(
        text.contains("file"),
        "error message should mention file: {}",
        text
    );
}

/// Test that analyze_symbol emits result=error with error_type=invalid_params
/// when mode=import_lookup is combined with match_mode/max_depth/impl_only (edge_case).
#[tokio::test]
async fn test_analyze_symbol_mode_param_conflict_error_metrics() {
    // Arrange: create a temp directory with a Rust file inside CWD
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::TempDir::new_in(&cwd).unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").unwrap();

    // Act: call analyze_symbol with mode=import_lookup and max_depth
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "std::collections",
        "mode": "import_lookup",
        "max_depth": 1,
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: error response with invalid_params
    let result = response.get("result").unwrap();
    assert!(
        result.get("isError").unwrap().as_bool().unwrap(),
        "expected isError=true"
    );
    let content = result.get("content").unwrap().as_array().unwrap();
    assert!(!content.is_empty(), "expected content");
    let text = content[0].get("text").unwrap().as_str().unwrap();
    assert!(
        text.contains("import_lookup"),
        "error message should mention import_lookup: {}",
        text
    );
}

/// Test that analyze_symbol emits result=error with error_type=invalid_params
/// when summary=true and cursor are both provided (edge_case).
#[tokio::test]
async fn test_analyze_symbol_summary_cursor_conflict_error_metrics() {
    // Arrange: create a temp directory with a Rust file inside CWD
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::TempDir::new_in(&cwd).unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").unwrap();

    // Act: call analyze_symbol with both summary=true and cursor
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "foo",
        "max_depth": 1,
        "summary": true,
        "cursor": "some_cursor",
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: error response with invalid_params
    let result = response.get("result").unwrap();
    assert!(
        result.get("isError").unwrap().as_bool().unwrap(),
        "expected isError=true"
    );
    let content = result.get("content").unwrap().as_array().unwrap();
    assert!(!content.is_empty(), "expected content");
    let text = content[0].get("text").unwrap().as_str().unwrap();
    assert!(
        text.contains("incompatible"),
        "error message should mention incompatible: {}",
        text
    );
}

/// Test that analyze_symbol emits result=error with error_type=invalid_params
/// when import_lookup=true with empty symbol (edge_case).
#[tokio::test]
async fn test_analyze_symbol_import_lookup_empty_symbol_error_metrics() {
    // Arrange: create a temp directory with a Rust file inside CWD
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::TempDir::new_in(&cwd).unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").unwrap();

    // Act: call analyze_symbol with mode=import_lookup and empty symbol
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "",
        "mode": "import_lookup",
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: error response with invalid_params
    let result = response.get("result").unwrap();
    assert!(
        result.get("isError").unwrap().as_bool().unwrap(),
        "expected isError=true"
    );
    let content = result.get("content").unwrap().as_array().unwrap();
    assert!(!content.is_empty(), "expected content");
    let text = content[0].get("text").unwrap().as_str().unwrap();
    assert!(
        text.contains("non-empty"),
        "error message should mention non-empty: {}",
        text
    );
}

/// Test that analyze_symbol accepts max_depth at the schema maximum of 3
/// (happy_path).
#[tokio::test]
async fn test_analyze_symbol_max_depth_at_maximum_accepted() {
    // Arrange: create a temp directory with a Rust file inside CWD
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::TempDir::new_in(&cwd).unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").unwrap();

    // Act: call analyze_symbol with max_depth=3
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "foo",
        "max_depth": 3,
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: success response, not an error
    let result = response.get("result").unwrap();
    assert!(
        !result.get("isError").unwrap().as_bool().unwrap(),
        "expected isError=false for max_depth=3"
    );
}

/// Test that analyze_symbol emits result=error with error_type=invalid_params
/// when max_depth exceeds the schema maximum of 3 (edge_case).
#[tokio::test]
async fn test_analyze_symbol_max_depth_above_maximum_rejected() {
    // Arrange: create a temp directory with a Rust file inside CWD
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::TempDir::new_in(&cwd).unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").unwrap();

    // Act: call analyze_symbol with max_depth=4
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "foo",
        "max_depth": 4,
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: error response with invalid_params mentioning max_depth/maximum
    let result = response.get("result").unwrap();
    assert!(
        result.get("isError").unwrap().as_bool().unwrap(),
        "expected isError=true for max_depth=4"
    );
    let content = result.get("content").unwrap().as_array().unwrap();
    assert!(!content.is_empty(), "expected content");
    let text = content[0].get("text").unwrap().as_str().unwrap();
    assert!(
        text.contains("max_depth") && text.contains("maximum"),
        "error message should mention max_depth and maximum: {}",
        text
    );
}

/// Test that analyze_symbol with cursor="" behaves like cursor omitted:
/// first page, no error, consistent mode/offset (edge_case).
#[tokio::test]
async fn test_analyze_symbol_empty_string_cursor_returns_first_page() {
    // Arrange: create a temp directory with a Rust file inside CWD
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::TempDir::new_in(&cwd).unwrap();
    std::fs::write(
        dir.path().join("lib.rs"),
        "fn foo() {}\nfn bar() { foo(); }",
    )
    .unwrap();

    // Act: call analyze_symbol with cursor=""
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "foo",
        "max_depth": 1,
        "cursor": "",
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: success, no error
    let result = response.get("result").unwrap();
    assert!(
        !result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "expected success with cursor=\"\"; got: {response}"
    );
}

/// Test that the analyze_symbol input schema (from tools/list) contains
/// max_depth and does NOT contain follow_depth (happy_path).
#[tokio::test]
async fn test_analyze_symbol_schema_has_max_depth_no_follow_depth() {
    // Arrange: spin up the server and fetch tools/list
    let analyzer = make_test_analyzer();
    let response = send_raw_request(analyzer, "tools/list", serde_json::json!({})).await;
    let result = response
        .get("result")
        .expect("tools/list must return a result");
    let tools = result
        .get("tools")
        .and_then(|v| v.as_array())
        .expect("tools/list must return a tools array");
    let schema = tools
        .iter()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("analyze_symbol"))
        .expect("analyze_symbol must be advertised")
        .get("inputSchema")
        .cloned()
        .expect("analyze_symbol must have inputSchema");
    let schema_str = serde_json::to_string(&schema).unwrap();

    // Assert: max_depth present, follow_depth absent
    assert!(
        schema_str.contains("max_depth"),
        "inputSchema must contain max_depth: {schema_str}"
    );
    assert!(
        !schema_str.contains("follow_depth"),
        "inputSchema must not contain follow_depth: {schema_str}"
    );
}

/// Test that a call_graph-mode analyze_symbol call with max_depth=3 emits a
/// server-side WARNING in the output while a default call does not (edge_case).
#[tokio::test]
async fn test_analyze_symbol_max_depth_above_two_warns_default_does_not() {
    // Arrange: create a temp directory with a Rust file inside CWD
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::TempDir::new_in(&cwd).unwrap();
    std::fs::write(
        dir.path().join("lib.rs"),
        "fn foo() {}\nfn bar() { foo(); }",
    )
    .unwrap();

    // Act: call analyze_symbol with mode=call_graph and max_depth=3
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "foo",
        "mode": "call_graph",
        "max_depth": 3,
    });
    let response = call_tool_raw("analyze_symbol", params).await;
    let result = response.get("result").unwrap();
    assert!(
        !result.get("isError").unwrap().as_bool().unwrap(),
        "expected success for max_depth=3; got: {response}"
    );
    let text = result["content"][0]["text"].as_str().unwrap();

    // Assert: warning emitted above depth 2
    assert!(
        text.contains("WARNING: max_depth="),
        "expected WARNING for max_depth=3; got: {text}"
    );

    // Act: default call (max_depth unset)
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "foo",
    });
    let response = call_tool_raw("analyze_symbol", params).await;
    let result = response.get("result").unwrap();
    assert!(
        !result.get("isError").unwrap().as_bool().unwrap(),
        "expected success for default call; got: {response}"
    );
    let text = result["content"][0]["text"].as_str().unwrap();

    // Assert: no warning on default call
    assert!(
        !text.contains("WARNING: max_depth="),
        "default call must not emit a WARNING; got: {text}"
    );
}
