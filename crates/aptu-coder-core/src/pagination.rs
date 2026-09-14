// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Cursor-based pagination for large result sets.
//!
//! Provides encoding and decoding of pagination cursors to track position within result sets,
//! supporting different pagination modes (default, callers, callees). Uses base64-encoded JSON.

use base64::engine::general_purpose::STANDARD;
use base64::{DecodeError, engine::Engine};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Legacy default kept for internal callers (MCP Resources) that do not take client page sizes.
pub const DEFAULT_PAGE_SIZE: usize = 100;
pub const MAX_PAGE_SIZE: usize = 10_000;

/// Selects which call-chain direction a pagination cursor tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaginationMode {
    /// Standard file/directory listing or call-graph default traversal.
    Default,
    /// Paginating through the callers chain of a symbol.
    Callers,
    /// Paginating through the callees chain of a symbol.
    Callees,
    /// Paginating through def-use sites for a symbol.
    DefUse,
}

/// Serializable state embedded in a pagination cursor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorData {
    /// Which chain direction this cursor belongs to.
    pub mode: PaginationMode,
    /// Zero-based index of the next item to return.
    pub offset: usize,
}

#[derive(Debug, Error)]
pub enum PaginationError {
    #[error("Invalid cursor: {0}")]
    InvalidCursor(String),
}

impl From<DecodeError> for PaginationError {
    fn from(err: DecodeError) -> Self {
        PaginationError::InvalidCursor(format!("Base64 decode error: {err}"))
    }
}

impl From<serde_json::Error> for PaginationError {
    fn from(err: serde_json::Error) -> Self {
        PaginationError::InvalidCursor(format!("JSON parse error: {err}"))
    }
}

/// Encode a cursor into a base64-encoded JSON string.
///
/// # Errors
///
/// Returns `PaginationError::InvalidCursor` if JSON serialization fails.
pub fn encode_cursor(data: &CursorData) -> Result<String, PaginationError> {
    let json = serde_json::to_string(data)?;
    Ok(STANDARD.encode(json))
}

/// Decode a base64-encoded JSON cursor string.
///
/// # Errors
///
/// Returns `PaginationError::InvalidCursor` if base64 decoding fails, UTF-8 conversion fails, or JSON parsing fails.
pub fn decode_cursor(cursor: &str) -> Result<CursorData, PaginationError> {
    let decoded = STANDARD.decode(cursor)?;
    let json_str = String::from_utf8(decoded)
        .map_err(|e| PaginationError::InvalidCursor(format!("UTF-8 decode error: {e}")))?;
    Ok(serde_json::from_str(&json_str)?)
}

#[derive(Debug, Clone)]
pub struct PaginationResult<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub total: usize,
}

/// Paginate a slice, returning a page of items and an optional next-page cursor.
///
/// Returns [`PaginationResult`] with the page items, total count, and a base64-encoded
/// cursor for the next page (or `None` if this is the last page).
///
/// # Errors
///
/// Returns [`PaginationError`] if cursor encoding fails.
pub fn paginate_slice<T: Clone>(
    items: &[T],
    offset: usize,
    page_size: usize,
    mode: PaginationMode,
) -> Result<PaginationResult<T>, PaginationError> {
    if page_size == 0 {
        return Err(PaginationError::InvalidCursor(
            "page_size must be at least 1".to_string(),
        ));
    }

    let total = items.len();

    if offset >= total {
        return Ok(PaginationResult {
            items: vec![],
            next_cursor: None,
            total,
        });
    }

    let clamped_page_size = page_size.min(MAX_PAGE_SIZE);
    let end = std::cmp::min(offset.saturating_add(clamped_page_size), total);
    let page_items = items[offset..end].to_vec();

    let next_cursor = if end < total {
        let cursor_data = CursorData { mode, offset: end };
        Some(encode_cursor(&cursor_data)?)
    } else {
        None
    };

    Ok(PaginationResult {
        items: page_items,
        next_cursor,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_encode_decode_roundtrip() {
        let original = CursorData {
            mode: PaginationMode::Default,
            offset: 42,
        };

        let encoded = encode_cursor(&original).expect("encode failed");
        let decoded = decode_cursor(&encoded).expect("decode failed");

        assert_eq!(decoded.mode, original.mode);
        assert_eq!(decoded.offset, original.offset);
    }

    #[test]
    fn test_pagination_mode_wire_format() {
        let cursor_data = CursorData {
            mode: PaginationMode::Callers,
            offset: 0,
        };

        let encoded = encode_cursor(&cursor_data).expect("encode failed");
        let decoded = decode_cursor(&encoded).expect("decode failed");

        assert_eq!(decoded.mode, PaginationMode::Callers);

        let json_str = serde_json::to_string(&cursor_data).expect("serialize failed");
        assert!(
            json_str.contains("\"mode\":\"callers\""),
            "expected lowercase 'callers' in JSON, got: {}",
            json_str
        );
    }

    #[test]
    fn test_pagination_mode_defuse_roundtrip() {
        let original = CursorData {
            mode: PaginationMode::DefUse,
            offset: 123,
        };

        let encoded = encode_cursor(&original).expect("encode failed");
        let decoded = decode_cursor(&encoded).expect("decode failed");

        assert_eq!(decoded.mode, PaginationMode::DefUse);
        assert_eq!(decoded.offset, 123);
    }

    #[test]
    fn test_paginate_slice_middle_page() {
        let items: Vec<i32> = (0..250).collect();
        let result =
            paginate_slice(&items, 100, 100, PaginationMode::Default).expect("paginate failed");

        assert_eq!(result.items.len(), 100);
        assert_eq!(result.items[0], 100);
        assert_eq!(result.items[99], 199);
        assert!(result.next_cursor.is_some());
        assert_eq!(result.total, 250);
    }

    #[test]
    fn test_paginate_slice_empty_and_beyond() {
        let empty: Vec<i32> = vec![];
        let result =
            paginate_slice(&empty, 0, 100, PaginationMode::Default).expect("paginate failed");
        assert_eq!(result.items.len(), 0);
        assert!(result.next_cursor.is_none());
        assert_eq!(result.total, 0);

        let items: Vec<i32> = (0..50).collect();
        let result =
            paginate_slice(&items, 100, 100, PaginationMode::Default).expect("paginate failed");
        assert_eq!(result.items.len(), 0);
        assert!(result.next_cursor.is_none());
        assert_eq!(result.total, 50);
    }

    #[test]
    fn test_paginate_slice_exact_boundary() {
        let items: Vec<i32> = (0..200).collect();
        let result =
            paginate_slice(&items, 100, 100, PaginationMode::Default).expect("paginate failed");

        assert_eq!(result.items.len(), 100);
        assert_eq!(result.items[0], 100);
        assert!(result.next_cursor.is_none());
        assert_eq!(result.total, 200);
    }

    #[test]
    fn test_invalid_cursor_error() {
        let result = decode_cursor("not-valid-base64!!!");
        assert!(result.is_err());
        match result {
            Err(PaginationError::InvalidCursor(msg)) => {
                assert!(msg.contains("Base64") || msg.contains("decode"));
            }
            _ => panic!("Expected InvalidCursor error"),
        }
    }
}
