// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::call_tool_raw;
use common::call_tool_raw_seq;

/// When old_text is not found, the error message includes "The file begins:"
/// with a preview of the first 20 lines of the file.
#[tokio::test]
async fn test_edit_replace_not_found_shows_file_preview() {
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

    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(msg.contains("The file begins:"));
    assert!(msg.contains("line one"));
    assert!(msg.contains("Nearest match:"));
    assert!(!msg.contains(working_dir));
    assert!(!msg.contains(file_name));
}

/// When old_text matches multiple locations, the error message includes
/// "Occurrences at lines:" with the 1-based line numbers of each match.
#[tokio::test]
async fn test_edit_replace_ambiguous_shows_line_numbers() {
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

    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(msg.contains("Occurrences at lines:"));
    assert!(msg.contains("2 locations"));
    assert!(!msg.contains(working_dir));
    assert!(!msg.contains(file_name));
}

/// Circuit breaker trips after EDIT_STALE_THRESHOLD (5) consecutive not_found
/// errors on the same (session_id, canonical_path) pair.
#[tokio::test]
async fn test_circuit_breaker_trips_at_threshold() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "unique content here\n";
    std::fs::write(&file_path, content).expect("should write file");

    let bad_params = serde_json::json!({
        "path": file_name,
        "old_text": "nonexistent text",
        "new_text": "replacement",
        "working_dir": working_dir
    });
    let calls: Vec<(&str, serde_json::Value)> = vec![("edit_replace", bad_params); 6];

    let responses = call_tool_raw_seq(calls).await;

    for (i, resp) in responses.iter().enumerate().take(5) {
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "call {} expected error: {resp}",
            i + 1
        );
    }

    let fifth_msg = responses[4]["result"]["content"][0]["text"]
        .as_str()
        .expect("call 5 should have text");
    assert!(
        fifth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 5 should contain EDIT_STALE_CONTEXT but got: {fifth_msg}"
    );
    assert!(
        fifth_msg.contains("5 consecutive"),
        "call 5 should mention 5 consecutive but got: {fifth_msg}"
    );
    assert!(
        !fifth_msg.contains(working_dir),
        "stale_context message must not contain working_dir: {fifth_msg}"
    );

    let sixth_msg = responses[5]["result"]["content"][0]["text"]
        .as_str()
        .expect("call 6 should have text");
    assert!(
        sixth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 6 should still contain stale-context but got: {sixth_msg}"
    );
    assert!(
        !sixth_msg.contains(working_dir),
        "stale_context message must not contain working_dir: {sixth_msg}"
    );
}

/// A successful edit_replace resets the circuit breaker counter for that path.
#[tokio::test]
async fn test_circuit_breaker_resets_on_edit_replace_success() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "Hello, world!\n";
    std::fs::write(&file_path, content).expect("should write file");

    let mut calls: Vec<(&str, serde_json::Value)> = Vec::new();
    for _ in 0..4 {
        calls.push((
            "edit_replace",
            serde_json::json!({
                "path": file_name,
                "old_text": "nonexistent",
                "new_text": "x",
                "working_dir": working_dir
            }),
        ));
    }
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "Hello, world!",
            "new_text": "Replaced!",
            "working_dir": working_dir
        }),
    ));
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "nonexistent",
            "new_text": "x",
            "working_dir": working_dir
        }),
    ));

    let responses = call_tool_raw_seq(calls).await;

    for (i, resp) in responses.iter().enumerate().take(4) {
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "call {} expected error: {resp}",
            i + 1
        );
    }
    let fifth_resp = &responses[4];
    assert!(
        !fifth_resp["result"]["isError"].as_bool().unwrap_or(true),
        "call 5 expected success but got error: {fifth_resp}"
    );
    let sixth_resp = &responses[5];
    assert!(
        sixth_resp["result"]["isError"].as_bool().unwrap_or(false),
        "call 6 expected error but got success: {sixth_resp}"
    );
    let sixth_msg = sixth_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("call 6 should have text");
    assert!(
        !sixth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 6 should NOT be stale_context after success reset but got: {sixth_msg}"
    );
}

