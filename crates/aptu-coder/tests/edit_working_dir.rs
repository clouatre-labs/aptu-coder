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

/// edit_overwrite without working_dir creates a new file relative to server CWD.
#[tokio::test]
async fn edit_overwrite_new_file_no_working_dir() {
    // Arrange: create a temp dir inside CWD for the target file.
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let file_name = "new_output.txt";
    let expected_path = temp_dir.path().join(file_name);
    // Build the relative path from CWD to the temp dir file.
    let temp_stem = temp_dir
        .path()
        .file_name()
        .expect("temp dir has file name")
        .to_str()
        .expect("temp dir name is valid UTF-8");
    let relative_path = format!("{temp_stem}/{file_name}");

    // Act: call edit_overwrite without working_dir
    let resp = call_tool_raw(
        "edit_overwrite",
        serde_json::json!({
            "path": relative_path,
            "content": "hello no working_dir"
        }),
    )
    .await;

    // Assert: tool must succeed and file must exist and contain correct content
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success but got error: {resp}"
    );
    assert!(
        expected_path.exists(),
        "file should exist at {:?}",
        expected_path
    );
    let written = std::fs::read_to_string(&expected_path).expect("should read written file");
    assert_eq!(written, "hello no working_dir");
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

/// edit_overwrite on a read-only directory returns an Io error without leaking path in message.
///
/// edit_overwrite uses an atomic write (NamedTempFile + rename). rename(2) succeeds on a
/// read-only *file* as long as the parent directory is writable, so the test locks the
/// directory (0o555) rather than the file. A non-writable directory blocks both the temp-file
/// creation and the rename, producing the expected Io error on any privilege level.
#[cfg(unix)]
#[tokio::test]
async fn edit_overwrite_io_error_no_path_leak() {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    // Arrange: create a temp dir inside CWD with a file inside it.
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let file_name = "readonly.txt";
    let file_path = temp_dir.path().join(file_name);
    std::fs::write(&file_path, "original content").expect("should write file");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");

    // Lock the parent directory (0o555). This blocks NamedTempFile::new_in and rename(2),
    // which is what edit_overwrite uses internally (atomic write via tempfile + persist).
    std::fs::set_permissions(temp_dir.path(), Permissions::from_mode(0o555))
        .expect("should set directory permissions");

    // Root-skip: root can create files even in a non-writable directory on some kernels.
    // Probe by attempting to create a new file in the locked directory.
    let probe_path = temp_dir.path().join("probe");
    if std::fs::write(&probe_path, "probe").is_ok() {
        std::fs::set_permissions(temp_dir.path(), Permissions::from_mode(0o755)).ok();
        return;
    }

    // Act
    let resp = call_tool_raw(
        "edit_overwrite",
        serde_json::json!({
            "path": file_name,
            "content": "new content",
            "working_dir": working_dir
        }),
    )
    .await;

    // Restore directory permissions before TempDir drops (drop needs write access to rmdir).
    std::fs::set_permissions(temp_dir.path(), Permissions::from_mode(0o755)).ok();

    // Assert: error without path in message
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        !msg.contains(file_name),
        "error message must not contain file name: {msg}"
    );
    assert!(
        !msg.contains(working_dir),
        "error message must not contain working_dir path: {msg}"
    );
}

/// edit_overwrite reports invalid working_dir without exposing path in error message.
#[tokio::test]
async fn edit_overwrite_invalid_working_dir_no_path_leak() {
    // Arrange: use a non-existent path as working_dir
    let bad_wd = "/nonexistent-working-dir-for-edit-overwrite-test";
    let resp = call_tool_raw(
        "edit_overwrite",
        serde_json::json!({
            "path": "test.txt",
            "content": "hello",
            "working_dir": bad_wd
        }),
    )
    .await;

    // Assert: error without raw path in message
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        !msg.contains(bad_wd),
        "error message must not contain working_dir path: {msg}"
    );
}

/// edit_replace reports invalid working_dir without exposing path in error message.
#[tokio::test]
async fn edit_replace_invalid_working_dir_no_path_leak() {
    // Arrange: use a non-existent path as working_dir
    let bad_wd = "/nonexistent-working-dir-for-edit-replace-test";
    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": "test.txt",
            "old_text": "old",
            "new_text": "new",
            "working_dir": bad_wd
        }),
    )
    .await;

    // Assert: error without raw path in message
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        !msg.contains(bad_wd),
        "error message must not contain working_dir path: {msg}"
    );
}

#[tokio::test]
async fn test_edit_replace_empty_new_text_deletes_block() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "delete_block.txt";
    let file_path = temp_dir.path().join(file_name);
    std::fs::write(&file_path, "line one\nDELETE ME\nline three\n").expect("should write file");

    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "DELETE ME\n",
            "new_text": "",
            "working_dir": working_dir
        }),
    )
    .await;

    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success, got: {resp}"
    );
    let content = std::fs::read_to_string(&file_path).expect("should read updated file");
    assert_eq!(content, "line one\nline three\n");
}

#[tokio::test]
async fn test_edit_replace_empty_new_text_entire_file() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "whole_file.txt";
    let file_path = temp_dir.path().join(file_name);
    std::fs::write(&file_path, "entire content").expect("should write file");

    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "entire content",
            "new_text": "",
            "working_dir": working_dir
        }),
    )
    .await;

    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success, got: {resp}"
    );
    let content = std::fs::read_to_string(&file_path).expect("should read updated file");
    assert_eq!(
        content, "",
        "file should be empty after full-content deletion"
    );
}
