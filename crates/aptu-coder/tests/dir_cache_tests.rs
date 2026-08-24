// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! Regression tests for `analyze_directory` cache key scoping.
//!
//! Verifies that the L1 cache key is built from only the git_ref-filtered entries,
//! not the full walk result. Out-of-scope file mtime changes must not bust the cache.

mod common;

use common::make_test_analyzer;
use filetime::{FileTime, set_file_mtime};
use rmcp::serve_server;
use serde_json::Value;
use std::process::Command;

/// Sets a deterministic future mtime on `path` to avoid `sleep` in tests.
fn bump_mtime(path: &std::path::Path) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("valid unix epoch")
        .as_secs() as i64;
    set_file_mtime(path, FileTime::from_unix_time(now + 3600, 0)).expect("set future mtime");
}
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Shared MCP connection state for sequential calls on the same analyzer.
struct SequentialMcp {
    client_tx: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    reader: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    next_id: u64,
    _server_handle: tokio::task::JoinHandle<()>,
}

impl SequentialMcp {
    /// Create a new MCP connection with a fresh analyzer.
    async fn new() -> Self {
        let analyzer = make_test_analyzer();
        let (client, server) = tokio::io::duplex(65536);

        let server_handle = tokio::spawn(async move {
            let (server_rx, server_tx) = tokio::io::split(server);
            if let Ok(service) = serve_server(analyzer, (server_rx, server_tx)).await {
                let _ = service.waiting().await;
            }
        });

        let (client_rx, mut client_tx) = tokio::io::split(client);
        let mut reader = BufReader::new(client_rx).lines();

        // Initialize (id=1)
        let init = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": rmcp::model::ProtocolVersion::LATEST.as_str(),
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
            }
        })
        .to_string()
            + "\n";
        client_tx.write_all(init.as_bytes()).await.unwrap();
        client_tx.flush().await.unwrap();
        reader.next_line().await.unwrap().unwrap();

        // notifications/initialized
        let notif = serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized", "params": {}
        })
        .to_string()
            + "\n";
        client_tx.write_all(notif.as_bytes()).await.unwrap();
        client_tx.flush().await.unwrap();

        Self {
            client_tx,
            reader,
            next_id: 2,
            _server_handle: server_handle,
        }
    }

    /// Send a `tools/call` request and wait for the response.
    async fn call(&mut self, tool_name: &str, params: &serde_json::Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;

        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool_name, "arguments": params}
        })
        .to_string()
            + "\n";
        self.client_tx.write_all(msg.as_bytes()).await.unwrap();
        self.client_tx.flush().await.unwrap();

        loop {
            let line = self.reader.next_line().await.unwrap().unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return v;
            }
        }
    }
}

/// Return the `cache_tier` string from a successful analyze_directory response's structuredContent.
fn extract_cache_tier(resp: &Value) -> Option<String> {
    resp["result"]["structuredContent"]
        .get("cache_tier")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

fn is_success(resp: &Value) -> bool {
    !resp["result"]["isError"].as_bool().unwrap_or(false)
}

/// Initialize a git repo in the given directory, create two source files, and commit them.
/// Returns the paths of the two files.
/// Panics if any git command fails (with stderr in the panic message).
fn setup_git_repo(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    fn run_git(args: &[&str], cwd: &std::path::Path, label: &str) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("{label} failed to spawn: {e}"));
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("{label} failed: {stderr}");
        }
    }

    run_git(&["init"], root, "git init");
    run_git(
        &["config", "user.email", "test@example.com"],
        root,
        "git config user.email",
    );
    run_git(
        &["config", "user.name", "Test User"],
        root,
        "git config user.name",
    );

    let lib_path = root.join("lib.rs");
    let utils_path = root.join("utils.rs");

    std::fs::write(&lib_path, "fn hello() {}\n").expect("write lib.rs");
    std::fs::write(&utils_path, "fn helper() {}\n").expect("write utils.rs");

    run_git(&["add", "."], root, "git add");
    run_git(
        &["commit", "--no-verify", "-m", "initial"],
        root,
        "git commit",
    );

    (lib_path, utils_path)
}

