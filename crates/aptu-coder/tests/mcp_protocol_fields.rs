// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for MCP 2026-07-28 protocol field adoption (issue #998).
//!
//! C05: `resultType` discriminator on `CallToolResult`.
//!   rmcp 3.1.2 constructors (`success`, `error`) already set
//!   `result_type: Some(ResultType::COMPLETE)`. The server handler strips the
//!   field for peers negotiating a protocol version older than `2026-07-28`,
//!   so we verify at the struct level (like existing tests in
//!   `integration_tests.rs`).
//!
//! C07: `ttlMs` / `cacheScope` on `ListToolsResult`.
//!   The tool list is static; a generous TTL with `CacheScope::Public` reduces
//!   redundant `tools/list` round-trips for long-running agent sessions.
//!   These fields are NOT stripped for legacy peers, so we verify on the wire.

mod common;

use common::{make_test_analyzer, send_raw_request};
use rmcp::model::{CallToolResult, ContentBlock, ResultType};

// ---------------------------------------------------------------------------
// C05: resultType on CallToolResult
// ---------------------------------------------------------------------------

/// rmcp 3.1.2 constructors set `result_type: Some(ResultType::COMPLETE)`.
/// The server handler strips the field for peers negotiating a protocol
/// version older than 2026-07-28, so we verify at the struct level (like
/// existing tests in `integration_tests.rs`).
#[test]
fn test_call_tool_result_constructors_set_result_type_complete() {
    for result in [
        CallToolResult::success(vec![ContentBlock::text("ok")]),
        CallToolResult::error(vec![ContentBlock::text("err")]),
    ] {
        assert_eq!(
            result.result_type,
            Some(ResultType::COMPLETE),
            "CallToolResult constructor must set result_type to Complete"
        );
    }
}

#[test]
fn test_call_tool_result_type_serializes_as_complete() {
    let result = CallToolResult::success(vec![ContentBlock::text("ok")]);
    let json = serde_json::to_value(&result).expect("should serialize");
    assert_eq!(
        json.get("resultType").and_then(|v| v.as_str()),
        Some("complete"),
        "resultType must serialize as \"complete\": {json}"
    );
}

// ---------------------------------------------------------------------------
// C07: ttlMs / cacheScope on tools/list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tools_list_has_ttl_and_cache_scope() {
    let analyzer = make_test_analyzer();
    let response = send_raw_request(analyzer, "tools/list", serde_json::json!({})).await;

    let result = response
        .get("result")
        .expect("tools/list must return a result");

    assert_eq!(
        result.get("ttlMs").and_then(|v| v.as_u64()),
        Some(3_600_000),
        "tools/list must advertise ttlMs=3600000: {result}"
    );
    assert_eq!(
        result.get("cacheScope").and_then(|v| v.as_str()),
        Some("public"),
        "tools/list must advertise cacheScope=public: {result}"
    );
}
