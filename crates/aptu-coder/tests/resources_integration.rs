// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the MCP resource surface (`list_resources`,
//! `list_resource_templates`, `read_resource`) over a real in-process MCP server.
//!
//! All MCP dispatch goes through `common::send_raw_request`, which runs the
//! full initialize/initialized handshake and races the response against the
//! server task.  No handshake boilerplate lives in this file.

mod common;

use common::{call_tool_raw, make_test_analyzer, send_raw_request};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// 64-char hex string used for cold-miss URIs.  `parse_graph_uri` does not
/// validate the hash format, so an all-zero hash is a well-formed key that
/// never matches a real shard.
const DUMMY_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Process-global graph cache directory.
///
/// `APTU_CODER_DISK_CACHE_DIR` must be set before any `CodeAnalyzer` is
/// constructed.  Using `OnceLock` + a leaked `TempDir` satisfies both the
/// single-initialization and lifetime requirements for the whole test binary.
fn graph_cache_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let cwd = std::env::current_dir().expect("must have cwd");
        let dir = tempfile::TempDir::new_in(&cwd).expect("cache tempdir");
        let path = dir.path().to_path_buf();
        // SAFETY: called once via OnceLock before any analyzer is constructed;
        // no concurrent reader of APTU_CODER_DISK_CACHE_DIR exists at this point.
        unsafe {
            std::env::set_var("APTU_CODER_DISK_CACHE_DIR", &path);
        }
        std::mem::forget(dir); // keep the directory alive for the whole binary
        path
    })
}

/// Poll until `graph_cache_dir()` contains at least one `.bin` shard, then
/// return the full path to the first one found.
///
/// The production code writes shards asynchronously after `analyze_symbol`
/// returns, so we poll rather than assume the file exists immediately.
async fn wait_for_any_shard(base: &Path) -> PathBuf {
    let deadline = Duration::from_secs(10);
    let start = std::time::Instant::now();
    loop {
        // Shards live at <base>/<key[..2]>/<key>.bin.
        if let Ok(rd) = std::fs::read_dir(base) {
            for subdir in rd.flatten() {
                if subdir.file_type().is_ok_and(|t| t.is_dir())
                    && let Ok(rd2) = std::fs::read_dir(subdir.path())
                    && let Some(entry) = rd2
                        .flatten()
                        .find(|e| e.path().extension().is_some_and(|x| x == "bin"))
                {
                    return entry.path();
                }
            }
        }
        assert!(
            start.elapsed() < deadline,
            "no graph shard appeared in {base:?} within 10 s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Extract the cache key (64-char hex string) from a shard path of the form
/// `<base>/<key[..2]>/<key>.bin`.
fn key_from_shard(shard: &Path) -> String {
    shard
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("shard path has a UTF-8 stem")
        .to_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_list_resources_cold_cache() {
    graph_cache_dir();
    let analyzer = make_test_analyzer();
    let resp = send_raw_request(analyzer, "resources/list", serde_json::json!({})).await;

    assert!(
        resp.get("error").is_none(),
        "list_resources must not error; got: {resp}"
    );
    assert!(
        resp["result"]["resources"].is_array(),
        "result.resources must be an array; got: {resp}"
    );
}

#[tokio::test]
async fn test_list_resource_templates() {
    graph_cache_dir();
    let analyzer = make_test_analyzer();
    let resp = send_raw_request(analyzer, "resources/templates/list", serde_json::json!({})).await;

    assert!(
        resp.get("error").is_none(),
        "list_resource_templates must not error; got: {resp}"
    );
    let templates = resp["result"]["resourceTemplates"]
        .as_array()
        .expect("result.resourceTemplates must be an array");
    assert!(
        templates.iter().any(|t| t["uriTemplate"]
            .as_str()
            .is_some_and(|s| s.starts_with("aptu-coder://"))),
        "at least one template must start with aptu-coder://; got: {resp}"
    );
}

#[tokio::test]
async fn test_read_resource_cold_miss() {
    graph_cache_dir();
    let analyzer = make_test_analyzer();
    let uri = format!("aptu-coder://graph/{DUMMY_HASH}/blast-radius/main");
    let resp = send_raw_request(analyzer, "resources/read", serde_json::json!({"uri": uri})).await;

    let msg = resp
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .or_else(|| resp["result"]["contents"][0]["text"].as_str())
        .unwrap_or_default();
    assert!(
        msg.contains("graph not built yet") || msg.contains("analyze_symbol"),
        "cold miss should mention building the graph; got: {resp}"
    );
}

#[tokio::test]
async fn test_read_resource_warm_cache() {
    let cache_dir = graph_cache_dir();
    let cwd = std::env::current_dir().expect("must have cwd");

    // Arrange: a small Rust fixture inside CWD (validate_path confinement).
    let fixture = tempfile::TempDir::new_in(&cwd).expect("fixture tempdir");
    std::fs::write(
        fixture.path().join("lib.rs"),
        "fn inner() {}\n\nfn outer() {\n    inner();\n}\n",
    )
    .expect("write fixture");

    // Act: warm the graph cache via analyze_symbol.
    let _warm = call_tool_raw(
        "analyze_symbol",
        serde_json::json!({
            "path": fixture.path().to_str().expect("fixture path is UTF-8"),
            "symbol": "",
            "follow_depth": 1,
            "max_depth": 2,
        }),
    )
    .await;

    // Wait for the shard the production code writes asynchronously, then read
    // the cache key directly from the file name -- no coupling to
    // compute_cache_key internals.
    let shard = wait_for_any_shard(cache_dir).await;
    let repo_hash = key_from_shard(&shard);

    // Read the resource for the warmed hash.
    let uri = format!("aptu-coder://graph/{repo_hash}/blast-radius/main");
    let analyzer = make_test_analyzer();
    let resp = send_raw_request(analyzer, "resources/read", serde_json::json!({"uri": uri})).await;

    assert!(
        resp.get("error").is_none(),
        "warm read must not error; got: {resp}"
    );
    let text = resp["result"]["contents"][0]["text"]
        .as_str()
        .expect("contents[0].text must be a string");
    let payload: serde_json::Value =
        serde_json::from_str(text).expect("contents[0].text must be JSON");
    assert!(
        payload.get("nodes").is_some(),
        "payload must contain a nodes key; got: {payload}"
    );
}

#[tokio::test]
async fn test_read_resource_malformed_uri() {
    graph_cache_dir();
    let analyzer = make_test_analyzer();
    let resp = send_raw_request(
        analyzer,
        "resources/read",
        serde_json::json!({"uri": "not-a-valid-uri"}),
    )
    .await;

    assert!(
        resp["error"]["code"].is_number(),
        "malformed URI must yield a JSON-RPC error with a code; got: {resp}"
    );
}