#[tokio::test]
async fn test_dir_cache_out_of_scope_file_does_not_bust() {
    // Arrange: temp git repo inside CWD so validate_path accepts it.
    let cwd = std::env::current_dir().expect("must have cwd");
    let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
    let (lib_path, utils_path) = setup_git_repo(dir.path());

    // Modify lib.rs to create a diff against HEAD (this file is in-scope for git_ref).
    std::fs::write(&lib_path, "fn hello() {}\nfn world() {}\n").expect("modify lib.rs");

    let params = serde_json::json!({
        "path": dir.path().to_str().unwrap(),
        "git_ref": "HEAD",
        "max_depth": 0,
        "page_size": 100
    });

    let mut mcp = SequentialMcp::new().await;

    // Call 1: cache miss (populates cache).
    let resp1 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp1), "call 1 must succeed; got: {resp1}");
    let tier1 = extract_cache_tier(&resp1);
    assert!(
        matches!(
            tier1.as_deref(),
            Some("miss") | Some("l1_only_miss") | Some("l1_l2_miss")
        ),
        "call 1 must be a cache miss; got: {tier1:?}"
    );

    // Call 2: L1 cache hit (no file changes since call 1).
    let resp2 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp2), "call 2 must succeed; got: {resp2}");
    let tier2 = extract_cache_tier(&resp2);
    assert_eq!(
        tier2.as_deref(),
        Some("l1_memory"),
        "call 2 must be an L1 cache hit; got: {tier2:?}"
    );

    // Touch the out-of-scope file (utils.rs is NOT in the git_ref diff).
    // Set a deterministic future mtime without sleeping.
    std::fs::write(&utils_path, "fn helper() {}\n").expect("touch utils.rs");
    bump_mtime(&utils_path);

    // Call 3: should STILL be an L1 cache hit (the fix).
    // Out-of-scope file mtime change must not bust the cache.
    let resp3 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp3), "call 3 must succeed; got: {resp3}");
    let tier3 = extract_cache_tier(&resp3);
    assert_eq!(
        tier3.as_deref(),
        Some("l1_memory"),
        "call 3 must be an L1 cache hit after touching out-of-scope file; got: {tier3:?}"
    );
}

#[tokio::test]
async fn test_dir_cache_in_scope_file_change_still_invalidates() {
    // Arrange: temp git repo inside CWD.
    let cwd = std::env::current_dir().expect("must have cwd");
    let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
    let (lib_path, _utils_path) = setup_git_repo(dir.path());

    // Modify lib.rs to create a diff against HEAD (in-scope for git_ref).
    std::fs::write(&lib_path, "fn hello() {}\nfn world() {}\n").expect("modify lib.rs");

    let params = serde_json::json!({
        "path": dir.path().to_str().unwrap(),
        "git_ref": "HEAD",
        "max_depth": 0,
        "page_size": 100
    });

    let mut mcp = SequentialMcp::new().await;

    // Call 1: cache miss.
    let resp1 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp1), "call 1 must succeed; got: {resp1}");
    assert!(
        matches!(
            extract_cache_tier(&resp1).as_deref(),
            Some("miss") | Some("l1_only_miss") | Some("l1_l2_miss")
        ),
        "call 1 must be a cache miss"
    );

    // Call 2: L1 cache hit.
    let resp2 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp2), "call 2 must succeed; got: {resp2}");
    assert_eq!(
        extract_cache_tier(&resp2).as_deref(),
        Some("l1_memory"),
        "call 2 must be an L1 cache hit"
    );

    // Modify the in-scope file (lib.rs is in the git_ref diff).
    std::fs::write(&lib_path, "fn hello() {}\nfn world() {}\nfn extra() {}\n")
        .expect("modify lib.rs again");
    bump_mtime(&lib_path);

    // Call 3: cache miss (in-scope file changed, cache must invalidate).
    let resp3 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp3), "call 3 must succeed; got: {resp3}");
    let tier3 = extract_cache_tier(&resp3);
    assert!(
        matches!(
            tier3.as_deref(),
            Some("miss") | Some("l1_only_miss") | Some("l1_l2_miss")
        ),
        "call 3 must be a cache miss after in-scope file change; got: {tier3:?}"
    );

    // Call 4: L1 cache hit (cache repopulated by call 3).
    let resp4 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp4), "call 4 must succeed; got: {resp4}");
    assert_eq!(
        extract_cache_tier(&resp4).as_deref(),
        Some("l1_memory"),
        "call 4 must be an L1 cache hit after repopulation; got: {:?}",
        extract_cache_tier(&resp4)
    );
}

