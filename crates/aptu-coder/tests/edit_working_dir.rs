// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;

/// edit_overwrite with working_dir writes the file inside working_dir, not server CWD.
#[tokio::test]
async fn edit_overwrite_working_dir_writes_inside_working_dir() {
    // Arrange: create a temp dir inside CWD to act as working_dir.
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "output.txt";
    let expected_path = temp_dir.path().join(file_name);

    // Act: call edit_overwrite with a relative path and working_dir
    let resp = call_tool_raw(
        "edit_overwrite",
        serde_json::json!({
            "path": file_name,
            "content": "hello from working_dir",
            "working_dir": working_dir
        }),
    )
    .await;

    // Assert: tool must succeed and file must exist inside working_dir
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success but got error: {resp}"
    );
    assert!(
        expected_path.exists(),
        "file should exist inside working_dir at {:?}",
        expected_path
    );
    let written = std::fs::read_to_string(&expected_path).expect("should read written file");
    assert_eq!(written, "hello from working_dir");
}

/// edit_replace with working_dir modifies the correct file inside working_dir, not server CWD.
#[tokio::test]
async fn edit_replace_working_dir_modifies_inside_working_dir() {
    // Arrange: create a temp dir inside CWD with a pre-existing file.
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "source.txt";
    let file_path = temp_dir.path().join(file_name);
    std::fs::write(&file_path, "old text here").expect("should write initial file");

    // Act: call edit_replace with a relative path and working_dir
    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "old text here",
            "new_text": "new text here",
            "working_dir": working_dir
        }),
    )
    .await;

    // Assert: tool must succeed and file inside working_dir must be updated
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success but got error: {resp}"
    );
    let updated = std::fs::read_to_string(&file_path).expect("should read updated file");
    assert_eq!(updated, "new text here");
}
