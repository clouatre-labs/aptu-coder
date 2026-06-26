// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Path validation helpers used by the edit_overwrite and edit_replace tool handlers.

use rmcp::model::ErrorData;

use crate::tools::common::error_meta;

/// Validates that the parent directory of `path` exists, is a directory,
/// and is within `root`.  Returns the resolved path (canonical_parent.join(file_name)).
fn validate_parent_in_root(
    path: &str,
    root: &std::path::Path,
) -> Result<std::path::PathBuf, ErrorData> {
    let p = std::path::Path::new(path);

    // Reject paths where file_name is None (bare '..', '.', or trailing slash).
    let file_name = p.file_name().ok_or_else(|| {
        ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path must include a filename component".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a path with a filename, not ending in '..' or '/'",
            )),
        )
    })?;

    // Extract parent; empty or '.' maps to root directly.
    // Note: if `path` is absolute, `root.join(parent)` discards `root` and returns
    // the absolute path as-is (standard Rust Path::join behaviour).  The
    // `starts_with(root)` check below then rejects it, so absolute paths that
    // escape root are handled correctly without a separate is_absolute() guard.
    let parent = p.parent().unwrap_or(std::path::Path::new(""));
    let parent_path = if parent.as_os_str().is_empty() || parent == std::path::Path::new(".") {
        root.to_path_buf()
    } else {
        root.join(parent)
    };

    // Canonicalize parent.
    let canonical_parent = std::fs::canonicalize(&parent_path).map_err(|e| {
        io_error_to_path_error(
            &e,
            parent.to_str().unwrap_or("(invalid utf-8)"),
            "provide a valid parent directory within the working directory",
        )
    })?;

    // Verify canonicalized parent is within root.
    if !canonical_parent.starts_with(root) {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is outside the working directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a path within the working directory",
            )),
        ));
    }

    // Verify canonicalized parent is a directory, not a file.
    if !std::fs::metadata(&canonical_parent)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "parent path is not a directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a path whose parent is a directory",
            )),
        ));
    }

    // Join parent with file_name to form the resolved path.
    let resolved_path = canonical_parent.join(file_name);

    // Final security check: resolved path must be within root.
    if !resolved_path.starts_with(root) {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is outside the working directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a path within the working directory",
            )),
        ));
    }

    Ok(resolved_path)
}

/// Validates that a path is within the current working directory.
/// For `require_exists=true`, the path must exist and be canonicalizable.
/// For `require_exists=false`, the parent directory must exist and be canonicalizable.
pub(crate) fn validate_path(
    path: &str,
    require_exists: bool,
) -> Result<std::path::PathBuf, ErrorData> {
    // Canonicalize the allowed root (CWD) to resolve symlinks
    let cwd = std::env::current_dir().map_err(|_| {
        ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is outside the working directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "ensure the working directory is accessible",
            )),
        )
    })?;
    let allowed_root = std::fs::canonicalize(&cwd).map_err(|_| {
        ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is outside the working directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "ensure the working directory is accessible",
            )),
        )
    })?;

    let canonical_path = if require_exists {
        std::fs::canonicalize(path).map_err(|e| {
            let msg = match e.kind() {
                std::io::ErrorKind::NotFound => "path not found".to_string(),
                std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
                _ => "path is outside the working directory".to_string(),
            };
            ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                msg,
                Some(error_meta(
                    "validation",
                    false,
                    "provide a valid path within the working directory",
                )),
            )
        })?
    } else {
        validate_parent_in_root(path, &allowed_root)?
    };

    if !canonical_path.starts_with(&allowed_root) {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is outside the working directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a path within the current working directory",
            )),
        ));
    }

    Ok(canonical_path)
}

