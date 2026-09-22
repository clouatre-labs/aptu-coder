// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! File write utilities for the `edit_overwrite` and `edit_replace` tools.

use crate::types::{BatchEdit, EditOverwriteOutput, EditReplaceOutput};
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
    #[error("batch edits failed validation: {}", format_batch_failures(.failures))]
    BatchValidationFailed {
        /// Per-index failure details, sorted by index.
        failures: Vec<BatchFailure>,
    },
}

/// Formats per-index batch failures for the [`EditError`] Display impl.
fn format_batch_failures(failures: &[BatchFailure]) -> String {
    failures
        .iter()
        .map(|f| format!("edit {}: {}", f.index, f.message))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Per-index failure detail for [`EditError::BatchValidationFailed`].
#[derive(Debug, Clone)]
pub struct BatchFailure {
    /// Zero-based index of the failed edit in the request `edits[]` array.
    pub index: usize,
    /// Human-readable reason including the 1-based line number where relevant.
    pub message: String,
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

/// Per-line trim-end index over normalized content.
///
/// Maps offsets in a "trimmed" representation (every line with trailing
/// whitespace removed, newlines preserved) back to offsets in the normalized
/// content. This is separate from [`build_crlf_positions`]/[`norm_to_original_offset`],
/// which only handle CRLF normalization.
struct TrimEndIndex {
    /// Normalized content with per-line trailing whitespace removed.
    trimmed: String,
    /// For each line: offset where the line's trimmed body starts in `trimmed`.
    line_trim_starts: Vec<usize>,
    /// For each line: offset where the line starts in the normalized content.
    line_norm_starts: Vec<usize>,
    /// Length of the normalized content.
    norm_len: usize,
}

impl TrimEndIndex {
    /// Map an offset in `trimmed` back to the corresponding normalized offset.
    fn map_to_norm(&self, off: usize) -> usize {
        // SAFETY of arithmetic: `off <= trimmed.len()` by construction from match
        // spans, and the first line's trim start is 0, so the subtract-1 is safe.
        let idx = self.line_trim_starts.partition_point(|&s| s <= off) - 1;
        self.line_norm_starts[idx] + (off - self.line_trim_starts[idx])
    }

    /// Map the end offset of a match in `trimmed` back to a normalized offset.
    ///
    /// When the match ends at the end of a trimmed line, the mapped offset
    /// extends past the line's trailing whitespace and newline: those bytes are
    /// consumed by the splice.
    fn map_end_to_norm(&self, off: usize) -> usize {
        let at_line_end =
            off == self.trimmed.len() || self.trimmed.as_bytes().get(off) == Some(&b'\n');
        if !at_line_end {
            return self.map_to_norm(off);
        }
        let idx = self.line_trim_starts.partition_point(|&s| s <= off) - 1;
        if idx + 1 < self.line_norm_starts.len() {
            self.line_norm_starts[idx + 1]
        } else {
            self.norm_len
        }
    }
}

/// Build a [`TrimEndIndex`] over normalized content: each line's trailing
/// whitespace is stripped (newline preserved) and per-line offset tables are
/// recorded for mapping back to normalized coordinates.
fn build_trim_end_index(norm: &str) -> TrimEndIndex {
    let mut trimmed = String::with_capacity(norm.len());
    let mut line_trim_starts = Vec::new();
    let mut line_norm_starts = Vec::new();
    let mut trim_off = 0usize;
    let mut norm_off = 0usize;
    for line in norm.split_inclusive('\n') {
        line_trim_starts.push(trim_off);
        line_norm_starts.push(norm_off);
        let body = line.strip_suffix('\n').unwrap_or(line);
        let t = body.trim_end();
        trimmed.push_str(t);
        trim_off += t.len();
        if line.ends_with('\n') {
            trimmed.push('\n');
            trim_off += 1;
        }
        norm_off += line.len();
    }
    TrimEndIndex {
        trimmed,
        line_trim_starts,
        line_norm_starts,
        norm_len: norm.len(),
    }
}

/// Strip per-line trailing whitespace from a normalized multi-line string,
/// preserving newlines. Used to transform `old_text` for trim-end comparison.
fn trim_end_lines(norm: &str) -> String {
    let mut out = String::with_capacity(norm.len());
    for line in norm.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        out.push_str(body.trim_end());
        if line.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Find all occurrences of `norm_old` in `norm_content` where each line matches
/// after per-line trim-end comparison. Returns `(start, end)` spans in ORIGINAL
/// content byte coordinates (trailing whitespace of matched lines is included,
/// i.e. consumed by the splice). Uses its own per-line trim-end offset table,
/// not `norm_to_original_offset`, for the trim-end span mapping.
fn find_trim_end_matches(
    norm_content: &str,
    norm_old: &str,
    crlf_positions: &[usize],
) -> Vec<(usize, usize)> {
    let trimmed_old = trim_end_lines(norm_old);
    if trimmed_old.is_empty() {
        return Vec::new();
    }
    let index = build_trim_end_index(norm_content);
    let mut spans = Vec::new();
    for (start, _) in index.trimmed.match_indices(trimmed_old.as_str()) {
        let end = start + trimmed_old.len();
        let norm_start = index.map_to_norm(start);
        let norm_end = index.map_end_to_norm(end);
        spans.push((
            norm_to_original_offset(norm_start, crlf_positions),
            norm_to_original_offset(norm_end, crlf_positions),
        ));
    }
    spans
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
        // Diff fields are populated by the MCP handler when include_diff=true.
        diff_truncated: None,
        diff_bytes: None,
    })
}

/// Replaces an exact text block; `old_text` must appear exactly once unless
/// `replace_all` is true, in which case all non-overlapping occurrences are
/// replaced in a single pass. When `expected_content_hash` is `Some`, the
/// raw file bytes are hashed with blake3 and compared before the edit proceeds.
/// A mismatch returns `EditError::StaleContentHash`.
pub fn edit_replace_block(
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
    // Exact-first matching: collect match spans in original byte space. Only
    // when there are zero exact matches, fall back silently to per-line
    // trim-end matching (also in original byte space).
    let exact_matches: Vec<(usize, usize)> = norm_content
        .match_indices(norm_old.as_ref())
        .map(|(offset, _)| {
            (
                norm_to_original_offset(offset, &crlf_positions),
                norm_to_original_offset(offset + norm_old.len(), &crlf_positions),
            )
        })
        .collect();
    let matches = if exact_matches.is_empty() {
        find_trim_end_matches(&norm_content, norm_old.as_ref(), &crlf_positions)
    } else {
        exact_matches
    };
    match matches.len() {
        0 => {
            let first_20_lines = content.lines().take(20).collect::<Vec<_>>().join("\n");
            return Err(EditError::NotFound {
                path: path.display().to_string(),
                first_20_lines,
            });
        }
        n if n > 1 && !replace_all => {
            let match_lines: Vec<usize> = matches
                .iter()
                .map(|(start, _)| content[..*start].bytes().filter(|&b| b == b'\n').count() + 1)
                .collect();
            return Err(EditError::Ambiguous {
                count: n,
                path: path.display().to_string(),
                match_lines,
            });
        }
        _ => {} // single match, or replace_all=true: fall through to single-pass splice
    }
    let bytes_before = content.len();

    {
        // Single-pass over original content: splice new_text between unmatched
        // spans in original byte space.
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
            // Single-edit response shape is unchanged: batch-only fields stay unset.
            content_hash: None,
            edits: None,
            diff_truncated: None,
            diff_bytes: None,
        })
    }
}

