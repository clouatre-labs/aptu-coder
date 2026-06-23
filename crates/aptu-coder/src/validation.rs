// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Path validation helpers used by the edit_overwrite and edit_replace tool handlers.

use rmcp::model::ErrorData;

use crate::error_meta;

/// Scans a shell command string for unclosed heredocs and file-write heredoc
/// patterns before any process is spawned.
///
/// Phase 1 pre-scan: walks the command byte-by-byte with quote tracking.  When a
/// `<<` token is found outside any quoted region, it scans backward to check for
/// file-write patterns (cat/tee/redirect + `<<`) and returns an error immediately
/// if one is detected.
///
/// Phase 2 main scan: continues the existing matching-closer scan (unchanged logic)
/// with quote state reset to initial values between phases.
///
/// Returns `Ok(())` if no file-write patterns or unclosed heredocs are found,
/// or `Err(ErrorData)` with `INVALID_PARAMS` otherwise.
///
/// This function does NOT spawn any process; it is a pure string scan.
pub(crate) fn validate_heredocs(command: &str) -> Result<(), ErrorData> {
    let bytes = command.as_bytes();
    let len = bytes.len();

    // Phase 1: pre-scan for heredoc file-write patterns (redirect + <<)
    //
    // Supported patterns (all rejected):
    //   - cat > file << EOF
    //   - cat >> file << EOF
    //   - tee file << EOF, tee -a file << EOF
    //   - tee > file << EOF, tee >> file << EOF
    //   - printf 'content' > file << EOF
    //   - dd of=file << EOF
    //   - install file << EOF
    //   - cp /dev/stdin file << EOF
    //   - mv /dev/stdin file << EOF
    //   - $VAR > file << EOF (variable-expanded command name)
    //   - > file << EOF, >> file << EOF (bare redirect)
    //   - (cat > file << EOF) (subshell grouping)
    //   - cat > >(proc) << EOF (process substitution in file position)
    //   - cat > $(cmd) << EOF (command substitution in file position)
    //
    // Unsupported (NOT rejected -- heuristic misses complex nesting):
    //   - cat > "${var}" << EOF (variable expansion in file path)
    //   - exec 3<>file; cat >&3 << EOF (arbitrary fd redirects)
    //   - deeply nested subshells with multiple heredoc levels
    {
        let mut i = 0;
        let mut in_single_quote = false;
        let mut in_double_quote = false;

        while i < len {
            let ch = bytes[i] as char;

            if ch == '\'' && !in_double_quote {
                in_single_quote = !in_single_quote;
                i += 1;
                continue;
            }
            if ch == '"' && !in_single_quote {
                in_double_quote = !in_double_quote;
                i += 1;
                continue;
            }
            if in_single_quote || in_double_quote {
                i += 1;
                continue;
            }

            // Found `<<` outside quotes -- check for file-write pattern via
            // backward scan from the first `<`.
            if ch == '<' && i + 1 < len && bytes[i + 1] == b'<' {
                if scan_backward_for_file_write(bytes, i) {
                    return Err(file_write_heredoc_error());
                }
                // Skip past `<<` to avoid re-scanning the same token.
                // Use a simple increment; the main scan loop below handles
                // the full delimiter parsing.
                i += 2;
                continue;
            }
            i += 1;
        }
    }

    // Phase 2: original heredoc delimiter scan (quote state is fresh)
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < len {
        let ch = bytes[i] as char;

        // Single-quote regions: no escaping inside; toggle on every `'`.
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            i += 1;
            continue;
        }

        // Double-quote regions: toggle on unescaped `"` outside single quotes.
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }

        // Inside any quoted region, skip everything (including `<<` tokens).
        if in_single_quote || in_double_quote {
            i += 1;
            continue;
        }

        // Look for `<<` token
        if ch == '<' && i + 1 < len && bytes[i + 1] == b'<' {
            let _here_start = i;
            i += 2;

            let strip_tabs = if i < len && bytes[i] == b'-' {
                i += 1;
                true
            } else {
                false
            };

            // Skip whitespace before delimiter
            while i < len && (bytes[i] as char).is_ascii_whitespace() {
                i += 1;
            }

            if i >= len {
                return Err(missing_heredoc_error());
            }

            // Extract the delimiter word, stripping quotes
            let delimiter = if bytes[i] == b'\'' {
                // Single-quoted delimiter: <<'EOF'
                i += 1;
                let start = i;
                while i < len && bytes[i] != b'\'' {
                    i += 1;
                }
                if i >= len {
                    return Err(missing_heredoc_error());
                }
                let word = &command[start..i];
                i += 1; // skip closing quote
                word.to_string()
            } else if bytes[i] == b'"' {
                // Double-quoted delimiter: <<"EOF"
                i += 1;
                let start = i;
                while i < len && bytes[i] != b'"' {
                    i += 1;
                }
                if i >= len {
                    return Err(missing_heredoc_error());
                }
                let word = &command[start..i];
                i += 1; // skip closing quote
                word.to_string()
            } else if bytes[i] == b'\\' {
                // Escaped delimiter: <<\EOF
                i += 1;
                let start = i;
                while i < len && !(bytes[i] as char).is_ascii_whitespace() && bytes[i] != b'<' {
                    i += 1;
                }
                command[start..i].to_string()
            } else {
                // Bare delimiter: <<EOF
                let start = i;
                while i < len && !(bytes[i] as char).is_ascii_whitespace() && bytes[i] != b'<' {
                    i += 1;
                }
                command[start..i].to_string()
            };

            if delimiter.is_empty() {
                return Err(missing_heredoc_error());
            }

            // Search the remainder of the command string for a line matching the
            // closing delimiter.  Walk line-by-line from after the `<<` token,
            // looking for a line that consists of exactly the delimiter word and
            // nothing else.
            //
            // POSIX rule: the closing delimiter must appear alone on its line
            // with no leading whitespace (for `<<`) or only leading tabs (for
            // `<<-`), and no trailing whitespace or comments.  Using a bare
            // `.trim()` would cause false negatives (accepting `EOF ` or
            // `  EOF` as closers when the shell does not) so we compare the
            // stripped line to the delimiter with an exact equality check.
            //
            // Using split_inclusive('\n') avoids manual index arithmetic and
            // eliminates any off-by-one risk on the final line: the iterator
            // yields every line including the terminating '\n' when present, and
            // the last segment (no trailing newline) is yielded as-is.
            let mut found = false;
            let rest = &command[i..];
            let mut consumed = i;

            for raw_line in rest.split_inclusive('\n') {
                // Strip the line terminator only; preserve all other whitespace
                // so the comparison is exact.
                let line = raw_line.trim_end_matches('\n');
                let candidate = if strip_tabs {
                    // <<-: strip leading tabs only (POSIX; spaces are NOT stripped)
                    line.trim_start_matches('\t')
                } else {
                    line
                };

                if candidate == delimiter {
                    found = true;
                    i = consumed + raw_line.len();
                    break;
                }

                consumed += raw_line.len();
            }

            if !found {
                return Err(missing_heredoc_error());
            }
        } else {
            i += 1;
        }
    }

    Ok(())
}

