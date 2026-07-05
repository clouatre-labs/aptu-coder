// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! File write utilities for the `edit_overwrite` and `edit_replace` tools.

use crate::types::{EditOverwriteOutput, EditReplaceOutput};
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
fn normalize_for_match(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Map a byte offset in normalized content (CRLF -> LF) back to the corresponding
/// byte offset in the original content, starting from `original_start`.
fn norm_offset_to_original_from(
    original: &str,
    norm_offset: usize,
    original_start: usize,
) -> usize {
    // Performance: O(n) byte walk is acceptable for the file sizes MCP tools operate on
    // (source files, typically <1 MB). If very large file support becomes a requirement,
    // a pre-built CRLF offset index could reduce this to O(log n) per lookup.
    let bytes = original.as_bytes();
    let mut norm_pos = 0usize;
    let mut i = original_start;
    while i < bytes.len() && norm_pos < norm_offset {
        if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            norm_pos += 1;
            i += 2;
        } else {
            norm_pos += 1;
            i += 1;
        }
    }
    i
}

/// Map a byte offset in normalized content back to the corresponding byte offset
/// in the original content.
fn norm_offset_to_original(original: &str, norm_offset: usize) -> usize {
    norm_offset_to_original_from(original, norm_offset, 0)
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
    if path.is_dir() {
        return Err(EditError::NotAFile(path.to_path_buf()));
    }
    let content = std::fs::read_to_string(path)?;
    let norm_content = normalize_for_match(&content);
    let norm_old = normalize_for_match(old_text);
    let count = norm_content.matches(&norm_old).count();
    match count {
        0 => {
            let first_20_lines = content.lines().take(20).collect::<Vec<_>>().join("\n");
            return Err(EditError::NotFound {
                path: path.display().to_string(),
                first_20_lines,
            });
        }
        1 => {}
        n => {
            let match_lines: Vec<usize> = norm_content
                .match_indices(&norm_old)
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
    }
    let bytes_before = content.len();
    // Find match offset in normalized space, then map back to original byte range
    // SAFETY: match was verified above via count check; find must succeed.
    // If count verification logic changes, this expect() site must be re-audited.
    #[allow(clippy::expect_used)]
    let norm_match_offset = norm_content
        .find(&norm_old)
        .expect("match was verified above via count check; find must succeed");
    let original_start = norm_offset_to_original(&content, norm_match_offset);
    let original_end = norm_offset_to_original_from(&content, norm_old.len(), original_start);
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
}
