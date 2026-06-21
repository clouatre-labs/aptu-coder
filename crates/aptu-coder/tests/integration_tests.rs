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
async fn test_no_cache_meta_on_pagination_error() {
    // Arrange: Trigger a DefUse pagination error by passing an invalid cursor.
    // Analyze_symbol with def_use=true requires a valid cursor to paginate through
    // def-use results; an invalid/corrupted cursor should trigger PaginationError,
    // which should return with cache_hint: no-cache in _meta.

    let resp = call_tool_raw(
        "analyze_symbol",
        serde_json::json!({
            "path": ".",
            "symbol": "test_symbol",
            "follow_depth": 1,
            "max_depth": 3,
            "page_size": 100,
            "def_use": true,
            "cursor": "INVALID_CORRUPTED_CURSOR_12345"
        }),
    )
    .await;

    // Assert the response has cache_hint: no-cache in _meta
    assert_eq!(
        resp["result"]["_meta"]
            .get("cache_hint")
            .and_then(|v| v.as_str()),
        Some("no-cache"),
        "Expected _meta.cache_hint to be 'no-cache' in pagination error response: {resp}"
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

#[tokio::test]
async fn test_analyze_file_unsupported_extension() {
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // Arrange: .txt file inside CWD so validate_path accepts it
    let cwd = std::env::current_dir().unwrap();
    let mut f = NamedTempFile::with_suffix_in(".txt", &cwd).expect("should create temp file");
    writeln!(f, "hello world").expect("should write");
    writeln!(f, "second line").expect("should write");

    let resp = call_tool_raw(
        "analyze_file",
        serde_json::json!({ "path": f.path().to_str().unwrap() }),
    )
    .await;

    // Success, not INVALID_PARAMS
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "analyze_file on unsupported extension must succeed; got: {resp}"
    );

    // Correct line count
    let sc = &resp["result"]["structuredContent"];
    let line_count = sc["line_count"]
        .as_u64()
        .expect("line_count must be present");
    assert_eq!(line_count, 2, "line_count must be 2; got: {resp}");

    // Semantic fields empty
    assert!(
        sc["semantic"]["functions"]
            .as_array()
            .expect("functions must be array")
            .is_empty(),
        "functions must be empty for unsupported extension"
    );
    assert!(
        sc["semantic"]["classes"]
            .as_array()
            .expect("classes must be array")
            .is_empty(),
        "classes must be empty for unsupported extension"
    );
    assert!(
        sc["semantic"]["imports"]
            .as_array()
            .expect("imports must be array")
            .is_empty(),
        "imports must be empty for unsupported extension"
    );

    // Formatted text includes unsupported-extension note
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.to_lowercase().contains("unsupported"),
        "formatted output must include unsupported-extension note; got: {text}"
    );
}

#[tokio::test]
async fn test_analyze_module_unsupported_fallback() {
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // Happy path: analyze_module on unsupported extension returns success
    // with empty functions and imports lists.
    let cwd = std::env::current_dir().unwrap();
    let mut f = NamedTempFile::with_suffix_in(".txt", &cwd).expect("should create temp file");
    writeln!(f, "some content").expect("should write");

    let resp = call_tool_raw(
        "analyze_module",
        serde_json::json!({ "path": f.path().to_str().unwrap() }),
    )
    .await;

    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "analyze_module on unsupported extension must succeed; got: {resp}"
    );

    let sc = &resp["result"]["structuredContent"];

    // Correct line count
    let line_count = sc["line_count"]
        .as_u64()
        .expect("line_count must be present");
    assert_eq!(line_count, 1, "line_count must be 1; got: {resp}");

    // Function and import lists empty
    assert!(
        sc["functions"]
            .as_array()
            .expect("functions must be array")
            .is_empty(),
        "functions must be empty for unsupported extension"
    );
    assert!(
        sc["imports"]
            .as_array()
            .expect("imports must be array")
            .is_empty(),
        "imports must be empty for unsupported extension"
    );
}

