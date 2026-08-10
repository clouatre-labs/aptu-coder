// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! File write utilities for the `edit_overwrite` and `edit_replace` tools.

use crate::types::{EditOverwriteOutput, EditReplaceOutput};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use thiserror::Error;

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum EditError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid range: start ({start}) > end ({end}); file has {total} lines")]
    InvalidRange {
        start: usize,
        end: usize,
        total: usize,
    },
    #[error("path is a directory, not a file: {0}")]
    NotAFile(PathBuf),
    #[error(
        "old_text not found in {path} — verify the text matches exactly, including whitespace and newlines"
    )]
    NotFound {
        path: String,
        first_20_lines: String,
    },
    #[error(
        "old_text appears {count} times in {path} — make old_text longer and more specific to uniquely identify the block"
    )]
    Ambiguous {
        count: usize,
        path: String,
        match_lines: Vec<usize>,
    },
    #[error("edit_replace invalid params: {0}")]
    InvalidParams(String),
    #[error(
        "stale content hash for {path}: expected {expected} but file has {actual} — re-read the file with analyze_file or analyze_module, then retry with the current content hash"
    )]
    StaleContentHash {
        expected: String,
        actual: String,
        path: String,
    },
}

fn write_file_atomic(path: &Path, content: &str) -> Result<(), EditError> {
    let parent = path.parent().ok_or_else(|| {
        EditError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent directory",
        ))
    })?;
    let mut temp_file = NamedTempFile::new_in(parent)?;
    use std::io::Write;
    temp_file.write_all(content.as_bytes())?;
    temp_file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Normalize content for matching: replace `\r\n` with `\n`.
/// Single `\r` bytes are left unchanged.
///
/// Returns `Cow::Borrowed` when no `\r` byte is present (fast path, zero allocation).
/// Returns `Cow::Owned` when CRLF sequences require replacement.
fn normalize_for_match(s: &str) -> Cow<'_, str> {
    if !s.as_bytes().contains(&b'\r') {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.replace("\r\n", "\n"))
    }
}

/// Build a sorted vector of normalized byte offsets at which each `\r\n`-derived `\n`
/// occurs. Used to map normalized offsets back to original byte offsets via binary search.
///
/// When the vector is empty, the content has no CRLF sequences and normalized offsets
/// are identical to original offsets (identity mapping).
fn build_crlf_positions(original: &str) -> Vec<usize> {
    let bytes = original.as_bytes();
    let mut positions = Vec::new();
    let mut norm_pos = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            positions.push(norm_pos);
            norm_pos += 1;
            i += 2;
        } else {
            norm_pos += 1;
            i += 1;
        }
    }
    positions
}

/// Map a normalized byte offset back to the corresponding original byte offset
/// using a pre-built CRLF position index.
///
/// When `crlf_positions` is empty, returns `norm_offset` unchanged (identity).
/// Otherwise, counts how many CRLF sequences precede `norm_offset` via binary search
/// and adds that count: `original = norm_offset + crlf_count`.
fn norm_to_original_offset(norm_offset: usize, crlf_positions: &[usize]) -> usize {
    if crlf_positions.is_empty() {
        norm_offset
    } else {
        norm_offset + crlf_positions.partition_point(|&x| x < norm_offset)
    }
}

