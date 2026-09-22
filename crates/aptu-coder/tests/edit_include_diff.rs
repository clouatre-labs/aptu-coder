// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;

/// Creates a temp dir inside CWD seeded with `content`; returns
/// (temp_dir, file_name, working_dir string). Tests must bind the TempDir
/// so it outlives the tool call.
fn setup_file(name: &str, content: &str) -> (tempfile::TempDir, String, String) {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir_string = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8")
        .to_string();
    std::fs::write(temp_dir.path().join(name), content).expect("should write file");
    (temp_dir, name.to_string(), working_dir_string)
}

/// Asserts success and returns the single text block content.
fn success_text<'a>(resp: &'a serde_json::Value, context: &'a str) -> &'a str {
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success but got error: {resp}"
    );
    let content = resp["result"]["content"].as_array().expect("content array");
    assert_eq!(content.len(), 1, "must be a single text block: {resp}");
    content[0]["text"].as_str().expect(context)
}

#[tokio::test]
async fn edit_replace_include_diff_appends_fenced_patch() {
    // Arrange
    let (_dir, name, working_dir) = setup_file("diff_single.txt", "alpha\nbeta\ngamma\n");

    // Act
    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": name,
            "old_text": "beta",
            "new_text": "BETA",
            "working_dir": working_dir,
            "include_diff": true
        }),
    )
    .await;

    // Assert
    let text = success_text(&resp, "text block");
    assert!(text.starts_with("Edited "));
    assert!(text.contains("```diff\n"));
    assert!(text.contains("-beta"));
    assert!(text.contains("+BETA"));
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["diff_truncated"], serde_json::json!(false));
    assert!(sc["diff_bytes"].as_u64().expect("diff_bytes") > 0);
}

#[tokio::test]
async fn edit_replace_batch_include_diff_diffs_pre_vs_post_batch() {
    // Arrange
    let (_dir, name, working_dir) = setup_file(
        "diff_batch.txt",
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\n",
    );

    // Act
    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": name,
            "working_dir": working_dir,
            "include_diff": true,
            "edits": [
                {"old_text": "two", "new_text": "TWO"},
                {"old_text": "five", "new_text": "FIVE"}
            ]
        }),
    )
    .await;

    // Assert: both changes appear in one pre-vs-post-batch patch.
    let text = success_text(&resp, "text block");
    assert!(text.contains("-two"));
    assert!(text.contains("+TWO"));
    assert!(text.contains("-five"));
    assert!(text.contains("+FIVE"));
    assert!(
        resp["result"]["structuredContent"]["diff_bytes"]
            .as_u64()
            .is_some()
    );
}

#[tokio::test]
async fn edit_replace_without_include_diff_output_unchanged() {
    // Arrange
    let (_dir, name, working_dir) = setup_file("diff_off.txt", "alpha\nbeta\ngamma\n");

    // Act
    let resp = call_tool_raw(
        "edit_replace",
        serde_json::json!({
            "path": name,
            "old_text": "beta",
            "new_text": "BETA",
            "working_dir": working_dir
        }),
    )
    .await;

    // Assert: default leaves output unchanged: no fence, no diff fields.
    let text = success_text(&resp, "text block");
    assert!(!text.contains("```diff"));
    let sc = &resp["result"]["structuredContent"];
    assert!(sc.get("diff_truncated").is_none() || sc["diff_truncated"].is_null());
    assert!(sc.get("diff_bytes").is_none() || sc["diff_bytes"].is_null());
}

#[tokio::test]
async fn edit_overwrite_include_diff_nonexistent_path_shows_creation() {
    // Arrange: temp dir with no file; overwrite targets a new path.
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir.path().to_str().expect("utf-8").to_string();
    let name = "diff_create.txt";

    // Act
    let resp = call_tool_raw(
        "edit_overwrite",
        serde_json::json!({
            "path": name,
            "content": "created line\n",
            "working_dir": working_dir,
            "include_diff": true
        }),
    )
    .await;

    // Assert: failed pre-read treated as empty document -> diff shows creation.
    let text = success_text(&resp, "text block");
    assert!(text.contains("```diff\n"));
    assert!(text.contains("+created line"));
    assert_eq!(
        resp["result"]["structuredContent"]["diff_truncated"],
        serde_json::json!(false)
    );
}

#[tokio::test]
async fn edit_overwrite_include_diff_whole_file_and_empty_patch_suppressed() {
    // Arrange
    let (dir, name, working_dir) = setup_file("diff_overwrite.txt", "old line\n");
    let params = |content: &str| {
        serde_json::json!({
            "path": name,
            "content": content,
            "working_dir": working_dir,
            "include_diff": true
        })
    };

    // Act 1: real change -> fenced whole-file diff
    let resp = call_tool_raw("edit_overwrite", params("new line\n")).await;
    let text = success_text(&resp, "text block");
    assert!(text.contains("```diff\n"));
    assert!(text.contains("-old line"));
    assert!(text.contains("+new line"));
    assert_eq!(
        resp["result"]["structuredContent"]["diff_truncated"],
        serde_json::json!(false)
    );

    // Act 2: identical content -> empty patch, no fence
    let resp2 = call_tool_raw("edit_overwrite", params("new line\n")).await;
    let text2 = success_text(&resp2, "text block");
    assert!(!text2.contains("```diff"));
    assert_eq!(
        resp2["result"]["structuredContent"]["diff_bytes"],
        serde_json::json!(0)
    );
    drop(dir);
}

#[tokio::test]
async fn edit_overwrite_include_diff_binary_pre_edit_emits_no_diff() {
    // Arrange: pre-edit file is not valid UTF-8, so no reliable baseline
    // exists and the diff fence must be suppressed.
    let cwd = std::env::current_dir().expect("should get cwd");
    let dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = dir.path().to_str().expect("utf-8").to_string();
    let name = "diff_binary.bin";
    std::fs::write(dir.path().join(name), [0xFF, 0xFE, 0x00, 0x01]).expect("should write binary");

    // Act
    let resp = call_tool_raw(
        "edit_overwrite",
        serde_json::json!({
            "path": name,
            "content": "text content\n",
            "working_dir": working_dir,
            "include_diff": true
        }),
    )
    .await;

    // Assert: summary and structured fields remain, but no diff fence.
    let text = success_text(&resp, "text block");
    assert!(text.starts_with("Wrote "));
    assert!(!text.contains("```diff"));
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["diff_truncated"], serde_json::json!(false));
    assert_eq!(sc["diff_bytes"], serde_json::json!(0));
}
