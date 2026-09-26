// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Shared helpers used by two or more tool handler modules.
//!
//! All items here are `pub(crate)`. OTel-specific types (`ClientMetadata`,
//! `extract_and_set_trace_context`) live in `crate::otel` where they are `pub`.

use aptu_coder_core::pagination::decode_cursor;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData, MetaObject};

/// Returns `true` when `summary=true` and a `cursor` are both provided, which is an invalid
/// combination since summary mode and pagination are mutually exclusive.
#[must_use]
pub(crate) fn summary_cursor_conflict(summary: Option<bool>, cursor: Option<&str>) -> bool {
    summary == Some(true) && cursor.is_some()
}

/// Normalizes an empty-string `cursor` to `None`, treating `cursor=""` as equivalent to an
/// omitted cursor (first page). Non-empty cursors pass through unchanged; genuine malformed
/// non-empty cursors are still rejected downstream by `decode_cursor`.
#[must_use]
pub(crate) fn normalize_cursor(cursor: Option<&str>) -> Option<&str> {
    cursor.filter(|c| !c.is_empty())
}

/// Decodes a pagination cursor into its byte offset, returning offset 0 when the cursor is
/// absent. Malformed cursors yield an `INVALID_PARAMS` error with a validation meta payload.
pub(crate) fn decode_offset(cursor: Option<&str>) -> Result<usize, ErrorData> {
    match cursor {
        Some(cursor_str) => decode_cursor(cursor_str).map(|c| c.offset).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                e.to_string(),
                Some(error_meta("validation", false, "invalid cursor format")),
            )
        }),
        None => Ok(0),
    }
}

/// Paginates a slice with the default pagination mode, mapping pagination failures to an
/// `INTERNAL_ERROR` with a transient, retryable meta payload.
pub(crate) fn paginate_or_internal_error<T: Clone>(
    items: &[T],
    offset: usize,
    page_size: usize,
) -> Result<aptu_coder_core::pagination::PaginationResult<T>, ErrorData> {
    aptu_coder_core::pagination::paginate_slice(
        items,
        offset,
        page_size,
        aptu_coder_core::pagination::PaginationMode::Default,
    )
    .map_err(|e| {
        ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            e.to_string(),
            Some(error_meta("transient", true, "retry the request")),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::normalize_cursor;

    #[test]
    fn normalize_cursor_none_stays_none() {
        // Arrange / Act
        let result = normalize_cursor(None);
        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn normalize_cursor_empty_string_becomes_none() {
        // Arrange / Act
        let result = normalize_cursor(Some(""));
        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn normalize_cursor_non_empty_passes_through() {
        // Arrange / Act
        let result = normalize_cursor(Some("abc"));
        // Assert
        assert_eq!(result, Some("abc"));
    }
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
        CallToolResult::error(vec![ContentBlock::text(e.message)]).with_meta(Some(no_cache_meta()));
    if let Some(data) = e.data {
        result.structured_content = Some(data);
    }
    result
}

pub(crate) fn no_cache_meta() -> MetaObject {
    let mut m = serde_json::Map::new();
    m.insert(
        "cache_hint".to_string(),
        serde_json::Value::String("no-cache".to_string()),
    );
    MetaObject(m)
}
