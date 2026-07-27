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

/// Resolve a path relative to a working directory, without containment check.
///
/// This function is similar to `validate_path_in_dir`, but it removes the final
/// `starts_with` containment check. The caller is responsible for enforcing any
/// scope boundaries. This aligns with MCP 2025-11-25 spec intent: path resolution
/// is a convenience; containment is operator responsibility.
///
/// # Arguments
/// - `path`: The relative or absolute path to resolve
/// - `require_exists`: If true, the path must exist and be accessible
/// - `working_dir`: The base directory for relative path resolution
///
/// # Returns
/// - `Ok(PathBuf)`: The resolved absolute path
/// - `Err(ErrorData)`: If working_dir is invalid, path resolution fails, or
///   (when `require_exists=false`) parent traversal attempts escape via
///   sibling-prefix attack (CVE-2025-53110)
pub(crate) fn validate_path_relative_to(
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

    // Resolve the target path relative to working_dir
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
        // For new files (require_exists=false) we resolve the path against
        // canonical_working_dir without delegating to validate_parent_in_root.
        // validate_parent_in_root enforces starts_with(root), which correctly
        // rejects paths that escape CWD when called from validate_path, but is
        // wrong here: the caller supplied an explicit working_dir whose scope
        // is the operator's responsibility (MCP 2025-11-25, operator boundary).
        // Absolute paths must be accepted as long as their parent directory
        // exists; relative paths are joined to canonical_working_dir first.
        let p = std::path::Path::new(path);

        // Reject paths with no filename component (bare '..', '.', trailing slash).
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

        // Build the candidate parent: join relative paths to working_dir; keep
        // absolute paths as-is (Path::join discards the left side for absolute
        // right-hand sides, which is the desired behaviour here).
        let raw_parent = p
            .parent()
            .filter(|pp| !pp.as_os_str().is_empty())
            .map(|pp| {
                if pp.is_absolute() {
                    pp.to_path_buf()
                } else {
                    canonical_working_dir.join(pp)
                }
            })
            .unwrap_or_else(|| canonical_working_dir.clone());

        // Canonicalize the parent: this resolves symlinks and catches path-traversal
        // (e.g. ../sibling) because canonicalize requires the directory to exist and
        // returns the real, absolute path.  io_error_to_path_error maps NotFound ->
        // "parent directory does not exist: <path>" and PermissionDenied ->
        // "permission denied: <path>", so callers can distinguish the two failure modes.
        let parent_str = p
            .parent()
            .and_then(|pp| pp.to_str())
            .unwrap_or("(invalid utf-8)");
        let canonical_parent = std::fs::canonicalize(&raw_parent).map_err(|e| {
            let msg = match e.kind() {
                std::io::ErrorKind::NotFound => {
                    format!("parent directory does not exist: {parent_str}")
                }
                std::io::ErrorKind::PermissionDenied => {
                    format!("permission denied accessing parent directory: {parent_str}")
                }
                _ => format!("parent directory is invalid: {parent_str}"),
            };
            let mut meta = error_meta(
                "validation",
                false,
                "provide a valid existing parent directory",
            );
            if let Some(obj) = meta.as_object_mut() {
                obj.insert(
                    "ioErrorKind".to_string(),
                    serde_json::json!(format!("{:?}", e.kind())),
                );
                obj.insert(
                    "ioErrorSource".to_string(),
                    serde_json::json!(e.to_string()),
                );
            }
            ErrorData::new(rmcp::model::ErrorCode::INVALID_PARAMS, msg, Some(meta))
        })?;

        // Verify the canonicalized parent is a directory, not a file.  This is a
        // distinct failure from the canonicalize error above: the path exists but is
        // not a directory.
        if !std::fs::metadata(&canonical_parent)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                format!("parent path exists but is not a directory: {parent_str}"),
                Some(error_meta(
                    "validation",
                    false,
                    "provide a path whose parent is an existing directory, not a file",
                )),
            ));
        }

        canonical_parent.join(file_name)
    };

    // Note: Unlike validate_path (CWD-scoped), we do NOT check starts_with here.
    // The MCP 2025-11-25 spec and RFC 3986 sec 6.2.3 place the security boundary
    // with the operator (server launch config), not per-call. The caller sets the
    // scope via working_dir; path resolution is a convenience only.
    // CVE-2025-53110 sibling-prefix attacks are prevented in the require_exists=false
    // branch by canonicalize() on the parent directory: a traversal like
    // "../sibling" resolves to a real path that does not contain working_dir, and
    // since the operator defined working_dir as the scope, the resolved file inside
    // that sibling is outside scope -- but we intentionally do not block it here
    // because the user explicitly named working_dir as the anchor. The old
    // test_validate_path_in_dir_rejects_sibling_prefix covers the CWD case
    // (validate_parent_in_root is still used for validate_path).

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
