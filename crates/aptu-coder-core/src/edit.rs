// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! File write utilities for the `edit_overwrite`, `edit_replace`, `edit_rename`, and `edit_insert` tools.

use crate::types::{
    EditInsertOutput, EditOverwriteOutput, EditRenameOutput, EditReplaceOutput, FileRenameError,
    FileRenameResult, InsertPosition,
};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use thiserror::Error;

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
    NotFound { path: String },
    #[error(
        "old_text appears {count} times in {path} — make old_text longer and more specific to uniquely identify the block"
    )]
    Ambiguous { count: usize, path: String },
    #[error("symbol '{name}' not found in {path}")]
    SymbolNotFound { name: String, path: String },
    #[error("symbol '{name}' matches multiple node kinds in {path} — supply kind to disambiguate")]
    AmbiguousKind {
        name: String,
        kinds: Vec<String>,
        path: String,
    },
    #[error("unsupported file extension for AST operations: {0}")]
    UnsupportedLanguage(String),
    #[error(
        "kind filtering is not supported with the current identifier query infrastructure; omit kind to rename all occurrences"
    )]
    KindFilterUnsupported,
}

const IDENTIFIER_QUERY: &str = "(identifier) @name";

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
            return Err(EditError::NotFound {
                path: path.display().to_string(),
            });
        }
        1 => {}
        n => {
            return Err(EditError::Ambiguous {
                count: n,
                path: path.display().to_string(),
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

pub fn edit_rename_in_file(
    path: &Path,
    old_name: &str,
    new_name: &str,
    kind: Option<&str>,
) -> Result<EditRenameOutput, EditError> {
    if kind.is_some() {
        return Err(EditError::KindFilterUnsupported);
    }

    if path.is_dir() {
        return Err(EditError::NotAFile(path.to_path_buf()));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| EditError::UnsupportedLanguage("no extension".to_string()))?;

    let language = crate::lang::language_for_extension(ext)
        .ok_or_else(|| EditError::UnsupportedLanguage(ext.to_string()))?;

    let source = std::fs::read_to_string(path)?;

    let captures = crate::execute_query(language, &source, IDENTIFIER_QUERY)
        .map_err(|_| EditError::UnsupportedLanguage(language.to_string()))?;

    let matching_captures: Vec<_> = captures.iter().filter(|c| c.text == old_name).collect();

    if matching_captures.is_empty() {
        return Err(EditError::SymbolNotFound {
            name: old_name.to_string(),
            path: path.display().to_string(),
        });
    }

    let mut bytes: Vec<u8> = source.into_bytes();
    let mut sorted_captures = matching_captures.clone();
    sorted_captures.sort_by_key(|b| std::cmp::Reverse(b.start_byte));

    for capture in sorted_captures {
        let start = capture.start_byte;
        let end = capture.end_byte;
        bytes.splice(start..end, new_name.bytes());
    }

    let updated = String::from_utf8(bytes).map_err(|_| {
        EditError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid UTF-8 after rename",
        ))
    })?;

    std::fs::write(path, &updated)?;

    Ok(EditRenameOutput {
        path: path.display().to_string(),
        old_name: old_name.to_string(),
        new_name: new_name.to_string(),
        occurrences_renamed: matching_captures.len(),
        files_changed: None,
        errors: None,
    })
}

pub fn edit_rename_directory(
    root: &Path,
    old_name: &str,
    new_name: &str,
    kind: Option<&str>,
) -> Result<(Vec<FileRenameResult>, Vec<FileRenameError>), EditError> {
    if kind.is_some() {
        return Err(EditError::KindFilterUnsupported);
    }

    let entries = crate::traversal::walk_directory(root, None).map_err(|e| {
        EditError::Io(std::io::Error::other(format!(
            "directory traversal failed: {}",
            e
        )))
    })?;

    let mut results = Vec::new();
    let mut errors = Vec::new();

    for entry in entries {
        if entry.is_dir {
            continue;
        }

        let ext = match entry.path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };

        if crate::lang::language_for_extension(ext).is_none() {
            continue;
        }

        match edit_rename_in_file(&entry.path, old_name, new_name, None) {
            Ok(output) => {
                if output.occurrences_renamed > 0 {
                    results.push(FileRenameResult {
                        path: entry.path.display().to_string(),
                        occurrences_renamed: output.occurrences_renamed,
                    });
                }
            }
            Err(e) => {
                errors.push(FileRenameError {
                    path: entry.path.display().to_string(),
                    error: e.to_string(),
                });
            }
        }
    }

    Ok((results, errors))
}

