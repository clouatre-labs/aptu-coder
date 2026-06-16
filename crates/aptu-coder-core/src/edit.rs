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
    let count = content.matches(old_text).count();
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
            let match_lines: Vec<usize> = content
                .match_indices(old_text)
                .map(|(offset, _)| content[..offset].bytes().filter(|&b| b == b'\n').count() + 1)
                .collect();
            return Err(EditError::Ambiguous {
                count: n,
                path: path.display().to_string(),
                match_lines,
            });
        }
    }
    let bytes_before = content.len();
    let updated = content.replacen(old_text, new_text, 1);
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
}
