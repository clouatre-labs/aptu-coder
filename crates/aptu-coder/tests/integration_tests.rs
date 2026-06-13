// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

mod common;

use aptu_coder::logging::LogEvent;
use common::call_tool_raw;
use rmcp::model::{CallToolResult, Content, LoggingLevel, Meta};

#[tokio::test]
async fn test_batch_draining_with_multiple_events() {
    use serde_json::json;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<LogEvent>();

    for i in 0..5 {
        let log_event = LogEvent {
            level: LoggingLevel::Info,
            logger: format!("logger_{i}"),
            data: json!({"index": i}),
        };
        let _ = event_tx.send(log_event);
    }

    let mut buffer = Vec::with_capacity(64);
    event_rx.recv_many(&mut buffer, 64).await;

    assert_eq!(buffer.len(), 5);
    for (i, event) in buffer.iter().enumerate() {
        assert_eq!(event.logger, format!("logger_{i}"));
        assert_eq!(event.data, json!({"index": i}));
    }
}

#[test]
fn test_call_tool_result_cache_hint_metadata() {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "cache_hint".to_string(),
        serde_json::Value::String("no-cache".to_string()),
    );

    let result =
        CallToolResult::success(vec![Content::text("test output")]).with_meta(Some(Meta(meta)));

    let json_val = serde_json::to_value(&result).expect("should serialize");

    assert_eq!(
        json_val
            .get("_meta")
            .and_then(|m| m.get("cache_hint"))
            .and_then(|v| v.as_str()),
        Some("no-cache"),
        "Expected _meta.cache_hint to be 'no-cache' in serialized JSON: {json_val}"
    );
}

#[tokio::test]
async fn test_analyze_directory_bounded_traversal_skips_deep() {
    use tempfile::TempDir;

    // Arrange: three-level directory tree within CWD so validate_path accepts it.
    let cwd = std::env::current_dir().unwrap();
    let dir = TempDir::new_in(&cwd).unwrap();
    let root = dir.path();
    // depth 1
    std::fs::create_dir(root.join("a")).unwrap();
    std::fs::write(root.join("a/file1.rs"), "fn a1() {}").unwrap();
    // depth 2
    std::fs::create_dir(root.join("a/b")).unwrap();
    std::fs::write(root.join("a/b/file2.rs"), "fn b1() {}").unwrap();
    // depth 3 -- must be omitted
    std::fs::create_dir(root.join("a/b/c")).unwrap();
    std::fs::write(root.join("a/b/c/deep.rs"), "fn deep() {}").unwrap();

    // Act: analyze_directory with max_depth=2
    let resp = call_tool_raw(
        "analyze_directory",
        serde_json::json!({
            "path": root.to_str().unwrap(),
            "max_depth": 2,
            "page_size": 100
        }),
    )
    .await;

    // Assert: no error
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success; got: {resp}"
    );

    // The text output must not mention the depth-3 file
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        !text.contains("deep.rs"),
        "depth-3 file 'deep.rs' must not appear in max_depth=2 output; got: {text}"
    );

    // The text must mention the depth-1 and depth-2 files
    assert!(
        text.contains("file1.rs") || text.contains("file2.rs"),
        "shallow files must appear in max_depth=2 output; got: {text}"
    );
}

#[tokio::test]
async fn test_path_outside_cwd_rejected() {
    // Arrange: path=/etc/passwd is outside the server's CWD
    let resp = call_tool_raw(
        "analyze_directory",
        serde_json::json!({
            "path": "/etc/passwd",
            "max_depth": 0,
            "page_size": 10
        }),
    )
    .await;

    // Assert: handler must reject with isError=true and mention 'outside'
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError=true for path outside CWD: {resp}"
    );
    let content_text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        content_text.contains("outside"),
        "error message should contain 'outside': {content_text}"
    );
}