/// Successful edit on the same path (via edit_replace after file rewrite) resets
/// the circuit breaker counter. Tests the same code path as edit_overwrite reset.
#[tokio::test]
async fn test_circuit_breaker_resets_on_successful_edit() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    let content = "some content\n";
    std::fs::write(&file_path, content).expect("should write file");

    let mut calls: Vec<(&str, serde_json::Value)> = Vec::new();
    for _ in 0..2 {
        calls.push((
            "edit_replace",
            serde_json::json!({
                "path": file_name,
                "old_text": "nonexistent",
                "new_text": "x",
                "working_dir": working_dir
            }),
        ));
    }
    // Rewrite file then do a successful edit_replace (same reset path as edit_overwrite)
    std::fs::write(&file_path, "replacement content\n").expect("should rewrite file");
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "replacement content\n",
            "new_text": "overwritten\n",
            "working_dir": working_dir
        }),
    ));
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "still nonexistent",
            "new_text": "x",
            "working_dir": working_dir
        }),
    ));

    let responses = call_tool_raw_seq(calls).await;

    for (i, resp) in responses.iter().enumerate().take(2) {
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "call {} expected error: {resp}",
            i + 1
        );
    }
    let third_resp = &responses[2];
    assert!(
        !third_resp["result"]["isError"].as_bool().unwrap_or(true),
        "call 3 (edit_replace) expected success but got error: {third_resp}"
    );
    let fourth_resp = &responses[3];
    assert!(
        fourth_resp["result"]["isError"].as_bool().unwrap_or(false),
        "call 4 expected error: {fourth_resp}"
    );
    let fourth_msg = fourth_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("call 4 should have text");
    assert!(
        !fourth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 4 should NOT be stale_context after success reset but got: {fourth_msg}"
    );
    assert!(
        fourth_msg.contains("not found"),
        "call 4 should be normal not_found after success reset but got: {fourth_msg}"
    );
}

/// Path isolation: 5 failures on path A do not affect the counter for path B.
#[tokio::test]
async fn test_circuit_breaker_path_isolation() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_a = "file_a.txt";
    let file_b = "file_b.txt";
    std::fs::write(temp_dir.path().join(file_a), "content a\n").expect("should write file a");
    std::fs::write(temp_dir.path().join(file_b), "content b\n").expect("should write file b");

    let mut calls: Vec<(&str, serde_json::Value)> = Vec::new();
    for _ in 0..5 {
        calls.push((
            "edit_replace",
            serde_json::json!({
                "path": file_a,
                "old_text": "nonexistent",
                "new_text": "x",
                "working_dir": working_dir
            }),
        ));
    }
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_b,
            "old_text": "nonexistent",
            "new_text": "x",
            "working_dir": working_dir
        }),
    ));

    let responses = call_tool_raw_seq(calls).await;

    for (i, resp) in responses.iter().enumerate().take(4) {
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "call {} on file_a expected error: {resp}",
            i + 1
        );
    }
    let fifth_msg = responses[4]["result"]["content"][0]["text"]
        .as_str()
        .expect("call 5 on file_a should have text");
    assert!(
        fifth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 5 on file_a should contain EDIT_STALE_CONTEXT but got: {fifth_msg}"
    );
    let sixth_resp = &responses[5];
    assert!(
        sixth_resp["result"]["isError"].as_bool().unwrap_or(false),
        "call 6 on file_b expected error: {sixth_resp}"
    );
    let sixth_msg = sixth_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("call 6 on file_b should have text");
    assert!(
        !sixth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 6 on file_b should NOT be stale_context (path isolation) but got: {sixth_msg}"
    );
    assert!(
        sixth_msg.contains("not found"),
        "call 6 on file_b should be normal not_found but got: {sixth_msg}"
    );
}

