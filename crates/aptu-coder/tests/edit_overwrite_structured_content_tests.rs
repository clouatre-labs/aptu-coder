// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! Drift-guard tests: edit_overwrite must not mirror structuredContent
//! on success (MCP 2026-07-28 alpha policy) and its tools/list entry must
//! not carry an outputSchema. Mirrors the analyze_module precedent (#1647).

mod common;

use common::{call_tool_raw, make_test_analyzer, send_raw_request};
use serde_json::json;

/// edit_overwrite success responses must not carry structuredContent.
#[tokio::test]
async fn test_edit_overwrite_success_has_no_structured_content() {
    // Arrange: temp file inside CWD so validate_path accepts it.
    let cwd = std::env::current_dir().expect("must have cwd");
    let path = cwd.join("edit_overwrite_drift_guard_tmp.txt");
    let content = "drift guard";

    // Act
    let response = call_tool_raw(
        "edit_overwrite",
        json!({ "path": path.to_str().expect("utf8 path"), "content": content }),
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
        "edit_overwrite call must succeed: {result}"
    );
    assert!(
        result.get("structuredContent").is_none(),
        "structuredContent must be absent: {result}"
    );

    // Cleanup
    std::fs::remove_file(&path).expect("cleanup fixture");
}

/// The tools/list entry for edit_overwrite must not advertise an outputSchema.
#[tokio::test]
async fn test_edit_overwrite_tools_list_has_no_output_schema() {
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
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("edit_overwrite"))
        .expect("edit_overwrite must be listed");
    assert!(
        tool.get("outputSchema").is_none(),
        "outputSchema must be absent from edit_overwrite: {tool}"
    );
}
