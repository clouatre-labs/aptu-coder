// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Path validation helpers used by the edit_overwrite and edit_replace tool handlers.

use rmcp::model::ErrorData;
use tracing::warn;

use crate::error_meta;

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
            "path is outside the allowed root".to_string(),
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
            "path is outside the allowed root".to_string(),
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
                _ => "path is outside the allowed root".to_string(),
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
        // For non-existent files (edit_overwrite), walk up the path until we find an existing ancestor.
        // `..` components are safe here: file_name() returns None for `..`, so the
        // loop hits the else branch and resets ancestor to allowed_root, anchoring
        // the resolved path inside allowed_root.  The starts_with check below catches
        // any residual traversal regardless.
        let p = std::path::Path::new(path);
        let mut ancestor = p.to_path_buf();
        // Collect suffix components in reverse order, then reassemble without
        // join(PathBuf::new()) to avoid a trailing separator on the first push.
        let mut suffix_components: Vec<std::ffi::OsString> = Vec::new();

        loop {
            if ancestor.exists() {
                break;
            }
            if let Some(parent) = ancestor.parent()
                && let Some(file_name) = ancestor.file_name()
            {
                suffix_components.push(file_name.to_owned());
                ancestor = parent.to_path_buf();
            } else {
                // No existing ancestor found — use allowed_root as anchor
                ancestor = allowed_root.clone();
                break;
            }
        }

        // Reassemble suffix in the original (forward) order without trailing separator.
        let suffix: std::path::PathBuf = suffix_components.into_iter().rev().collect();

        let canonical_base = std::fs::canonicalize(&ancestor).unwrap_or_else(|e| {
            warn!(
                path = %ancestor.display(),
                error = %e,
                "canonicalize of existing ancestor failed (race condition); falling back to allowed_root"
            );
            allowed_root.clone()
        });
        canonical_base.join(&suffix)
    };

    if !canonical_path.starts_with(&allowed_root) {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is outside the allowed root".to_string(),
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
        std::io::ErrorKind::NotFound => format!("{path_context} not found"),
        std::io::ErrorKind::PermissionDenied => format!("permission denied: {path_context}"),
        _ => format!("{path_context} is invalid"),
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
        // For non-existent files, walk up the path until we find an existing ancestor.
        // `..` components are safe here: file_name() returns None for `..`, so the
        // loop hits the else branch and resets ancestor to PathBuf::new(), anchoring
        // the resolved path inside canonical_working_dir.  The starts_with check
        // below catches any residual traversal regardless.
        let p = std::path::Path::new(path);
        let mut ancestor = p.to_path_buf();
        // Collect suffix components in reverse order, then reassemble without
        // join(PathBuf::new()) to avoid a trailing separator on the first push.
        let mut suffix_components: Vec<std::ffi::OsString> = Vec::new();

        loop {
            let full_path = canonical_working_dir.join(&ancestor);
            if full_path.exists() {
                break;
            }
            if let Some(parent) = ancestor.parent()
                && let Some(file_name) = ancestor.file_name()
            {
                suffix_components.push(file_name.to_owned());
                ancestor = parent.to_path_buf();
            } else {
                // No existing ancestor found (or path contains `..`) --
                // use working_dir as anchor; starts_with below enforces the boundary.
                ancestor = std::path::PathBuf::new();
                break;
            }
        }

        // Reassemble suffix in the original (forward) order without trailing separator.
        let suffix: std::path::PathBuf = suffix_components.into_iter().rev().collect();

        let canonical_base = canonical_working_dir.join(&ancestor);
        let canonical_base =
            std::fs::canonicalize(&canonical_base).unwrap_or(canonical_working_dir.clone());
        canonical_base.join(&suffix)
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