fn scan_backward_for_file_write(bytes: &[u8], here_pos: usize) -> bool {
    /// Checks whether the token before `pos` (walked backward to preceding
    /// whitespace or command separator) is returned as a byte slice.
    /// Stops at command separators: whitespace, `(`, `|`, `;`, `&`.
    fn token_before_pos<'a>(bytes: &'a [u8], pos: &mut usize) -> &'a [u8] {
        let end = *pos;
        while *pos > 0 {
            let b = bytes[*pos - 1];
            if b.is_ascii_whitespace() || b == b'(' || b == b'|' || b == b';' || b == b'&' {
                break;
            }
            *pos -= 1;
        }
        &bytes[*pos..end]
    }

    /// Extracts a token backward from `pos` that may contain paren-grouped
    /// constructs such as `$(...)`, `>(...)`, `<(...)`.  When encountering `)`
    /// the helper enters paren-tracking mode and skips backward through the
    /// group (including interior whitespace) until the matching `(` is found.
    /// If `(` is preceded by `$`, `>`, or `<`, the whole construct is consumed
    /// as part of the token.
    fn paren_aware_token<'a>(bytes: &'a [u8], pos: &mut usize) -> &'a [u8] {
        let end = *pos;
        let mut depth: i32 = 0;

        while *pos > 0 {
            let b = bytes[*pos - 1];
            if b == b')' {
                depth += 1;
                *pos -= 1;
            } else if b == b'(' {
                depth -= 1;
                *pos -= 1;
                if depth == 0 {
                    // Check if preceded by $, >, or < (nested-context marker)
                    if *pos > 0 && matches!(bytes[*pos - 1], b'$' | b'>' | b'<') {
                        *pos -= 1;
                    }
                    // Continue scanning; the paren group plus any surrounding
                    // characters are part of a single token-like unit.
                    continue;
                }
            } else if depth > 0 {
                // Inside parens -- skip interior whitespace
                *pos -= 1;
            } else if b.is_ascii_whitespace() || b == b'(' || b == b'|' || b == b';' || b == b'&' {
                break;
            } else {
                *pos -= 1;
            }
        }
        &bytes[*pos..end]
    }

    /// Skips whitespace scanning backward from `pos`.
    fn skip_ws_backward(bytes: &[u8], pos: &mut usize) {
        while *pos > 0 && (bytes[*pos - 1] as char).is_ascii_whitespace() {
            *pos -= 1;
        }
    }

    /// Returns true if `cmd` is a known file-write command or a
    /// variable-expanded command name (starts with `$`).
    fn is_file_write_command(cmd: &[u8]) -> bool {
        cmd == b"cat"
            || cmd == b"tee"
            || cmd == b"printf"
            || cmd == b"dd"
            || cmd == b"install"
            || cmd == b"cp"
            || cmd == b"mv"
            || cmd.first() == Some(&b'$')
    }

    // here_pos points to the first '<' of '<<'.  Walk backward looking for
    // a file-write pattern (redirect + >? file + <<).

    let mut pos = here_pos;

    // Skip whitespace between << and the preceding word
    skip_ws_backward(bytes, &mut pos);
    if pos == 0 {
        return false;
    }

    // Find the file path token immediately before <<.
    // Use paren_aware_token to handle `$(cmd)`, `>(proc)`, `<(...)` in file position.
    let file_token = paren_aware_token(bytes, &mut pos);
    if file_token.is_empty() {
        return false;
    }

    // Skip whitespace before the file token
    skip_ws_backward(bytes, &mut pos);
    if pos == 0 {
        return false;
    }

    // Check for >> or > redirect operator before the file token
    if pos >= 2 && bytes[pos - 1] == b'>' && bytes[pos - 2] == b'>' {
        // >> append-redirect operator
        pos -= 2;
        skip_ws_backward(bytes, &mut pos);
        if pos == 0 {
            // Bare >> file << EOF -- no command before redirect
            return true;
        }
        // Walk backward through all arguments (same logic as > branch below).
        loop {
            let tok = token_before_pos(bytes, &mut pos);
            if tok.is_empty() {
                return false;
            }
            if is_file_write_command(tok) {
                return true;
            }
            skip_ws_backward(bytes, &mut pos);
            if pos == 0 {
                return false;
            }
            let next = bytes[pos - 1];
            if next == b'|' || next == b';' || next == b'&' || next == b'(' {
                return false;
            }
        }
    }

    if bytes[pos - 1] == b'>' {
        // > write-redirect operator
        pos -= 1;
        skip_ws_backward(bytes, &mut pos);
        if pos == 0 {
            // Bare > file << EOF -- no command before redirect
            return true;
        }
        // Walk backward through all arguments until we reach a command
        // separator or the start.  This handles patterns like:
        //   printf '%s\n' hello > file << EOF
        // where multiple arguments appear between the command and the redirect.
        loop {
            let tok = token_before_pos(bytes, &mut pos);
            if tok.is_empty() {
                return false;
            }
            if is_file_write_command(tok) {
                return true;
            }
            // If this token looks like an argument (not a separator already
            // consumed by token_before_pos), keep scanning backward.
            skip_ws_backward(bytes, &mut pos);
            if pos == 0 {
                return false;
            }
            // Stop at command-separating characters that token_before_pos
            // leaves in place: |, ;, &, (.
            let next = bytes[pos - 1];
            if next == b'|' || next == b';' || next == b'&' || next == b'(' {
                return false;
            }
        }
    }

    // No redirect operator -- check for tee, dd (dd of=file << EOF), install etc.
    let cmd = token_before_pos(bytes, &mut pos);
    if is_file_write_command(cmd) {
        return true;
    }

    // Check if the previous token is a flag (starts with '-') for
    // patterns like `tee -a file << EOF` or `install -m 644 file << EOF`
    if cmd.len() > 1 && cmd[0] == b'-' {
        skip_ws_backward(bytes, &mut pos);
        if pos == 0 {
            return false;
        }
        let prev_cmd = token_before_pos(bytes, &mut pos);
        return is_file_write_command(prev_cmd);
    }

    false
}