/// 5 consecutive ambiguous failures trigger EDIT_STALE_CONTEXT.
/// Verifies the stale_context message does not contain the absolute working_dir path.
#[tokio::test]
async fn test_circuit_breaker_trips_via_ambiguous() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    // Content where "foo" appears twice -> old_text "foo" matches ambiguously
    let content = "foo\nbar\nfoo\n";
    std::fs::write(&file_path, content).expect("should write file");

    let bad_params = serde_json::json!({
        "path": file_name,
        "old_text": "foo",
        "new_text": "baz",
        "working_dir": working_dir
    });
    let calls: Vec<(&str, serde_json::Value)> = vec![("edit_replace", bad_params); 5];

    let responses = call_tool_raw_seq(calls).await;

    for (i, resp) in responses.iter().enumerate().take(4) {
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "call {} expected error: {resp}",
            i + 1
        );
    }

    let fifth_msg = responses[4]["result"]["content"][0]["text"]
        .as_str()
        .expect("call 5 should have text");
    assert!(
        fifth_msg.contains("EDIT_STALE_CONTEXT"),
        "call 5 should contain EDIT_STALE_CONTEXT but got: {fifth_msg}"
    );
    assert!(
        fifth_msg.contains("5 consecutive"),
        "call 5 should mention 5 consecutive but got: {fifth_msg}"
    );
    assert!(
        !fifth_msg.contains(working_dir),
        "stale_context message must not contain working_dir: {fifth_msg}"
    );
}

/// A successful edit_overwrite resets the circuit breaker counter for that path.
/// After tripping with 5 not_found failures, an edit_overwrite on the same file
/// should clear the counter so the next edit_replace returns a normal error (not stale_context).
#[tokio::test]
async fn test_circuit_breaker_edit_overwrite_resets() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let working_dir = temp_dir
        .path()
        .to_str()
        .expect("temp dir path is valid UTF-8");
    let file_name = "test.txt";
    let file_path = temp_dir.path().join(file_name);
    std::fs::write(&file_path, "hello\n").expect("should write initial file");

    // Build 5 not_found failures to trip the breaker + 1 edit_overwrite + 1 edit_replace
    let mut calls: Vec<(&str, serde_json::Value)> = Vec::new();
    for _ in 0..5 {
        calls.push((
            "edit_replace",
            serde_json::json!({
                "path": file_name,
                "old_text": "nonexistent",
                "new_text": "x",
                "working_dir": working_dir
            }),
        ));
    }
    calls.push((
        "edit_overwrite",
        serde_json::json!({
            "path": file_name,
            "content": "new content\n",
            "working_dir": working_dir
        }),
    ));
    calls.push((
        "edit_replace",
        serde_json::json!({
            "path": file_name,
            "old_text": "still nonexistent",
            "new_text": "x",
            "working_dir": working_dir
        }),
    ));

    let responses = call_tool_raw_seq(calls).await;

    // Calls 1-5 should all be errors
    for (i, resp) in responses.iter().enumerate().take(5) {
        assert!(
            resp["result"]["isError"].as_bool().unwrap_or(false),
            "call {} expected error: {resp}",
            i + 1
        );
    }

    // Call 6 (edit_overwrite) should be success
    let sixth_resp = &responses[5];
    assert!(
        !sixth_resp["result"]["isError"].as_bool().unwrap_or(true),
        "call 6 (edit_overwrite) expected success but got error: {sixth_resp}\nworking_dir: {working_dir}",
    );

    // Call 7 (edit_replace) should be error but NOT stale_context
    let seventh_resp = &responses[6];
    assert!(
        seventh_resp["result"]["isError"].as_bool().unwrap_or(false),
        "call 7 expected error: {seventh_resp}"
    );
    let seventh_msg = seventh_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("call 7 should have text");
    assert!(
        !seventh_msg.contains("EDIT_STALE_CONTEXT"),
        "call 7 should not contain EDIT_STALE_CONTEXT (counter was reset by edit_overwrite) but got: {seventh_msg}"
    );
    assert!(
        !seventh_msg.contains(working_dir),
        "error message must not contain working_dir: {seventh_msg}"
    );
}
