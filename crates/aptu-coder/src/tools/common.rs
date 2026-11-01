// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Shared helpers used by two or more tool handler modules.
//!
//! All items here are `pub(crate)`. OTel-specific types (`ClientMetadata`,
//! `extract_and_set_trace_context`) live in `crate::otel` where they are `pub`.

use aptu_coder_core::pagination::decode_cursor;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData, MetaObject};
use std::sync::{Mutex, MutexGuard};

/// Appends the pagination footer to `final_text` when a next cursor exists.
///
/// Emits `\n` + `NEXT_CURSOR: <cursor>`, and when `cursor_param` is `None`
/// (first page, no cursor passed by the caller) also emits the usage sentence.
/// Byte-identical to the previously inlined assembly at each call site.
pub(crate) fn append_pagination_footer(
    final_text: &mut String,
    next_cursor: Option<&str>,
    cursor_param: Option<&str>,
) {
    if let Some(next) = next_cursor {
        final_text.push('\n');
        final_text.push_str("NEXT_CURSOR: ");
        final_text.push_str(next);
        if cursor_param.is_none() {
            final_text.push('\n');
            final_text.push_str(PAGINATION_USAGE);
        }
    }
}

/// Locks `mutex`, recovering from poisoning by resetting the guarded value to
/// `T::default()` (fresh state), mirroring `lock_or_recover` in
/// `aptu_coder_core::cache`. A single panicked task must not convert every
/// subsequent edit into internal errors; the stale-state cost is bounded and
/// benign (observability counts, empty lock registries).
pub(crate) fn lock_or_recover<'a, T: Default>(
    mutex: &'a Mutex<T>,
    what: &'static str,
) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                what,
                "mutex poisoned; recovering with fresh state instead of panicking"
            );
            let mut guard = poisoned.into_inner();
            *guard = T::default();
            guard
        }
    }
}

/// Locks `mutex`, recovering from poisoning via `PoisonError::into_inner()`
/// so existing entries are retained. Use for registries such as
/// `file_edit_locks`, where resetting to a fresh default would discard live
/// per-path locks held by in-flight edits, reintroducing concurrency.
pub(crate) fn lock_registry_or_recover<'a, T>(
    mutex: &'a Mutex<T>,
    what: &'static str,
) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                what,
                "mutex poisoned; recovering with retained state instead of panicking"
            );
            poisoned.into_inner()
        }
    }
}

/// Returns `true` when `summary=true` and a `cursor` are both provided, which is an invalid
/// combination since summary mode and pagination are mutually exclusive.
#[must_use]
pub(crate) fn summary_cursor_conflict(summary: Option<bool>, cursor: Option<&str>) -> bool {
    summary == Some(true) && cursor.is_some()
}

/// Usage sentence appended on the first page (no request cursor) of paginated
/// analyze tools. Cursor-bearing pages emit only the NEXT_CURSOR line.
pub(crate) const PAGINATION_USAGE: &str = "Pagination: pass cursor=<NEXT_CURSOR> from the line above on your next call with identical params; page sizes are server-owned.";

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
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Edge case: lock_or_recover recovers a deliberately poisoned mutex with a
    /// fresh default value instead of panicking.
    #[test]
    fn lock_or_recover_poisoned_mutex_recovers_fresh() {
        let mutex: Mutex<HashMap<String, u8>> = Mutex::new(HashMap::new());
        {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = mutex.lock().unwrap();
                panic!("deliberate poison");
            }));
            assert!(result.is_err());
        }
        let mut guard = super::lock_or_recover(&mutex, "test_mutex");
        assert!(guard.is_empty(), "recovery resets to the fresh default");
        guard.insert("k".to_string(), 1);
        assert_eq!(guard.get("k"), Some(&1));
    }

    /// Edge case: lock_registry_or_recover recovers a deliberately poisoned
    /// registry mutex while retaining all existing entries (live per-path
    /// locks must survive poisoning).
    #[test]
    fn lock_registry_or_recover_poisoned_mutex_retains_entries() {
        use std::sync::Arc;
        let mutex: Mutex<HashMap<String, Arc<()>>> = Mutex::new(HashMap::new());
        mutex
            .lock()
            .unwrap()
            .insert("a.rs".to_string(), Arc::new(()));
        {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = mutex.lock().unwrap();
                panic!("deliberate poison");
            }));
            assert!(result.is_err());
        }
        let guard = super::lock_registry_or_recover(&mutex, "test_registry");
        assert!(
            guard.contains_key("a.rs"),
            "recovery retains existing registry entries"
        );
        assert_eq!(guard.len(), 1);
    }

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

/// Shared terminal-failure helper for tool handlers: records span error fields,
/// delegates metric emission to the tool-specific `emit_validation` closure, and
/// wraps the `ErrorData` exactly once into a `CallToolResult`.
pub(crate) fn emit_internal_error<F>(
    span: &tracing::Span,
    error_type: &str,
    e: ErrorData,
    emit_validation: F,
) -> Result<CallToolResult, ErrorData>
where
    F: FnOnce(&str),
{
    span.record("error", true);
    span.record("error.type", error_type);
    emit_validation(error_type);
    Ok(err_to_tool_result(e))
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