fn file_write_heredoc_error() -> ErrorData {
    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "heredoc file-write pattern detected (cat/tee/redirect + <<) -- use edit_overwrite to write files instead of shell heredocs".to_string(),
        Some(error_meta("validation", false, "use edit_overwrite to write files")),
    )
}

fn missing_heredoc_error() -> ErrorData {
    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "heredoc closing delimiter not found -- likely a quoting or escaping issue; use edit_overwrite to write files instead of shell heredocs".to_string(),
        Some(error_meta("validation", false, "use edit_overwrite to write files")),
    )
}

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

    // -----------------------------------------------------------------------
    // Unit tests for scan_backward_for_file_write edge cases.
    // The inner helpers (token_before_pos, skip_ws_backward) are tested
    // indirectly via scan_backward_for_file_write with targeted inputs.
    // -----------------------------------------------------------------------

    #[test]
    fn scan_backward_empty_input_not_file_write() {
        // here_pos == 0: nothing before <<, must not panic and must return false.
        assert!(!scan_backward_for_file_write(b"", 0));
    }

    #[test]
    fn scan_backward_only_whitespace_before_heredoc_not_file_write() {
        // All whitespace before <<: "   <<"
        // token_before_pos returns an empty slice, skip_ws_backward leaves pos==0.
        let cmd = b"   <<";
        assert!(!scan_backward_for_file_write(cmd, 3));
    }

    #[test]
    fn scan_backward_leading_whitespace_file_token_then_redirect() {
        // "  cat > file <<" -- whitespace at start of string, cat before redirect.
        let cmd = b"  cat > file <<";
        assert!(scan_backward_for_file_write(cmd, 13));
    }

    #[test]
    fn scan_backward_file_token_only_no_redirect_no_tee_not_file_write() {
        // "somecmd file <<" -- neither cat/tee nor a redirect; must return false.
        let cmd = b"somecmd file <<";
        assert!(!scan_backward_for_file_write(cmd, 13));
    }
}