pub fn edit_insert_at_symbol(
    path: &Path,
    symbol_name: &str,
    position: InsertPosition,
    content: &str,
) -> Result<EditInsertOutput, EditError> {
    if path.is_dir() {
        return Err(EditError::NotAFile(path.to_path_buf()));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| EditError::UnsupportedLanguage("no extension".to_string()))?;

    let language = crate::lang::language_for_extension(ext)
        .ok_or_else(|| EditError::UnsupportedLanguage(ext.to_string()))?;

    let source = std::fs::read_to_string(path)?;

    let captures = crate::execute_query(language, &source, IDENTIFIER_QUERY)
        .map_err(|_| EditError::UnsupportedLanguage(language.to_string()))?;

    let target = captures
        .iter()
        .find(|c| c.text == symbol_name)
        .ok_or_else(|| EditError::SymbolNotFound {
            name: symbol_name.to_string(),
            path: path.display().to_string(),
        })?;

    let byte_offset = match position {
        InsertPosition::Before => target.start_byte,
        InsertPosition::After => target.end_byte,
    };

    let mut bytes: Vec<u8> = source.into_bytes();
    bytes.splice(byte_offset..byte_offset, content.bytes());

    let updated = String::from_utf8(bytes).map_err(|_| {
        EditError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid UTF-8 after insert",
        ))
    })?;

    std::fs::write(path, &updated)?;

    let position_str = match position {
        InsertPosition::Before => "before",
        InsertPosition::After => "after",
    };

    Ok(EditInsertOutput {
        path: path.display().to_string(),
        symbol_name: symbol_name.to_string(),
        position: position_str.to_string(),
        byte_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_temp_file(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "{}", content).unwrap();
        f
    }

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
        assert!(matches!(err, EditError::NotAFile(_)));
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
        assert!(matches!(err, EditError::NotFound { .. }));
    }

    #[test]
    fn edit_replace_block_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "foo foo baz").unwrap();
        let err = edit_replace_block(&path, "foo", "x").unwrap_err();
        assert!(matches!(err, EditError::Ambiguous { count: 2, .. }));
    }

    #[test]
    fn edit_replace_block_directory_guard() {
        let dir = tempfile::tempdir().unwrap();
        let err = edit_replace_block(dir.path(), "old", "new").unwrap_err();
        assert!(matches!(err, EditError::NotAFile(_)));
    }

    fn write_temp(content: &str, ext: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(ext).tempfile().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn edit_rename_in_file_renames_identifier_not_comment() {
        let src = "fn foo() {}\n// foo is a function\n";
        let f = write_temp(src, ".rs");
        let out = edit_rename_in_file(f.path(), "foo", "bar", None).unwrap();
        assert_eq!(out.occurrences_renamed, 1);
        let updated = std::fs::read_to_string(f.path()).unwrap();
        assert!(updated.contains("fn bar()"));
        assert!(updated.contains("// foo is a function"));
    }

    #[test]
    fn edit_rename_in_file_not_found_error() {
        let f = write_temp("fn foo() {}\n", ".rs");
        let err = edit_rename_in_file(f.path(), "missing", "bar", None).unwrap_err();
        assert!(matches!(err, EditError::SymbolNotFound { .. }));
    }

    #[test]
    fn edit_rename_in_file_kind_returns_kind_filter_unsupported() {
        let f = write_temp("fn foo() {}\n", ".rs");
        let err = edit_rename_in_file(f.path(), "foo", "bar", Some("function")).unwrap_err();
        assert!(matches!(err, EditError::KindFilterUnsupported));
    }

    #[test]
    fn edit_rename_in_file_unsupported_extension() {
        let f = write_temp("foo bar\n", ".txt");
        let err = edit_rename_in_file(f.path(), "foo", "bar", None).unwrap_err();
        assert!(matches!(err, EditError::UnsupportedLanguage(_)));
    }

    #[test]
    fn edit_insert_at_symbol_before() {
        let src = "fn foo() {}\n";
        let f = write_temp(src, ".rs");
        let out = edit_insert_at_symbol(f.path(), "foo", InsertPosition::Before, "bar_").unwrap();
        let updated = std::fs::read_to_string(f.path()).unwrap();
        assert!(updated.contains("fn bar_foo()"));
        assert_eq!(out.position, "before");
    }

    #[test]
    fn edit_insert_at_symbol_after() {
        let src = "fn foo() {}\n";
        let f = write_temp(src, ".rs");
        let out =
            edit_insert_at_symbol(f.path(), "foo", InsertPosition::After, "_renamed").unwrap();
        let updated = std::fs::read_to_string(f.path()).unwrap();
        assert!(updated.contains("fn foo_renamed()"));
        assert_eq!(out.position, "after");
    }

    #[test]
    fn edit_insert_at_symbol_not_found_error() {
        let f = write_temp("fn foo() {}\n", ".rs");
        let err =
            edit_insert_at_symbol(f.path(), "missing", InsertPosition::Before, "x").unwrap_err();
        assert!(matches!(err, EditError::SymbolNotFound { .. }));
    }

    #[test]
    fn edit_insert_at_symbol_unsupported_extension() {
        let f = write_temp("foo bar\n", ".txt");
        let err = edit_insert_at_symbol(f.path(), "foo", InsertPosition::Before, "x").unwrap_err();
        assert!(matches!(err, EditError::UnsupportedLanguage(_)));
    }

    #[test]
    fn edit_rename_directory_multi_file() {
        let dir = tempfile::tempdir().unwrap();
        let file1 = dir.path().join("file1.rs");
        let file2 = dir.path().join("file2.rs");
        std::fs::write(&file1, "fn foo() {}\n").unwrap();
        std::fs::write(&file2, "fn foo() { foo(); }\n").unwrap();

        let (results, errors) = edit_rename_directory(dir.path(), "foo", "bar", None).unwrap();

        assert_eq!(errors.len(), 0);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].occurrences_renamed, 1);
        assert_eq!(results[1].occurrences_renamed, 2);

        let content1 = std::fs::read_to_string(&file1).unwrap();
        let content2 = std::fs::read_to_string(&file2).unwrap();
        assert!(content1.contains("fn bar()"));
        assert!(content2.contains("fn bar() { bar(); }"));
    }

    #[test]
    fn edit_rename_directory_partial_failure() {
        let dir = tempfile::tempdir().unwrap();
        let file1 = dir.path().join("file1.rs");
        let file2 = dir.path().join("file2.rs");
        std::fs::write(&file1, "fn foo() {}\n").unwrap();
        std::fs::write(&file2, "fn foo() {}\n").unwrap();

        let (results, errors) = edit_rename_directory(dir.path(), "foo", "bar", None).unwrap();

        assert_eq!(errors.len(), 0);
        assert_eq!(results.len(), 2);

        let content1 = std::fs::read_to_string(&file1).unwrap();
        let content2 = std::fs::read_to_string(&file2).unwrap();
        assert!(content1.contains("fn bar()"));
        assert!(content2.contains("fn bar()"));
    }
}