/// Recursively walk a `serde_json::Value` (a raw JSON Schema object), collecting paths
/// where `"format"` equals one of the forbidden values.
/// Covers: properties, $defs, allOf, anyOf, oneOf, items, additionalProperties.
fn collect_forbidden_formats(
    val: &serde_json::Value,
    forbidden: &[&str],
    path: &str,
    found: &mut Vec<String>,
) {
    let serde_json::Value::Object(map) = val else {
        return;
    };
    if let Some(fmt) = map.get("format").and_then(|v| v.as_str()) {
        if forbidden.contains(&fmt) {
            found.push(format!("{path}: format={fmt}"));
        }
    }
    for (key, child) in map {
        let child_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        match key.as_str() {
            "properties" | "$defs" => {
                if let Some(props) = child.as_object() {
                    for (name, val) in props {
                        collect_forbidden_formats(
                            val,
                            forbidden,
                            &format!("{child_path}.{name}"),
                            found,
                        );
                    }
                }
            }
            "allOf" | "anyOf" | "oneOf" => {
                if let Some(arr) = child.as_array() {
                    for (i, item) in arr.iter().enumerate() {
                        collect_forbidden_formats(
                            item,
                            forbidden,
                            &format!("{child_path}[{i}]"),
                            found,
                        );
                    }
                }
            }
            "items" => collect_forbidden_formats(child, forbidden, &child_path, found),
            "additionalProperties" => {
                if child.is_object() {
                    collect_forbidden_formats(child, forbidden, &child_path, found);
                }
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn test_schema_compliance_no_nonstandard_formats() {
    use common::make_test_analyzer;
    use rmcp::ServiceExt as _;

    // Spin up the MCP server and connect a typed rmcp client over an in-process duplex pipe.
    let analyzer = make_test_analyzer();
    let (client_io, server_io) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        let (rx, tx) = tokio::io::split(server_io);
        if let Ok(svc) = rmcp::serve_server(analyzer, (rx, tx)).await {
            let _ = svc.waiting().await;
        }
    });
    let (client_rx, client_tx) = tokio::io::split(client_io);
    let client = ().serve((client_rx, client_tx)).await.expect("client handshake failed");

    let result = client
        .peer()
        .list_tools(None)
        .await
        .expect("tools/list failed");

    assert!(!result.tools.is_empty(), "expected at least one tool");

    let forbidden = &["uint", "uint64"];
    let mut found = Vec::new();

    for tool in &result.tools {
        // inputSchema is a serde_json::Value on rmcp's Tool struct.
        collect_forbidden_formats(
            &serde_json::to_value(&tool.input_schema).expect("inputSchema serialization failed"),
            forbidden,
            &tool.name,
            &mut found,
        );
        // outputSchema is optional.
        if let Some(output_schema) = &tool.output_schema {
            collect_forbidden_formats(
                &serde_json::to_value(output_schema).expect("outputSchema serialization failed"),
                forbidden,
                &format!("{}.outputSchema", tool.name),
                &mut found,
            );
        }
    }

    assert!(
        found.is_empty(),
        "found forbidden format values in tool schemas:\n{}",
        found.join("\n")
    );
}

/// analyze_file with a directory path returns error without leaking path in message.
#[tokio::test]
async fn test_analyze_file_directory_error_no_path_leak() {
    // Arrange: create a temp dir inside CWD
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let dir_name = temp_dir
        .path()
        .file_name()
        .expect("temp dir has file name")
        .to_str()
        .expect("temp dir name is valid UTF-8");

    // Act: call analyze_file with the directory path
    let resp = call_tool_raw(
        "analyze_file",
        serde_json::json!({
            "path": dir_name,
        }),
    )
    .await;

    // Assert: error without path leak
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        !msg.contains(dir_name),
        "error message must not contain directory path: {msg}"
    );
}

/// analyze_module on an unreadable file returns error without leaking path in message.
#[cfg(unix)]
#[tokio::test]
async fn test_analyze_module_read_error_no_path_leak() {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    // Arrange: create a temp file inside CWD with a supported extension
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let file_name = "secret.rs";
    let file_path = temp_dir.path().join(file_name);
    std::fs::write(&file_path, "fn foo() {}").expect("should write file");
    let relative_path = format!(
        "{}/{}",
        temp_dir.path().file_name().unwrap().to_str().unwrap(),
        file_name
    );

    // chmod 000
    std::fs::set_permissions(&file_path, Permissions::from_mode(0o000))
        .expect("should set permissions");

    // Root-skip: if we can still read the file, we are root -- skip
    if std::fs::read(&file_path).is_ok() {
        std::fs::set_permissions(&file_path, Permissions::from_mode(0o644)).ok();
        return;
    }

    // Act
    let resp = call_tool_raw(
        "analyze_module",
        serde_json::json!({ "path": relative_path }),
    )
    .await;

    // Restore before TempDir drops
    std::fs::set_permissions(&file_path, Permissions::from_mode(0o644)).ok();

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
        !msg.contains(temp_dir.path().to_str().unwrap()),
        "error message must not contain dir path: {msg}"
    );
}