#[tokio::test]
async fn test_analyze_module_moduleonly_cache_tier_metrics() {
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // Arrange: a temp Rust file inside CWD so validate_path accepts it
    let cwd = std::env::current_dir().unwrap();
    let mut f = NamedTempFile::with_suffix_in(".rs", &cwd).unwrap();
    writeln!(f, "fn hello() {{}}").unwrap();

    // Act: first call -- cache miss (L2 disk cache is empty for a fresh unique file)
    let resp1 = call_tool_raw(
        "analyze_module",
        serde_json::json!({ "path": f.path().to_str().unwrap() }),
    )
    .await;

    // Assert first call succeeds and returns the function name
    assert!(
        !resp1["result"]["isError"].as_bool().unwrap_or(false),
        "first analyze_module call must succeed; got: {resp1}"
    );
    let text1 = resp1["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text1.contains("hello"),
        "first call output must contain function 'hello'; got: {text1}"
    );

    // Wait for the write-behind L2 task to flush the cache entry to disk.
    // Poll for the expected cache file rather than sleeping a fixed duration to avoid flakiness.
    {
        let file_bytes = std::fs::read(f.path()).expect("temp file must be readable");
        let hash = blake3::hash(&file_bytes);
        let hex = hash.to_hex();
        let cache_dir = std::env::var("APTU_CODER_DISK_CACHE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let xdg = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
                    std::env::var("HOME")
                        .map(|h| format!("{h}/.local/share"))
                        .unwrap_or_else(|_| ".".to_string())
                });
                std::path::PathBuf::from(xdg)
                    .join("aptu-coder")
                    .join("analysis-cache")
            });
        let entry = cache_dir
            .join("analyze_module")
            .join(&hex[..2])
            .join(format!("{}.json.snap", hex.as_str()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if entry.exists() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "L2 disk cache entry not written within 5 s; expected path: {}",
                entry.display()
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    }

    // Act: second call on same file -- content hash unchanged so L2 disk cache should hit
    let resp2 = call_tool_raw(
        "analyze_module",
        serde_json::json!({ "path": f.path().to_str().unwrap() }),
    )
    .await;

    // Assert second call succeeds with consistent output
    assert!(
        !resp2["result"]["isError"].as_bool().unwrap_or(false),
        "second analyze_module call must succeed; got: {resp2}"
    );
    let text2 = resp2["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text2.contains("hello"),
        "second call output must contain function 'hello'; got: {text2}"
    );
}

#[tokio::test]
async fn test_fields_functions_only_structured() {
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // Arrange: temp Rust file with functions, a class, and an import
    let cwd = std::env::current_dir().unwrap();
    let mut f = NamedTempFile::with_suffix_in(".rs", &cwd).unwrap();
    writeln!(
        f,
        "use std::collections::HashMap;\npub struct Foo {{}}\nimpl Foo {{\n    pub fn bar(&self) {{}}\n}}\npub fn baz() {{}}\n"
    )
    .unwrap();

    // Act: analyze_file with fields=[functions]
    let resp = call_tool_raw(
        "analyze_file",
        serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "ast_recursion_limit": null,
            "page_size": null,
            "fields": ["functions"]
        }),
    )
    .await;

    // Assert: no error
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success; got: {resp}"
    );

    // Inspect structuredContent.semantic
    let sc = &resp["result"]["structuredContent"];
    let functions = sc["semantic"]["functions"]
        .as_array()
        .expect("functions must be array");
    let classes = sc["semantic"]["classes"]
        .as_array()
        .expect("classes must be array");
    let imports = sc["semantic"]["imports"]
        .as_array()
        .expect("imports must be array");

    assert!(
        !functions.is_empty(),
        "functions must be non-empty for fields=[functions]; got: {sc}"
    );
    assert!(
        classes.is_empty(),
        "classes must be empty for fields=[functions]; got: {sc}"
    );
    assert!(
        imports.is_empty(),
        "imports must be empty for fields=[functions]; got: {sc}"
    );
}