/// Applies multiple exact-text replacements to one file atomically.
///
/// All edits validate against one content snapshot (a single blake3 hash check when
/// `expected_content_hash` is `Some`) and apply via one sorted-span splice followed by one
/// atomic write. Any invalid edit aborts the entire batch with
/// [`EditError::BatchValidationFailed`] and no write.
pub fn edit_replace_batch(
    path: &Path,
    edits: &[BatchEdit],
    expected_content_hash: Option<&str>,
) -> Result<EditReplaceOutput, EditError> {
    if path.is_dir() {
        return Err(EditError::NotAFile(path.to_path_buf()));
    }
    let content = std::fs::read_to_string(path)?;
    // Staleness check: hash the raw bytes once, before any validation runs.
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
    let bytes_before = content.len();
    // One normalize and one CRLF offset index per batch call.
    let norm_content = normalize_for_match(&content);
    let crlf_positions = build_crlf_positions(&content);
    let line_at = |offset: usize| {
        norm_content[..offset]
            .bytes()
            .filter(|&b| b == b'\n')
            .count()
            + 1
    };

    // Collect match spans in original byte space, recording per-index failures.
    let mut all_spans: Vec<(usize, usize, usize)> = Vec::new(); // (start, end, edit_index)
    let mut failures: Vec<BatchFailure> = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let mut push = |message: String| failures.push(BatchFailure { index, message });
        let norm_old = normalize_for_match(&edit.old_text);
        if norm_old.is_empty() {
            push("old_text must not be empty".to_string());
            continue;
        }
        let norm_old_len = norm_old.len();
        let matches: Vec<usize> = norm_content
            .match_indices(norm_old.as_ref())
            .map(|(o, _)| o)
            .collect();
        if matches.is_empty() {
            // Exact-first: on zero exact matches, fall back silently to per-line
            // trim-end matching. Spans are already in original byte space.
            let spans = find_trim_end_matches(&norm_content, norm_old.as_ref(), &crlf_positions);
            if spans.is_empty() {
                push(format!(
                    "old_text not found in {} — verify the text matches exactly, including whitespace and newlines",
                    path.display()
                ));
                continue;
            }
            if spans.len() > 1 && !edit.replace_all.unwrap_or(false) {
                let lines = spans
                    .iter()
                    .map(|(s, _)| content[..*s].bytes().filter(|&b| b == b'\n').count() + 1)
                    .map(|l| l.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                push(format!(
                    "old_text appears {} times in {} (lines {lines}) — make old_text longer and more specific to uniquely identify the block",
                    spans.len(),
                    path.display()
                ));
                continue;
            }
            for (start, end) in spans {
                all_spans.push((start, end, index));
            }
            continue;
        }
        if matches.len() > 1 && !edit.replace_all.unwrap_or(false) {
            let lines = matches
                .iter()
                .map(|&o| line_at(o).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            push(format!(
                "old_text appears {} times in {} (lines {lines}) — make old_text longer and more specific to uniquely identify the block",
                matches.len(),
                path.display()
            ));
            continue;
        }
        for norm_start in matches {
            let start = norm_to_original_offset(norm_start, &crlf_positions);
            let end = norm_to_original_offset(norm_start + norm_old_len, &crlf_positions);
            all_spans.push((start, end, index));
        }
    }

    // Sort spans by start; reject intersecting regions. Adjacent (abutting) regions are
    // allowed: matches were collected against one pre-splice snapshot, so the splice is
    // deterministic regardless of adjacency.
    all_spans.sort_by_key(|&(start, end, index)| (start, end, index));
    let mut last_end: Option<usize> = None;
    for &(start, end, index) in &all_spans {
        if let Some(prev_end) = last_end.filter(|&prev_end| start < prev_end) {
            last_end = Some(end.max(prev_end));
            failures.push(BatchFailure {
                index,
                message: format!(
                    "edit region (bytes {start}..{end}) overlaps an earlier edit region ending at byte {prev_end}; all edits must target non-overlapping regions"
                ),
            });
            continue;
        }
        last_end = Some(match last_end {
            Some(prev_end) => prev_end.max(end),
            None => end,
        });
    }

    if !failures.is_empty() {
        failures.sort_by_key(|f| f.index);
        return Err(EditError::BatchValidationFailed { failures });
    }

    // Single last_end splice over original bytes.
    let mut result = String::with_capacity(bytes_before);
    let mut cursor = 0usize;
    let mut per_edit_counts = vec![0usize; edits.len()];
    for (start, end, index) in all_spans {
        result.push_str(&content[cursor..start]);
        result.push_str(&edits[index].new_text);
        cursor = end;
        per_edit_counts[index] += 1;
    }
    result.push_str(&content[cursor..]);
    let bytes_after = result.len();
    write_file_atomic(path, &result)?;
    Ok(EditReplaceOutput {
        path: path.display().to_string(),
        bytes_before,
        bytes_after,
        occurrences_replaced: per_edit_counts.iter().sum(),
        edits: Some(
            edits
                .iter()
                .enumerate()
                .map(|(index, _)| crate::types::BatchEditResult {
                    index,
                    occurrences_replaced: per_edit_counts[index],
                })
                .collect(),
        ),
        content_hash: Some(blake3::hash(result.as_bytes()).to_hex().to_string()),
        diff_truncated: None,
        diff_bytes: None,
    })
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
        let result = edit_replace_block(&path, "bar", "qux", false, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo qux baz");
        assert_eq!(result.bytes_before, 11);
        assert_eq!(result.bytes_after, 11);
    }

    #[test]
    fn edit_replace_block_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "foo bar baz").unwrap();
        let err = edit_replace_block(&path, "missing", "x", false, None).unwrap_err();
        std::assert_matches!(&err, EditError::NotFound { first_20_lines, .. } if !first_20_lines.is_empty());
    }

    #[test]
    fn edit_replace_block_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "foo foo baz").unwrap();
        let err = edit_replace_block(&path, "foo", "x", false, None).unwrap_err();
        std::assert_matches!(&err, EditError::Ambiguous { count: 2, match_lines, .. } if match_lines == &[1, 1]);
    }

    #[test]
    fn edit_replace_block_directory_guard() {
        let dir = tempfile::tempdir().unwrap();
        let err = edit_replace_block(dir.path(), "old", "new", false, None).unwrap_err();
        std::assert_matches!(err, EditError::NotAFile(_));
    }

    #[test]
    fn edit_replace_block_crlf_file_lf_oldtext() {
        // CRLF file + LF old_text => match succeeds and non-replaced lines retain CRLF bytes
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crlf.txt");
        // Write raw CRLF bytes: "foo\r\nbar\r\nbaz"
        std::fs::write(&path, b"foo\r\nbar\r\nbaz").unwrap();
        let result = edit_replace_block(&path, "bar", "qux", false, None).unwrap();
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
        let result = edit_replace_block(&path, "bar\r\n", "qux\n", false, None).unwrap();
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
        let result = edit_replace_block(&path, "line2\r\n", "replaced\n", false, None).unwrap();
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
        let result = edit_replace_block(&path, "foo\nbar", "replaced", false, None).unwrap();
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
        let result = edit_replace_block(&path, "a", "x", true, None).unwrap();
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
        let err = edit_replace_block(&path, "missing", "x", true, None).unwrap_err();
        std::assert_matches!(&err, EditError::NotFound { .. });
    }

    #[test]
    fn edit_replace_block_replace_all_empty_oldtext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "foo bar baz").unwrap();
        let err = edit_replace_block(&path, "", "x", true, None).unwrap_err();
        std::assert_matches!(&err, EditError::InvalidParams(_));
    }

    #[test]
    fn edit_replace_block_replace_all_preserves_crlf() {
        // CRLF file with replace_all: unmatched spans retain CRLF bytes
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crlf_all.txt");
        std::fs::write(&path, b"a\r\nb\r\na\r\nc").unwrap();
        let result = edit_replace_block(&path, "a", "x", true, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x\r\nb\r\nx\r\nc");
        assert_eq!(result.occurrences_replaced, 2);
    }

    #[test]
    fn replace_all_deletes_all_occurrences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete.txt");
        std::fs::write(&path, "a b a c a d").unwrap();
        let result = edit_replace_block(&path, "a", "", true, None).unwrap();
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
        let result = edit_replace_block(&path, "aa", "xx", true, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "xxxx");
        assert_eq!(result.occurrences_replaced, 2);
    }

    #[test]
    fn replace_all_size_changing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("size.txt");
        std::fs::write(&path, "x y x z x").unwrap();
        let result = edit_replace_block(&path, "x", "yyy", true, None).unwrap();
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
        let err = edit_replace_block(&path, "", "x", false, None).unwrap_err();
        std::assert_matches!(&err, EditError::InvalidParams(_));
    }

    #[test]
    fn expected_content_hash_mismatch_returns_stale_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.txt");
        std::fs::write(&path, "hello world").unwrap();
        let err = edit_replace_block(
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
        let result = edit_replace_block(&path, "hello", "hi", false, Some(&hash)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi world");
        assert_eq!(result.occurrences_replaced, 1);
    }

    #[test]
    fn expected_content_hash_none_skips_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nohash.txt");
        std::fs::write(&path, "foo bar baz").unwrap();
        let result = edit_replace_block(&path, "bar", "qux", false, None).unwrap();
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
        let result = edit_replace_block(&path, "a", "XYZ", true, None).unwrap();
        assert_eq!(result.occurrences_replaced, 3);
        let output = std::fs::read(&path).unwrap();
        // Expected: "XYZ\r\nb\nXYZ\r\nc\nXYZ\r\nd"
        assert_eq!(output, b"XYZ\r\nb\nXYZ\r\nc\nXYZ\r\nd");
    }

    /// Builds a batch edit item with `replace_all` unset.
    fn be(old_text: &str, new_text: &str) -> BatchEdit {
        BatchEdit {
            old_text: old_text.into(),
            new_text: new_text.into(),
            replace_all: None,
        }
    }

    #[test]
    fn edit_replace_batch_happy_path_mixed_single_and_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("batch.txt");
        std::fs::write(&path, "a b a c a d").unwrap();
        let mut replace_all_edit = be("a", "X");
        replace_all_edit.replace_all = Some(true);
        let out = edit_replace_batch(&path, &[be("b", "B"), replace_all_edit], None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "X B X c X d");
        assert_eq!(
            (
                out.edits.as_ref().unwrap()[0].occurrences_replaced,
                out.edits.as_ref().unwrap()[1].occurrences_replaced
            ),
            (1, 3)
        );
        assert_eq!(
            out.content_hash.as_deref(),
            Some(
                blake3::hash(&std::fs::read(&path).unwrap())
                    .to_hex()
                    .as_str()
            )
        );
    }

    /// Asserts the batch fails validation with a failure at `expected_index`.
    fn assert_rejected(path: &Path, edits: &[BatchEdit], expected_index: usize) {
        match edit_replace_batch(path, edits, None) {
            Err(EditError::BatchValidationFailed { failures }) => {
                assert!(failures.iter().any(|f| f.index == expected_index));
            }
            other => panic!("expected BatchValidationFailed, got {other:?}"),
        }
    }

    #[test]
    fn edit_replace_batch_rejection_and_crlf_cases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cases.txt");
        // Overlap: intersecting spans rejected at the later index; file untouched.
        std::fs::write(&path, "hello world").unwrap();
        assert_rejected(&path, &[be("hello", "X"), be("hello w", "Y")], 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
        // Duplicate old_text rejected at the later index.
        std::fs::write(&path, "target here").unwrap();
        assert_rejected(&path, &[be("target", "A"), be("target", "B")], 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "target here");
        // One invalid edit aborts the batch; bytes stay bit-identical.
        std::fs::write(&path, b"keep\r\nme").unwrap();
        let edits = vec![be("keep", "changed"), be("missing", "x")];
        assert!(matches!(
            edit_replace_batch(&path, &edits, None),
            Err(EditError::BatchValidationFailed { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"keep\r\nme");
        // Edit spanning a CRLF boundary keeps surrounding CRLF bytes intact.
        std::fs::write(&path, b"foo\r\nbar\r\nbaz").unwrap();
        let out = edit_replace_batch(&path, &[be("bar\r\nbaz", "qux")], None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"foo\r\nqux");
        assert_eq!(out.edits.as_ref().unwrap()[0].occurrences_replaced, 1);
    }

    #[test]
    fn edit_replace_batch_empty_oldtext_fails_index_and_empty_edits_rejected_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty_item.txt");
        std::fs::write(&path, "content").unwrap();
        match edit_replace_batch(&path, &[be("", "x")], None) {
            Err(EditError::BatchValidationFailed { failures }) => {
                assert_eq!((failures.len(), failures[0].index), (1, 0));
                assert!(failures[0].message.contains("must not be empty"));
            }
            other => panic!("expected BatchValidationFailed, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "content");
    }

    #[test]
    fn edit_replace_batch_allows_abutting_regions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abut.txt");
        std::fs::write(&path, "abcd").unwrap();
        let out = edit_replace_batch(&path, &[be("ab", "X"), be("cd", "Y")], None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "XY");
        assert_eq!(out.occurrences_replaced, 2);
    }

    #[test]
    fn edit_replace_batch_stale_hash_rejected_before_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale_batch.txt");
        std::fs::write(&path, "hello").unwrap();
        let err = edit_replace_batch(
            &path,
            &[be("missing", "x")],
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
        )
        .unwrap_err();
        // Stale hash takes precedence over per-edit validation failures.
        std::assert_matches!(err, EditError::StaleContentHash { .. });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn edit_replace_block_trim_end_fallback_when_no_exact_match() {
        // Fallback fires when the exact match is absent: lines carry trailing
        // whitespace that the old_text lacks. Original trailing bytes are consumed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fallback.txt");
        std::fs::write(&path, "foo  \nbar\t\nbaz").unwrap();
        let result = edit_replace_block(&path, "foo\nbar", "qux\n", false, None).unwrap();
        let output = std::fs::read_to_string(&path).unwrap();
        assert_eq!(output, "qux\nbaz");
        assert_eq!(result.bytes_before, 14); // "foo  \nbar\t\nbaz"
        assert_eq!(result.bytes_after, 7); // "qux\nbaz"
        assert_eq!(result.occurrences_replaced, 1);
    }

    #[test]
    fn edit_replace_block_exact_match_wins_over_trim_end_occurrence() {
        // Covered behavior: the existing trailing_spaces_distinct test pins that
        // an exact match wins over a trim-end-only occurrence. Re-assert here in
        // the fallback family: content has both a trailing-space block and an
        // exact block; the exact block must be replaced, and the trailing-space
        // block must remain untouched (no fallback firing).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exact_wins.txt");
        std::fs::write(&path, "foo  \nbar\nfoo\nbar").unwrap();
        let result = edit_replace_block(&path, "foo\nbar", "X", false, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo  \nbar\nX");
        assert_eq!(result.occurrences_replaced, 1);
    }

    #[test]
    fn edit_replace_block_trim_end_fallback_multi_match_is_ambiguous() {
        // Fallback producing multiple occurrences with replace_all=false is ambiguous.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ambiguous_fb.txt");
        std::fs::write(&path, "foo \nbar \nfoo\t\nbar").unwrap();
        let err = edit_replace_block(&path, "foo\nbar", "x", false, None).unwrap_err();
        std::assert_matches!(&err, EditError::Ambiguous { count: 2, match_lines, .. } if match_lines == &[1, 3]);
        // File must be unmodified.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "foo \nbar \nfoo\t\nbar"
        );
    }

    #[test]
    fn edit_replace_block_trim_end_fallback_replace_all_mixed_crlf() {
        // Fallback with replace_all=true replaces all trim-end occurrences using
        // correct original-byte spans in mixed CRLF + trailing-space content.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("all_fb.txt");
        std::fs::write(&path, b"x a  \r\nb\r\nx a \r\nb").unwrap();
        let result = edit_replace_block(&path, "x a\nb", "R", true, None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"RR");
        assert_eq!(result.occurrences_replaced, 2);
        assert_eq!(result.bytes_before, 17);
        assert_eq!(result.bytes_after, 2);
    }

    #[test]
    fn edit_replace_batch_trim_end_fallback_leaves_other_edits_unaffected() {
        // Batch edit with no exact match succeeds via trim-end fallback while the
        // other edit applies normally; non-overlap checks hold in original-byte space.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("batch_fb.txt");
        std::fs::write(&path, "foo  \nbar\nother").unwrap();
        let out =
            edit_replace_batch(&path, &[be("foo\nbar", "X"), be("other", "Y")], None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "XY");
        assert_eq!(
            (
                out.edits.as_ref().unwrap()[0].occurrences_replaced,
                out.edits.as_ref().unwrap()[1].occurrences_replaced
            ),
            (1, 1)
        );
    }

    #[test]
    fn edit_replace_batch_trim_end_fallback_not_found_still_fails() {
        // Fallback finds nothing: the edit fails validation and the file is untouched.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("batch_nf.txt");
        std::fs::write(&path, "hello  \nworld").unwrap();
        match edit_replace_batch(&path, &[be("missing\ntext", "x")], None) {
            Err(EditError::BatchValidationFailed { failures }) => {
                assert_eq!(failures.len(), 1);
                assert!(failures[0].message.contains("not found"));
            }
            other => panic!("expected BatchValidationFailed, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello  \nworld");
    }
}