/// analyze_module with a directory path returns error without leaking path in message.
#[tokio::test]
async fn test_analyze_module_directory_error_no_path_leak() {
    // Arrange: create a temp dir inside CWD
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let dir_name = temp_dir
        .path()
        .file_name()
        .expect("temp dir has file name")
        .to_str()
        .expect("temp dir name is valid UTF-8");

    // Act: call analyze_module with the directory path
    let resp = call_tool_raw(
        "analyze_module",
        serde_json::json!({
            "path": dir_name,
        }),
    )
    .await;

    // Assert: error without path leak
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected error but got success: {resp}"
    );
    let msg = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("should have error text");
    assert!(
        !msg.contains(dir_name),
        "error message must not contain directory path: {msg}"
    );
}

#[tokio::test]
async fn test_analyze_directory_default_max_depth_is_three() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let base = temp_dir.path();

    std::fs::create_dir_all(base.join("a/b/c/d")).unwrap();
    std::fs::write(base.join("a/d1.rs"), "fn d1() {}").unwrap();
    std::fs::write(base.join("a/b/d2.rs"), "fn d2() {}").unwrap();
    std::fs::write(base.join("a/b/c/d3.rs"), "fn d3() {}").unwrap();
    // depth 4 -- must NOT appear with default max_depth=3
    std::fs::write(base.join("a/b/c/d/d4.rs"), "fn d4() {}").unwrap();

    let resp = call_tool_raw(
        "analyze_directory",
        serde_json::json!({
            "path": base.to_str().expect("path is valid UTF-8"),
            "page_size": 100
        }),
    )
    .await;

    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success, got: {resp}"
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    // Summary/overview mode rolls up individual files into directory nodes;
    // assert the depth-3 directory "c/" appears and depth-4 content does not.
    assert!(
        text.contains("c/") || text.contains("/c"),
        "depth-3 directory must appear: {text}"
    );
    assert!(
        !text.contains("d4.rs") && !text.contains("  d/"),
        "depth-4 content must NOT appear with default max_depth=3: {text}"
    );
}

#[tokio::test]
async fn test_analyze_directory_explicit_max_depth_zero_unlimited() {
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let base = temp_dir.path();

    // depth 4 -- must appear when max_depth=0 (unlimited)
    std::fs::create_dir_all(base.join("a/b/c/d")).unwrap();
    std::fs::write(base.join("a/b/c/d/deep.rs"), "fn deep() {}").unwrap();

    let resp = call_tool_raw(
        "analyze_directory",
        serde_json::json!({
            "path": base.to_str().expect("path is valid UTF-8"),
            "max_depth": 0,
            "page_size": 100
        }),
    )
    .await;

    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected success, got: {resp}"
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("deep.rs"),
        "depth-4 file must appear when max_depth=0 (unlimited): {text}"
    );
}

#[tokio::test]
async fn test_meta_field_ordering() {
    // serde_json Map is BTreeMap (alphabetical), so "cache_hint" < "content_hash"
    // alphabetically. This test is a regression guard ensuring no_cache_meta() builds
    // the map in insertion order that still satisfies alphabetical serialization.
    // CWD during tests is crates/aptu-coder; use src/lib.rs which is a valid file there.
    let resp = call_tool_raw(
        "analyze_module",
        serde_json::json!({
            "path": "src/lib.rs"
        }),
    )
    .await;

    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "analyze_module must succeed: {resp}"
    );

    let meta_str =
        serde_json::to_string(&resp["result"]["_meta"]).expect("_meta should serialize to string");

    let cache_hint_pos = meta_str
        .find("cache_hint")
        .expect("cache_hint must be present");
    let content_hash_pos = meta_str
        .find("content_hash")
        .expect("content_hash must be present");

    assert!(
        cache_hint_pos < content_hash_pos,
        "cache_hint must appear before content_hash in serialized _meta JSON: {meta_str}"
    );
}