#[tokio::test]
async fn test_fields_classes_only_structured() {
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // Arrange: temp Rust file with functions, a class, and an import
    let cwd = std::env::current_dir().unwrap();
    let mut f = NamedTempFile::with_suffix_in(".rs", &cwd).unwrap();
    writeln!(
        f,
        "use std::collections::HashMap;\npub struct Foo {{}}\nimpl Foo {{\n    pub fn bar(&self) {{}}\n}}\npub fn baz() {{}}\n"
    )
    .unwrap();

    // Act: analyze_file with fields=[classes]
    let resp = call_tool_raw(
        "analyze_file",
        serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "ast_recursion_limit": null,
            "page_size": null,
            "fields": ["classes"]
        }),
    )
    .await;

    // Assert: no error
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success; got: {resp}"
    );

    // Inspect structuredContent.semantic
    let sc = &resp["result"]["structuredContent"];
    let functions = sc["semantic"]["functions"]
        .as_array()
        .expect("functions must be array");
    let classes = sc["semantic"]["classes"]
        .as_array()
        .expect("classes must be array");
    let imports = sc["semantic"]["imports"]
        .as_array()
        .expect("imports must be array");

    assert!(
        functions.is_empty(),
        "functions must be empty for fields=[classes]; got: {sc}"
    );
    assert!(
        !classes.is_empty(),
        "classes must be non-empty for fields=[classes]; got: {sc}"
    );
    assert!(
        imports.is_empty(),
        "imports must be empty for fields=[classes]; got: {sc}"
    );
}

#[tokio::test]
async fn test_fields_imports_only_structured() {
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // Arrange: temp Rust file with functions, a class, and an import
    let cwd = std::env::current_dir().unwrap();
    let mut f = NamedTempFile::with_suffix_in(".rs", &cwd).unwrap();
    writeln!(
        f,
        "use std::collections::HashMap;\npub struct Foo {{}}\nimpl Foo {{\n    pub fn bar(&self) {{}}\n}}\npub fn baz() {{}}\n"
    )
    .unwrap();

    // Act: analyze_file with fields=[imports]
    let resp = call_tool_raw(
        "analyze_file",
        serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "ast_recursion_limit": null,
            "page_size": null,
            "fields": ["imports"]
        }),
    )
    .await;

    // Assert: no error
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success; got: {resp}"
    );

    // Inspect structuredContent.semantic
    let sc = &resp["result"]["structuredContent"];
    let functions = sc["semantic"]["functions"]
        .as_array()
        .expect("functions must be array");
    let classes = sc["semantic"]["classes"]
        .as_array()
        .expect("classes must be array");
    let imports = sc["semantic"]["imports"]
        .as_array()
        .expect("imports must be array");

    assert!(
        functions.is_empty(),
        "functions must be empty for fields=[imports]; got: {sc}"
    );
    assert!(
        classes.is_empty(),
        "classes must be empty for fields=[imports]; got: {sc}"
    );
    assert!(
        !imports.is_empty(),
        "imports must be non-empty for fields=[imports]; got: {sc}"
    );
}

#[tokio::test]
async fn test_fields_none_structured_full() {
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // Arrange: temp Rust file with functions, a class, and an import
    let cwd = std::env::current_dir().unwrap();
    let mut f = NamedTempFile::with_suffix_in(".rs", &cwd).unwrap();
    writeln!(
        f,
        "use std::collections::HashMap;\npub struct Foo {{}}\nimpl Foo {{\n    pub fn bar(&self) {{}}\n}}\npub fn baz() {{}}\n"
    )
    .unwrap();

    // Act: analyze_file with fields=None (no projection)
    let resp = call_tool_raw(
        "analyze_file",
        serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "ast_recursion_limit": null,
            "page_size": null
        }),
    )
    .await;

    // Assert: no error
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success; got: {resp}"
    );

    // Inspect structuredContent.semantic -- all sections must be present (regression)
    let sc = &resp["result"]["structuredContent"];
    let functions = sc["semantic"]["functions"]
        .as_array()
        .expect("functions must be array");
    let classes = sc["semantic"]["classes"]
        .as_array()
        .expect("classes must be array");
    let imports = sc["semantic"]["imports"]
        .as_array()
        .expect("imports must be array");

    assert!(
        !functions.is_empty(),
        "functions must be non-empty when fields=None; got: {sc}"
    );
    assert!(
        !classes.is_empty(),
        "classes must be non-empty when fields=None; got: {sc}"
    );
    assert!(
        !imports.is_empty(),
        "imports must be non-empty when fields=None; got: {sc}"
    );
}
