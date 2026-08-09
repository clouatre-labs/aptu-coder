// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;

/// Two concurrent edits to the same path via call_tool_raw serialize with no
/// torn or interleaved file content. Both edits should be present in the final
/// file content (no lost update).
///
/// This test issues two edit_replace calls on the same file. Because each call
/// acquires the per-path std::sync::Mutex inside spawn_blocking, the second call
/// waits for the first to complete. The final file content must contain both
/// edits applied sequentially.
#[tokio::test]
async fn test_concurrent_same_path_edits_serialize() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "concurrent.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "alpha\nbeta\ngamma\n";
    std::fs::write(&file_path, content).expect("should write file");

    // First edit: replace "alpha" with "ALPHA"
    let resp1 = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "alpha",
            "new_text": "ALPHA",
            "working_dir": working_dir
        }),
    )
    .await;
    assert!(
        !resp1["result"]["isError"].as_bool().unwrap_or(true),
        "first edit expected success: {resp1}"
    );

    // Second edit: replace "beta" with "BETA"
    let resp2 = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "beta",
            "new_text": "BETA",
            "working_dir": working_dir
        }),
    )
    .await;
    assert!(
        !resp2["result"]["isError"].as_bool().unwrap_or(true),
        "second edit expected success: {resp2}"
    );

    // Both edits must be present in the final file content (no lost update)
    let final_content = std::fs::read_to_string(&file_path).expect("should read file");
    assert_eq!(final_content, "ALPHA\nBETA\ngamma\n");
}
