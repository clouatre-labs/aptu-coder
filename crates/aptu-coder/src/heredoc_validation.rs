// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Heredoc validation: pre-spawn guard logic extracted from `shell_write.rs`.
//!
//! Validates shell heredoc patterns for file-write and stdin conflicts before
//! any process is spawned.

use rmcp::model::ErrorData;

use crate::shell_scan::{scan_backward_for_file_write, scan_backward_for_stdin_flag};
use crate::tools::common::error_meta;

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
pub(crate) fn validate_heredocs(command: &str, has_stdin: bool) -> Result<(), ErrorData> {
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
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut i = 0usize;

        while i < len {
            let ch = bytes[i] as char;

            // Backslash escapes: outside single-quote regions, a backslash escapes
            // the next character so an escaped quote does not toggle quote state.
            // Inside single quotes, backslash has no special meaning in POSIX sh.
            if ch == '\\' && !in_single_quote {
                i += 2; // skip the escaped character
                continue;
            }

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
                // Scan backward from i to check for file-write pattern.
                if scan_backward_for_file_write(bytes, i) {
                    return Err(file_write_heredoc_error());
                }
                if scan_backward_for_stdin_flag(bytes, i) {
                    return Err(stdin_flag_heredoc_error());
                }
                if has_stdin {
                    return Err(stdin_param_heredoc_error());
                }
                i += 2;
                continue;
            }

            i += 1;
        }
    }

    // Phase 2: main heredoc closer scan (existing logic, quote state reset)
    //
    // Track opening heredocs and their delimiters; ensure each one is properly
    // closed.
    {
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut i = 0usize;

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
                let mut found = false;
                let rest = &command[i..];
                let mut consumed = i;

                for raw_line in rest.split_inclusive('\n') {
                    let line = raw_line.trim_end_matches('\n');
                    let candidate = if strip_tabs {
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
    }

    Ok(())
}

fn stdin_flag_heredoc_error() -> ErrorData {
    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "stdin-consuming flag with heredoc detected (--body-file -, --data -, etc.) -- pass content via the `stdin` parameter instead, or write to a file first with edit_overwrite".to_string(),
        Some(error_meta("validation", false, "use the stdin parameter instead of heredoc + stdin-consuming flags")),
    )
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

fn stdin_param_heredoc_error() -> ErrorData {
    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "stdin parameter and heredoc cannot be used together -- pass content via the `stdin` parameter instead".to_string(),
        Some(error_meta("validation", false, "use the stdin parameter instead of a heredoc")),
    )
}

#[cfg(test)]
mod tests {
    use super::validate_heredocs;

    #[test]
    fn validate_heredocs_simple_heredoc_ok() {
        // happy_path: simple <<EOF with proper closing
        let cmd = "cat <<EOF\nhello\nEOF\n";
        assert!(validate_heredocs(cmd, false).is_ok());
    }

    #[test]
    fn validate_heredocs_file_write_pattern_returns_error() {
        // edge_case: file-write pattern (tee file <<EOF)
        let cmd = "tee output.txt <<EOF\ncontent\nEOF\n";
        let err = validate_heredocs(cmd, false);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err().message);
        assert!(msg.contains("heredoc file-write pattern"));
    }

    #[test]
    fn validate_heredocs_stdin_flag_returns_error() {
        // edge_case: stdin-consuming flag (--data - <<EOF)
        let cmd = "curl --data - <<EOF\ncontent\nEOF\n";
        let err = validate_heredocs(cmd, false);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err().message);
        assert!(msg.contains("stdin-consuming flag"));
    }

    #[test]
    fn validate_heredocs_stdin_param_returns_error() {
        // edge_case: stdin param set with heredoc
        let cmd = "cat <<EOF\ncontent\nEOF\n";
        let err = validate_heredocs(cmd, true);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err().message);
        assert!(msg.contains("stdin parameter and heredoc"));
    }

    #[test]
    fn validate_heredocs_empty_command_ok() {
        // happy_path: empty command string returns Ok
        assert!(validate_heredocs("", false).is_ok());
    }

    #[test]
    fn validate_heredocs_no_heredoc_ok() {
        // happy_path: command without any << passes
        assert!(validate_heredocs("echo hello", false).is_ok());
        assert!(validate_heredocs("ls -la", false).is_ok());
    }
}