pub fn edit_overwrite_content(
    path: &Path,
    content: &str,
) -> Result<EditOverwriteOutput, EditError> {
    if path.is_dir() {
        return Err(EditError::NotAFile(path.to_path_buf()));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    write_file_atomic(path, content)?;
    Ok(EditOverwriteOutput {
        path: path.display().to_string(),
        bytes_written: content.len(),
    })
}

pub fn edit_replace_block(
    path: &Path,
    old_text: &str,
    new_text: &str,
) -> Result<EditReplaceOutput, EditError> {
    edit_replace_block_inner(path, old_text, new_text, false, None)
}

/// Same as `edit_replace_block` but with an explicit `replace_all` flag and an
/// optional `expected_content_hash` for optimistic-concurrency staleness
/// detection.
///
/// When `replace_all` is true, all non-overlapping occurrences of `old_text`
/// are replaced in a single pass. When `expected_content_hash` is `Some`, the
/// raw file bytes are hashed with blake3 and compared before the edit proceeds.
/// A mismatch returns `EditError::StaleContentHash`.
pub fn edit_replace_block_with_options(
    path: &Path,
    old_text: &str,
    new_text: &str,
    replace_all: bool,
    expected_content_hash: Option<&str>,
) -> Result<EditReplaceOutput, EditError> {
    edit_replace_block_inner(path, old_text, new_text, replace_all, expected_content_hash)
}

pub(crate) fn edit_replace_block_inner(
    path: &Path,
    old_text: &str,
    new_text: &str,
    replace_all: bool,
    expected_content_hash: Option<&str>,
) -> Result<EditReplaceOutput, EditError> {
    if path.is_dir() {
        return Err(EditError::NotAFile(path.to_path_buf()));
    }
    let content = std::fs::read_to_string(path)?;

    // Staleness check: hash the raw file bytes (as read by read_to_string) and compare
    // with the caller-provided expected hash. Runs inside the per-path lock (caller
    // acquires the lock before calling this function via spawn_blocking).
    if let Some(expected_hash) = expected_content_hash {
        let actual_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        if actual_hash != expected_hash {
            return Err(EditError::StaleContentHash {
                expected: expected_hash.to_string(),
                actual: actual_hash,
                path: path.display().to_string(),
            });
        }
    }

    let norm_content = normalize_for_match(&content);
    let norm_old = normalize_for_match(old_text);
    if norm_old.is_empty() {
        return Err(EditError::InvalidParams(
            "old_text must not be empty".to_string(),
        ));
    }
    // Build CRLF offset index once. When no CRLF is present (Cow::Borrowed),
    // the index is empty and offset mapping is identity.
    let crlf_positions = build_crlf_positions(&content);
    let count = norm_content.matches(norm_old.as_ref()).count();
    match count {
        0 => {
            let first_20_lines = content.lines().take(20).collect::<Vec<_>>().join("\n");
            return Err(EditError::NotFound {
                path: path.display().to_string(),
                first_20_lines,
            });
        }
        1 if !replace_all => {}
        n if !replace_all => {
            let match_lines: Vec<usize> = norm_content
                .match_indices(norm_old.as_ref())
                .map(|(offset, _)| {
                    norm_content[..offset]
                        .bytes()
                        .filter(|&b| b == b'\n')
                        .count()
                        + 1
                })
                .collect();
            return Err(EditError::Ambiguous {
                count: n,
                path: path.display().to_string(),
                match_lines,
            });
        }
        _ => {} // replace_all=true: fall through to single-pass splice
    }
    let bytes_before = content.len();

    if replace_all {
        // Single-pass over original normalized content: collect all match spans,
        // then splice new_text between unmatched spans in original byte space.
        let mut matches: Vec<(usize, usize)> = Vec::new();
        for (norm_start, _m) in norm_content.match_indices(norm_old.as_ref()) {
            let original_start = norm_to_original_offset(norm_start, &crlf_positions);
            let original_end =
                norm_to_original_offset(norm_start + norm_old.len(), &crlf_positions);
            matches.push((original_start, original_end));
        }
        let occurrences_replaced = matches.len();
        let old_span_total: usize = matches.iter().map(|(s, e)| e - s).sum();
        // capacity upper bound: existing bytes + new bytes added - old bytes removed
        // saturating_mul guards against overflow on 32-bit targets with pathological inputs
        let mut result = String::with_capacity(
            bytes_before + new_text.len().saturating_mul(occurrences_replaced) - old_span_total,
        );
        let mut last_end = 0usize;
        for (start, end) in &matches {
            result.push_str(&content[last_end..*start]);
            result.push_str(new_text);
            last_end = *end;
        }
        result.push_str(&content[last_end..]);
        let bytes_after = result.len();
        write_file_atomic(path, &result)?;
        Ok(EditReplaceOutput {
            path: path.display().to_string(),
            bytes_before,
            bytes_after,
            occurrences_replaced,
        })
    } else {
        // Single-match path (existing behavior)
        // SAFETY: match was verified above via count check; find must succeed.
        // If count verification logic changes, this expect() site must be re-audited.
        #[allow(clippy::expect_used)]
        let norm_match_offset = norm_content
            .find(norm_old.as_ref())
            .expect("match was verified above via count check; find must succeed");
        let original_start = norm_to_original_offset(norm_match_offset, &crlf_positions);
        let original_end =
            norm_to_original_offset(norm_match_offset + norm_old.len(), &crlf_positions);
        let updated = [
            &content[..original_start],
            new_text,
            &content[original_end..],
        ]
        .concat();
        let bytes_after = updated.len();
        write_file_atomic(path, &updated)?;
        Ok(EditReplaceOutput {
            path: path.display().to_string(),
            bytes_before,
            bytes_after,
            occurrences_replaced: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_overwrite_content_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.txt");
        let result = edit_overwrite_content(&path, "hello world").unwrap();
        assert_eq!(result.bytes_written, 11);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[test]
    fn edit_overwrite_content_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.txt");
        std::fs::write(&path, "old content").unwrap();
        let result = edit_overwrite_content(&path, "new content").unwrap();
        assert_eq!(result.bytes_written, 11);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content");
    }

    #[test]
    fn edit_overwrite_content_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("c.txt");
        let result = edit_overwrite_content(&path, "nested").unwrap();
        assert_eq!(result.bytes_written, 6);
        assert!(path.exists());
    }

    #[test]
    fn edit_overwrite_content_directory_guard() {
        let dir = tempfile::tempdir().unwrap();
        let err = edit_overwrite_content(dir.path(), "content").unwrap_err();
        std::assert_matches!(err, EditError::NotAFile(_));
    }

    #[test]
    fn edit_replace_block_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "foo bar baz").unwrap();
        let result = edit_replace_block(&path, "bar", "qux").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo qux baz");
        assert_eq!(result.bytes_before, 11);
        assert_eq!(result.bytes_after, 11);
    }

    #[test]
    fn edit_replace_block_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "foo bar baz").unwrap();
        let err = edit_replace_block(&path, "missing", "x").unwrap_err();
        std::assert_matches!(&err, EditError::NotFound { first_20_lines, .. } if !first_20_lines.is_empty());
    }

    #[test]
    fn edit_replace_block_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "foo foo baz").unwrap();
        let err = edit_replace_block(&path, "foo", "x").unwrap_err();
        std::assert_matches!(&err, EditError::Ambiguous { count: 2, match_lines, .. } if match_lines == &[1, 1]);
    }

    #[test]
    fn edit_replace_block_directory_guard() {
        let dir = tempfile::tempdir().unwrap();
        let err = edit_replace_block(dir.path(), "old", "new").unwrap_err();
        std::assert_matches!(err, EditError::NotAFile(_));
    }

    #[test]
    fn edit_replace_block_crlf_file_lf_oldtext() {
        // CRLF file + LF old_text => match succeeds and non-replaced lines retain CRLF bytes
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crlf.txt");
        // Write raw CRLF bytes: "foo\r\nbar\r\nbaz"
        std::fs::write(&path, b"foo\r\nbar\r\nbaz").unwrap();
        let result = edit_replace_block(&path, "bar", "qux").unwrap();
        // The result should contain "foo\r\nqux\r\nbaz" (non-replaced lines retain CRLF)
        let output = std::fs::read_to_string(&path).unwrap();
        assert_eq!(output, "foo\r\nqux\r\nbaz");
        assert_eq!(result.bytes_before, 13); // "foo\r\nbar\r\nbaz" = 13 bytes
        assert_eq!(result.bytes_after, 13); // "foo\r\nqux\r\nbaz" = 13 bytes
    }

    #[test]
    fn edit_replace_block_lf_file_crlf_oldtext() {
        // LF file + CRLF old_text => match succeeds
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lf.txt");
        std::fs::write(&path, b"foo\nbar\nbaz").unwrap();
        let result = edit_replace_block(&path, "bar\r\n", "qux\n").unwrap();
        // old_text "bar\r\n" is normalized to "bar\n", matches "bar\n" in file
        let output = std::fs::read_to_string(&path).unwrap();
        assert_eq!(output, "foo\nqux\nbaz");
        assert_eq!(result.bytes_before, 11); // "foo\nbar\nbaz" = 11 bytes
        assert_eq!(result.bytes_after, 11); // "foo\nqux\nbaz" = 11 bytes
    }

    #[test]
    fn edit_replace_block_crlf_file_crlf_oldtext() {
        // CRLF file + CRLF old_text => both normalized, match succeeds
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bothcrlf.txt");
        std::fs::write(&path, b"line1\r\nline2\r\nline3").unwrap();
        let result = edit_replace_block(&path, "line2\r\n", "replaced\n").unwrap();
        let output = std::fs::read_to_string(&path).unwrap();
        assert_eq!(output, "line1\r\nreplaced\nline3");
        assert_eq!(result.bytes_before, 19); // "line1\r\nline2\r\nline3" = 19 bytes
    }

    #[test]
    fn edit_replace_block_trailing_spaces_distinct() {
        // Two blocks differing only by trailing spaces remain distinct after normalization
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spaces.txt");
        std::fs::write(&path, "foo  \nbar\nfoo\nbar").unwrap();
        // old_text "foo\nbar" should match the SECOND occurrence ("foo\nbar"),
        // not the first ("foo  \nbar"), because trailing spaces are not stripped
        let result = edit_replace_block(&path, "foo\nbar", "replaced").unwrap();
        let output = std::fs::read_to_string(&path).unwrap();
        assert_eq!(output, "foo  \nbar\nreplaced");
        assert_eq!(result.bytes_before, 17); // "foo  \nbar\nfoo\nbar" = 17 bytes
        assert_eq!(result.bytes_after, 18); // "foo  \nbar\nreplaced" = 18 bytes
    }

    #[test]
    fn edit_replace_block_replace_all_three_occurrences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("all.txt");
        std::fs::write(&path, "a b a c a d").unwrap();
        let result = edit_replace_block_with_options(&path, "a", "x", true, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x b x c x d");
        assert_eq!(result.bytes_before, 11);
        assert_eq!(result.bytes_after, 11);
        assert_eq!(result.occurrences_replaced, 3);
    }

    #[test]
    fn edit_replace_block_replace_all_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nf.txt");
        std::fs::write(&path, "foo bar baz").unwrap();
        let err = edit_replace_block_with_options(&path, "missing", "x", true, None).unwrap_err();
        std::assert_matches!(&err, EditError::NotFound { .. });
    }

    #[test]
    fn edit_replace_block_replace_all_empty_oldtext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "foo bar baz").unwrap();
        let err = edit_replace_block_with_options(&path, "", "x", true, None).unwrap_err();
        std::assert_matches!(&err, EditError::InvalidParams(_));
    }

    #[test]
    fn edit_replace_block_replace_all_preserves_crlf() {
        // CRLF file with replace_all: unmatched spans retain CRLF bytes
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crlf_all.txt");
        std::fs::write(&path, b"a\r\nb\r\na\r\nc").unwrap();
        let result = edit_replace_block_with_options(&path, "a", "x", true, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x\r\nb\r\nx\r\nc");
        assert_eq!(result.occurrences_replaced, 2);
    }

    #[test]
    fn replace_all_deletes_all_occurrences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete.txt");
        std::fs::write(&path, "a b a c a d").unwrap();
        let result = edit_replace_block_with_options(&path, "a", "", true, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), " b  c  d");
        assert_eq!(result.bytes_before, 11);
        assert_eq!(result.bytes_after, 8);
        assert_eq!(result.occurrences_replaced, 3);
    }

    #[test]
    fn replace_all_non_overlap_adjacent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("adjacent.txt");
        std::fs::write(&path, "aaaa").unwrap();
        let result = edit_replace_block_with_options(&path, "aa", "xx", true, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "xxxx");
        assert_eq!(result.occurrences_replaced, 2);
    }

    #[test]
    fn replace_all_size_changing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("size.txt");
        std::fs::write(&path, "x y x z x").unwrap();
        let result = edit_replace_block_with_options(&path, "x", "yyy", true, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "yyy y yyy z yyy");
        assert_eq!(result.bytes_before, 9);
        assert_eq!(result.bytes_after, 15);
        assert_eq!(result.occurrences_replaced, 3);
    }

    #[test]
    fn replace_all_empty_oldtext_no_replace_all_returns_invalid_params() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "foo bar baz").unwrap();
        let err = edit_replace_block(&path, "", "x").unwrap_err();
        std::assert_matches!(&err, EditError::InvalidParams(_));
    }

    #[test]
    fn expected_content_hash_mismatch_returns_stale_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.txt");
        std::fs::write(&path, "hello world").unwrap();
        let err = edit_replace_block_with_options(
            &path,
            "hello",
            "hi",
            false,
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
        )
        .unwrap_err();
        std::assert_matches!(&err, EditError::StaleContentHash { path: p, .. } if p.contains("stale.txt"));
        // File must be unmodified
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[test]
    fn expected_content_hash_match_proceeds_normally() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("match.txt");
        std::fs::write(&path, "hello world").unwrap();
        let raw_bytes = std::fs::read(&path).unwrap();
        let hash = blake3::hash(&raw_bytes).to_hex().to_string();
        let result =
            edit_replace_block_with_options(&path, "hello", "hi", false, Some(&hash)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi world");
        assert_eq!(result.occurrences_replaced, 1);
    }

    #[test]
    fn expected_content_hash_none_skips_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nohash.txt");
        std::fs::write(&path, "foo bar baz").unwrap();
        let result = edit_replace_block_with_options(&path, "bar", "qux", false, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo qux baz");
        assert_eq!(result.occurrences_replaced, 1);
    }

    #[test]
    fn replace_all_crlf_offset_index_byte_identical() {
        // After Cow fast path and offset-index refactor, replace_all on a mixed
        // CRLF/LF file produces byte-identical output to the previous linear-walk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed_crlf.txt");
        // Mixed CRLF and LF: "a\r\nb\na\r\nc\na\r\nd"
        let original = b"a\r\nb\na\r\nc\na\r\nd";
        std::fs::write(&path, original).unwrap();
        let result = edit_replace_block_with_options(&path, "a", "XYZ", true, None).unwrap();
        assert_eq!(result.occurrences_replaced, 3);
        let output = std::fs::read(&path).unwrap();
        // Expected: "XYZ\r\nb\nXYZ\r\nc\nXYZ\r\nd"
        assert_eq!(output, b"XYZ\r\nb\nXYZ\r\nc\nXYZ\r\nd");
    }
}
