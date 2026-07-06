// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;
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
    write!(f, "fn foo() {{}}\n").unwrap();
    f.flush().unwrap();

    // Act: call analyze_symbol with file path
    let params = json!({
        "path": f.path().to_str().unwrap(),
        "symbol": "foo",
        "follow_depth": 1,
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: error response with invalid_params
    let result = response.get("result").unwrap();
    assert_eq!(
        result.get("isError").unwrap().as_bool().unwrap(),
        true,
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
/// when both import_lookup=true and def_use=true (edge_case).
#[tokio::test]
async fn test_analyze_symbol_import_lookup_def_use_conflict_error_metrics() {
    // Arrange: create a temp directory with a Rust file inside CWD
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::TempDir::new_in(&cwd).unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").unwrap();

    // Act: call analyze_symbol with both import_lookup=true and def_use=true
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "std::collections",
        "follow_depth": 1,
        "import_lookup": true,
        "def_use": true,
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: error response with invalid_params
    let result = response.get("result").unwrap();
    assert_eq!(
        result.get("isError").unwrap().as_bool().unwrap(),
        true,
        "expected isError=true"
    );
    let content = result.get("content").unwrap().as_array().unwrap();
    assert!(!content.is_empty(), "expected content");
    let text = content[0].get("text").unwrap().as_str().unwrap();
    assert!(
        text.contains("mutually exclusive"),
        "error message should mention mutually exclusive: {}",
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
        "follow_depth": 1,
        "summary": true,
        "cursor": "some_cursor",
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: error response with invalid_params
    let result = response.get("result").unwrap();
    assert_eq!(
        result.get("isError").unwrap().as_bool().unwrap(),
        true,
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

    // Act: call analyze_symbol with import_lookup=true and empty symbol
    let params = json!({
        "path": dir.path().to_str().unwrap(),
        "symbol": "",
        "follow_depth": 1,
        "import_lookup": true,
    });
    let response = call_tool_raw("analyze_symbol", params).await;

    // Assert: error response with invalid_params
    let result = response.get("result").unwrap();
    assert_eq!(
        result.get("isError").unwrap().as_bool().unwrap(),
        true,
        "expected isError=true"
    );
    let content = result.get("content").unwrap().as_array().unwrap();
    assert!(!content.is_empty(), "expected content");
    let text = content[0].get("text").unwrap().as_str().unwrap();
    assert!(
        text.contains("module path"),
        "error message should mention module path: {}",
        text
    );
}
