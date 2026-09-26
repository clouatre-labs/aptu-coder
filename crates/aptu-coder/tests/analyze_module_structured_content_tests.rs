// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! Drift-guard tests: analyze_module must not mirror structuredContent
//! (MCP 2026-07-28 alpha policy) and its tools/list entry must not carry
//! an outputSchema. Mirrors the analyze_symbol precedent (#1645).

mod common;

use common::{call_tool_raw, make_test_analyzer, send_raw_request};
use serde_json::json;
use std::io::Write as _;
use tempfile::NamedTempFile;

/// analyze_module success responses must not carry structuredContent.
#[tokio::test]
async fn test_analyze_module_success_has_no_structured_content() {
    // Arrange: temp Rust file inside CWD so validate_path accepts it.
    let cwd = std::env::current_dir().expect("must have cwd");
    let mut f = NamedTempFile::with_suffix_in(".rs", &cwd).expect("tempfile");
    writeln!(f, "fn foo() {{}}").expect("write fixture");
    f.flush().expect("flush fixture");

    // Act
    let response = call_tool_raw(
        "analyze_module",
        json!({ "path": f.path().to_str().expect("utf8 path") }),
    )
    .await;

    // Assert
    let result = response
        .get("result")
        .expect("tool call must return result");
    assert!(
        !result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "analyze_module call must succeed: {result}"
    );
    assert!(
        result.get("structuredContent").is_none(),
        "structuredContent must be absent: {result}"
    );
}

/// The tools/list entry for analyze_module must not advertise an outputSchema.
#[tokio::test]
async fn test_analyze_module_tools_list_has_no_output_schema() {
    // Arrange
    let analyzer = make_test_analyzer();

    // Act
    let response = send_raw_request(analyzer, "tools/list", json!({})).await;

    // Assert
    let result = response
        .get("result")
        .expect("tools/list must return result");
    let tool = result["tools"]
        .as_array()
        .expect("tools must be an array")
        .iter()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("analyze_module"))
        .expect("analyze_module must be listed");
    assert!(
        tool.get("outputSchema").is_none(),
        "outputSchema must be absent from analyze_module: {tool}"
    );
}