#[tokio::test]
async fn test_dir_cache_out_of_scope_depth_file_does_not_bust() {
    // Arrange: temp dir with in-scope file at depth 1 and out-of-scope file at depth 4.
    let cwd = std::env::current_dir().expect("must have cwd");
    let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
    let root = dir.path();

    // Create in-scope file at depth 1: dir/a.rs
    let in_scope_path = root.join("a.rs");
    std::fs::write(&in_scope_path, "fn alpha() {}\n").expect("write a.rs");

    // Create out-of-scope file at depth 4: dir/sub1/sub2/sub3/deep.rs
    let deep_dir = root.join("sub1/sub2/sub3");
    std::fs::create_dir_all(&deep_dir).expect("create deep dirs");
    let out_of_scope_path = deep_dir.join("deep.rs");
    std::fs::write(&out_of_scope_path, "fn deeper() {}\n").expect("write deep.rs");

    let params = serde_json::json!({
        "path": root.to_str().unwrap(),
        "max_depth": 2,
        "page_size": 100
    });

    let mut mcp = SequentialMcp::new().await;

    // Call 1: cache miss (populates cache).
    let resp1 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp1), "call 1 must succeed; got: {resp1}");
    let tier1 = extract_cache_tier(&resp1);
    assert!(
        matches!(
            tier1.as_deref(),
            Some("miss") | Some("l1_only_miss") | Some("l1_l2_miss")
        ),
        "call 1 must be a cache miss; got: {tier1:?}"
    );

    // Call 2: L1 cache hit (no file changes since call 1).
    let resp2 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp2), "call 2 must succeed; got: {resp2}");
    let tier2 = extract_cache_tier(&resp2);
    assert_eq!(
        tier2.as_deref(),
        Some("l1_memory"),
        "call 2 must be an L1 cache hit; got: {tier2:?}"
    );

    // Touch the out-of-scope file (depth 4 is beyond max_depth=2).
    // Set a deterministic future mtime without sleeping.
    std::fs::write(&out_of_scope_path, "fn deeper() {}\n").expect("touch deep.rs");
    bump_mtime(&out_of_scope_path);

    // Call 3: should STILL be an L1 cache hit (the fix).
    // Out-of-scope depth file mtime change must not bust the cache.
    let resp3 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp3), "call 3 must succeed; got: {resp3}");
    let tier3 = extract_cache_tier(&resp3);
    assert_eq!(
        tier3.as_deref(),
        Some("l1_memory"),
        "call 3 must be an L1 cache hit after touching out-of-scope depth file; got: {tier3:?}"
    );
}

#[tokio::test]
async fn test_dir_cache_in_scope_depth_file_change_still_invalidates() {
    // Arrange: temp dir with in-scope file at depth 1 and out-of-scope file at depth 4.
    let cwd = std::env::current_dir().expect("must have cwd");
    let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
    let root = dir.path();

    // Create in-scope file at depth 1: dir/a.rs
    let in_scope_path = root.join("a.rs");
    std::fs::write(&in_scope_path, "fn alpha() {}\n").expect("write a.rs");

    // Create out-of-scope file at depth 4: dir/sub1/sub2/sub3/deep.rs
    let deep_dir = root.join("sub1/sub2/sub3");
    std::fs::create_dir_all(&deep_dir).expect("create deep dirs");
    let out_of_scope_path = deep_dir.join("deep.rs");
    std::fs::write(&out_of_scope_path, "fn deeper() {}\n").expect("write deep.rs");

    let params = serde_json::json!({
        "path": root.to_str().unwrap(),
        "max_depth": 2,
        "page_size": 100
    });

    let mut mcp = SequentialMcp::new().await;

    // Call 1: cache miss.
    let resp1 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp1), "call 1 must succeed; got: {resp1}");
    assert!(
        matches!(
            extract_cache_tier(&resp1).as_deref(),
            Some("miss") | Some("l1_only_miss") | Some("l1_l2_miss")
        ),
        "call 1 must be a cache miss"
    );

    // Call 2: L1 cache hit.
    let resp2 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp2), "call 2 must succeed; got: {resp2}");
    assert_eq!(
        extract_cache_tier(&resp2).as_deref(),
        Some("l1_memory"),
        "call 2 must be an L1 cache hit"
    );

    // Modify the in-scope file (depth 1 is within max_depth=2).
    std::fs::write(&in_scope_path, "fn alpha() {}\nfn beta() {}\n").expect("modify a.rs");
    bump_mtime(&in_scope_path);

    // Call 3: cache miss (in-scope file changed, cache must invalidate).
    let resp3 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp3), "call 3 must succeed; got: {resp3}");
    let tier3 = extract_cache_tier(&resp3);
    assert!(
        matches!(
            tier3.as_deref(),
            Some("miss") | Some("l1_only_miss") | Some("l1_l2_miss")
        ),
        "call 3 must be a cache miss after in-scope depth file change; got: {tier3:?}"
    );

    // Call 4: L1 cache hit (cache repopulated by call 3).
    let resp4 = mcp.call("analyze_directory", &params).await;
    assert!(is_success(&resp4), "call 4 must succeed; got: {resp4}");
    assert_eq!(
        extract_cache_tier(&resp4).as_deref(),
        Some("l1_memory"),
        "call 4 must be an L1 cache hit after repopulation; got: {:?}",
        extract_cache_tier(&resp4)
    );
}