/// Maps an io::Error to an ErrorData with kind-specific message and preserved context.
pub(crate) fn io_error_to_path_error(
    err: &std::io::Error,
    path_context: &str,
    suggested_action: &'static str,
) -> ErrorData {
    let msg = match err.kind() {
        std::io::ErrorKind::NotFound => format!("path not found: {path_context}"),
        std::io::ErrorKind::PermissionDenied => format!("permission denied: {path_context}"),
        _ => format!("path is invalid: {path_context}"),
    };
    let mut meta = error_meta("validation", false, suggested_action);
    // Preserve io::Error context in data field
    if let Some(obj) = meta.as_object_mut() {
        obj.insert(
            "ioErrorKind".to_string(),
            serde_json::json!(format!("{:?}", err.kind())),
        );
        obj.insert(
            "ioErrorSource".to_string(),
            serde_json::json!(err.to_string()),
        );
    }
    ErrorData::new(rmcp::model::ErrorCode::INVALID_PARAMS, msg, Some(meta))
}

/// Validates a path relative to a working directory.
/// The working_dir may be anywhere on disk; it is not restricted to the server CWD.
/// For `require_exists=true`, the path must exist and be canonicalizable within working_dir.
/// For `require_exists=false`, the parent directory must exist, be a directory, and be
/// within working_dir.  The filename is then appended without canonicalization.
/// The resolved path must be within the working_dir.
pub(crate) fn validate_path_in_dir(
    path: &str,
    require_exists: bool,
    working_dir: &std::path::Path,
) -> Result<std::path::PathBuf, ErrorData> {
    // Canonicalize the working_dir to resolve symlinks
    let canonical_working_dir = std::fs::canonicalize(working_dir).map_err(|e| {
        io_error_to_path_error(&e, "working_dir", "provide a valid working directory")
    })?;

    // Verify working_dir is actually a directory
    if !std::fs::metadata(&canonical_working_dir)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "working_dir must be a directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a valid directory path",
            )),
        ));
    }

    // working_dir is intentionally not restricted to the server CWD here.
    // The security boundary is the inner PathBuf::starts_with check below,
    // which ensures the resolved path cannot escape working_dir regardless
    // of where working_dir itself lives on disk.  Restricting working_dir to
    // server CWD was the original design but it prevented legitimate
    // cross-repository edits (e.g. orchestrators writing to a sibling repo)
    // while exec_command already allows arbitrary paths via `cd`.  The
    // operator sets the scope at server launch; per-call working_dir is a
    // convenience override within that operator-controlled process.

    // Now resolve the target path relative to working_dir
    let canonical_path = if require_exists {
        let target_path = canonical_working_dir.join(path);
        std::fs::canonicalize(&target_path).map_err(|e| {
            io_error_to_path_error(
                &e,
                path,
                "provide a valid path within the working directory",
            )
        })?
    } else {
        validate_parent_in_root(path, &canonical_working_dir)?
    };

    // Verify the resolved path is within working_dir.
    // PathBuf::starts_with compares path *components*, not raw bytes, so
    // a sibling directory whose name shares our prefix (e.g. "/work_evil"
    // when the allowed root is "/work") is correctly rejected -- this is
    // the exact prefix-confusion vector exploited in CVE-2025-53110 against
    // @modelcontextprotocol/server-filesystem.  Do not replace this check
    // with a string-level prefix comparison.
    if !canonical_path.starts_with(&canonical_working_dir) {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is outside the working directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a path within the working directory",
            )),
        ));
    }

    Ok(canonical_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_path_no_trailing_slash() {
        // Arrange: a multi-component path for a non-existent file
        let input = "subdir/new_file.txt";

        // Act: validate with require_exists=false
        let result = validate_path(input, false);

        // Assert: resolved path must not have a trailing slash
        // The old bug (PathBuf::from(file_name).join(&suffix) with an empty
        // PathBuf as the initial suffix) injected a trailing separator,
        // producing ".../subdir/new_file.txt/" instead of
        // ".../subdir/new_file.txt".
        if let Ok(resolved) = result {
            let path_str = resolved.to_string_lossy();
            // PathBuf::to_string_lossy surrogates the trailing separator as "",
            // but the canonical representation still carries it.  Check both.
            assert!(
                !path_str.ends_with('/'),
                "resolved path must not end with trailing slash: {path_str}"
            );
            assert_eq!(
                resolved.extension(),
                Some(std::ffi::OsStr::new("txt")),
                "file extension should be txt, path has trailing separator"
            );
        }
    }
}
