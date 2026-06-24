// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Shared helpers used by two or more tool handler modules.
//!
//! All items here are `pub(crate)`. OTel-specific types (`ClientMetadata`,
//! `extract_and_set_trace_context`) live in `crate::otel` where they are `pub`.

use rmcp::model::{CallToolResult, Content, ErrorData, Meta};

/// Returns `true` when `summary=true` and a `cursor` are both provided, which is an invalid
/// combination since summary mode and pagination are mutually exclusive.
#[must_use]
pub(crate) fn summary_cursor_conflict(summary: Option<bool>, cursor: Option<&str>) -> bool {
    summary == Some(true) && cursor.is_some()
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorMeta {
    error_category: &'static str,
    is_retryable: bool,
    suggested_action: &'static str,
}

#[must_use]
pub(crate) fn error_meta(
    category: &'static str,
    is_retryable: bool,
    suggested_action: &'static str,
) -> serde_json::Value {
    serde_json::to_value(ErrorMeta {
        error_category: category,
        is_retryable,
        suggested_action,
    })
    .unwrap_or_default()
}

#[must_use]
pub(crate) fn err_to_tool_result(e: ErrorData) -> CallToolResult {
    let mut result =
        CallToolResult::error(vec![Content::text(e.message)]).with_meta(Some(no_cache_meta()));
    if let Some(data) = e.data {
        result.structured_content = Some(data);
    }
    result
}

pub(crate) fn no_cache_meta() -> Meta {
    let mut m = serde_json::Map::new();
    m.insert(
        "cache_hint".to_string(),
        serde_json::Value::String("no-cache".to_string()),
    );
    Meta(m)
}
