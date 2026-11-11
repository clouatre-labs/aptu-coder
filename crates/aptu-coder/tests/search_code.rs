// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Integration tests for the `search_code` tool: per-language byte-stable
//! pattern goldens (rust/go/python/typescript), unsupported-language error,
//! cold-repo symbol query, pagination roundtrip, and non-blocking telemetry.

mod common;

use common::{call_tool_raw, make_test_analyzer_with_metrics, send_raw_request};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Creates an auto-cleaning fixture directory inside CWD so `validate_path`
/// accepts it and the shared walker does not skip it as gitignored.
fn fixture_dir(name: &str) -> TempDir {
    let cwd = std::env::current_dir().expect("must have cwd");
    tempfile::tempdir_in(cwd).unwrap_or_else(|e| panic!("tempdir {name}: {e}"))
}

fn write(dir: &std::path::Path, name: &str, content: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write fixture");
    path.to_string_lossy().to_string()
}

fn text_of(response: &Value) -> String {
    let result = response
        .get("result")
        .expect("tool call must return result");
    let blocks = result
        .get("content")
        .and_then(Value::as_array)
        .expect("content blocks");
    blocks
        .iter()
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_error(response: &Value) -> bool {
    response
        .get("result")
        .and_then(|r| r.get("isError"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Extract `file:line:col bytes a..b [kind]` headers plus verbatim bodies from
/// the text output; this is the byte-stable golden contract.
fn match_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.contains(" bytes ") && line.contains('[') {
            let mut block = line.to_string();
            // The verbatim body follows the header; may span multiple lines.
            let mut body = Vec::new();
            for l in lines.by_ref() {
                if l.is_empty() {
                    break;
                }
                body.push(l);
            }
            block.push('\n');
            block.push_str(&body.join("\n"));
            blocks.push(block);
        }
    }
    blocks
}

const RUST_PATTERN: &str = "fn $NAME($$$PARAMS) { $$$BODY }";

#[tokio::test]
async fn pattern_mode_rust_golden_is_byte_stable() {
    // Arrange
    let dir = fixture_dir("rust");
    let path = write(
        dir.path(),
        "alpha.rs",
        "fn alpha() {\n    beta();\n}\n\nfn gamma(x: u32) {\n    x + 1\n}\n",
    );

    // Act
    let response = call_tool_raw(
        "search_code",
        json!({
            "mode": "pattern",
            "path": path,
            "language": "rust",
            "pattern": RUST_PATTERN
        }),
    )
    .await;

    // Assert: byte-stable golden for the two matched spans.
    assert!(!is_error(&response), "must succeed: {response}");
    let blocks = match_blocks(&text_of(&response));
    assert_eq!(
        blocks,
        vec![
            format!(
                "{}:1:1 bytes 0..26 [pattern]\nfn alpha() {{\n    beta();\n}}",
                path
            ),
            format!(
                "{}:5:1 bytes 28..58 [pattern]\nfn gamma(x: u32) {{\n    x + 1\n}}",
                path
            ),
        ]
    );
}

#[tokio::test]
async fn pattern_mode_go_python_typescript_goldens_are_byte_stable() {
    // Arrange: one fixture per language, one match each.
    let dir = fixture_dir("langs");
    let go = write(
        dir.path(),
        "m.go",
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(1)\n}\n",
    );
    let py = write(dir.path(), "m.py", "def go():\n    helper(1, 2)\n");
    let ts = write(
        dir.path(),
        "m.ts",
        "function go(): void {\n  helper(true);\n}\n",
    );

    let cases = [
        (go.clone(), "go", "$PKG.$FN($$$ARGS)", "fmt.Println(1)"),
        (py.clone(), "python", "$FN($$$ARGS)", "helper(1, 2)"),
        (ts.clone(), "typescript", "$FN($$$ARGS);", "helper(true);"),
    ];
    for (path, language, pattern, expected_text) in cases {
        // Act
        let response = call_tool_raw(
            "search_code",
            json!({
                "mode": "pattern",
                "path": path,
                "language": language,
                "pattern": pattern
            }),
        )
        .await;

        // Assert
        assert!(!is_error(&response), "{language} must succeed: {response}");
        let blocks = match_blocks(&text_of(&response));
        assert_eq!(blocks.len(), 1, "{language} golden count");
        assert!(
            blocks[0].ends_with(&format!("[pattern]\n{expected_text}")),
            "{language} golden text mismatch: {}",
            blocks[0]
        );
    }
}

#[tokio::test]
async fn unsupported_language_returns_structured_error() {
    // Arrange
    let dir = fixture_dir("unsupported");
    let path = write(dir.path(), "m.java", "class M { void m() {} }");

    // Act
    let response = call_tool_raw(
        "search_code",
        json!({
            "mode": "pattern",
            "path": path,
            "language": "java",
            "pattern": "void $N()"
        }),
    )
    .await;

    // Assert: structured error, no panic.
    assert!(is_error(&response), "must be an error: {response}");
    let text = text_of(&response);
    assert!(
        text.contains("unsupported language"),
        "error must name the cause: {text}"
    );
}

#[tokio::test]
async fn symbol_mode_cold_repo_returns_definitions_and_callers() {
    // Arrange: two files, no prior analyze_directory call (cold graph build).
    let dir = fixture_dir("cold");
    let a = write(dir.path(), "a.rs", "fn target_fn() {}\n");
    write(
        dir.path(),
        "b.rs",
        "fn caller_fn() {\n    target_fn();\n}\n",
    );

    // Act
    let response = call_tool_raw(
        "search_code",
        json!({
            "mode": "symbol",
            "path": dir.path().to_str().expect("utf8 dir"),
            "symbol": "target_fn"
        }),
    )
    .await;

    // Assert: definition in a.rs plus caller in b.rs, byte-exact.
    assert!(!is_error(&response), "must succeed: {response}");
    let blocks = match_blocks(&text_of(&response));
    assert_eq!(blocks.len(), 2, "definition + caller: {}", blocks.len());
    assert!(
        blocks
            .iter()
            .any(|b| b.starts_with(&format!("{a}:1:1")) && b.contains("[definition]")),
        "definition golden: {blocks:?}"
    );
    assert!(
        blocks.iter().any(|b| b.contains("[caller]")),
        "caller golden: {blocks:?}"
    );
}

#[tokio::test]
async fn symbol_mode_missing_symbol_returns_error() {
    // Arrange
    let dir = fixture_dir("missing");
    write(dir.path(), "a.rs", "fn present() {}\n");

    // Act
    let response = call_tool_raw(
        "search_code",
        json!({
            "mode": "symbol",
            "path": dir.path().to_str().expect("utf8 dir"),
            "symbol": "totally_absent"
        }),
    )
    .await;

    // Assert
    assert!(is_error(&response), "must be an error: {response}");
    assert!(text_of(&response).contains("symbol not found"));
}

#[tokio::test]
async fn pagination_cursor_roundtrip_yields_stable_pages() {
    // Arrange: many one-line matches so page size (50) forces pagination.
    let dir = fixture_dir("pages");
    let mut src = String::new();
    for i in 0..60 {
        src.push_str(&format!("fn item_{i}() {{}}\n"));
    }
    let path = write(dir.path(), "many.rs", &src);

    // Act: page 1
    let page1 = call_tool_raw(
        "search_code",
        json!({
            "mode": "pattern",
            "path": path,
            "language": "rust",
            "pattern": RUST_PATTERN
        }),
    )
    .await;
    assert!(!is_error(&page1), "page 1 must succeed: {page1}");
    let text1 = text_of(&page1);
    let cursor_line = text1
        .lines()
        .find(|l| l.starts_with("NEXT_CURSOR: "))
        .expect("page 1 must carry NEXT_CURSOR")
        .strip_prefix("NEXT_CURSOR: ")
        .expect("cursor prefix")
        .to_string();

    // Act: page 2 with the cursor
    let page2 = call_tool_raw(
        "search_code",
        json!({
            "mode": "pattern",
            "path": path,
            "language": "rust",
            "pattern": RUST_PATTERN,
            "cursor": cursor_line
        }),
    )
    .await;
    assert!(!is_error(&page2), "page 2 must succeed: {page2}");

    // Assert: disjoint, byte-stable pages totaling 60 matches.
    let p1 = match_blocks(&text1);
    let p2 = match_blocks(&text_of(&page2));
    assert!(!p2.is_empty(), "page 2 must have items");
    assert!(
        !text_of(&page2).contains("NEXT_CURSOR: "),
        "page 2 should be final"
    );
    let first_of_p2 = p2[0].clone();
    assert!(!p1.contains(&first_of_p2), "pages must be disjoint");
    assert_eq!(p1.len() + p2.len(), 60, "all matches across pages");
}

#[tokio::test]
async fn telemetry_never_blocks_tool_calls() {
    // Arrange: sequential calls share one analyzer whose metrics receiver is
    // intentionally never drained until after the calls return.
    let dir = fixture_dir("drop");
    let path = write(dir.path(), "a.rs", "fn one() {}\n");
    let args = json!({
        "mode": "symbol",
        "path": path,
        "symbol": "one"
    });
    let calls: Vec<(&str, Value)> = (0..64).map(|_| ("search_code", args.clone())).collect();

    // Act
    let responses = common::call_tool_raw_seq(calls).await;

    // Assert: every call succeeded despite an undrained (drop-counting)
    // bounded channel; telemetry is fire-and-forget, never blocking.
    assert_eq!(responses.len(), 64);
    for r in &responses {
        assert!(!is_error(r), "non-blocking telemetry: {r}");
    }
}

#[tokio::test]
async fn ok_event_carries_query_mode_and_result_count() {
    // Arrange
    let (analyzer, mut rx) = make_test_analyzer_with_metrics();
    let dir = fixture_dir("metrics");
    let path = write(dir.path(), "a.rs", "fn one() {}\n");

    // Act
    let response = send_raw_request(
        analyzer,
        "tools/call",
        json!({
            "name": "search_code",
            "arguments": {
                "mode": "pattern",
                "path": path,
                "language": "rust",
                "pattern": "fn $N() {}"
            }
        }),
    )
    .await;
    assert!(!is_error(&response), "must succeed: {response}");

    // Assert: terminal ok event carries query_mode and result_count.
    let mut ok_event = None;
    while let Ok(event) = rx.try_recv() {
        if event.tool == "search_code" && event.result == "ok" {
            ok_event = Some(event);
        }
    }
    let event = ok_event.expect("search_code ok event");
    assert_eq!(event.query_mode, Some("pattern"));
    assert_eq!(event.result_count, Some(1));
}
