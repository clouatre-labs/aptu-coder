// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;

/// When old_text is not found, the error message includes "The file begins:"
/// with a preview of the first 20 lines of the file.
#[tokio::test]
async fn test_edit_replace_not_found_shows_file_preview() {
    // Arrange: create a temp file inside CWD with known content
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "line one\nline two\nline three\n";
    std::fs::write(&file_path, content).expect("should write file");

    // Act: call edit_replace with old_text that does not exist
    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "nonexistent text",
            "new_text": "replacement",
            "working_dir": working_dir
        }),
    )
    .await;

    // Assert: error response with file preview
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        msg.contains("The file begins:"),
        "error message should contain 'The file begins:' but got: {msg}"
    );
    assert!(
        msg.contains("line one"),
        "error message should contain file content but got: {msg}"
    );
    assert!(
        msg.contains("Nearest match:"),
        "error message should contain nearest match hint but got: {msg}"
    );
    // Path must not leak into model-visible error message
    assert!(
        !msg.contains(working_dir),
        "error message must not contain working_dir path: {msg}"
    );
    assert!(
        !msg.contains(file_name),
        "error message must not contain file path: {msg}"
    );
}

/// When old_text matches multiple locations, the error message includes
/// "Occurrences at lines:" with the 1-based line numbers of each match.
#[tokio::test]
async fn test_edit_replace_ambiguous_shows_line_numbers() {
    // Arrange: create a temp file inside CWD with duplicate content on different lines
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "alpha\nbeta\nalpha\n";
    std::fs::write(&file_path, content).expect("should write file");

    // Act: call edit_replace with old_text that appears twice
    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "alpha",
            "new_text": "replacement",
            "working_dir": working_dir
        }),
    )
    .await;

    // Assert: error response with line numbers
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        msg.contains("Occurrences at lines:"),
        "error message should contain 'Occurrences at lines:' but got: {msg}"
    );
    assert!(
        msg.contains("2 locations"),
        "error message should mention match count but got: {msg}"
    );
    // Path must not leak into model-visible error message
    assert!(
        !msg.contains(working_dir),
        "error message must not contain working_dir path: {msg}"
    );
    assert!(
        !msg.contains(file_name),
        "error message must not contain file path: {msg}"
    );
}
