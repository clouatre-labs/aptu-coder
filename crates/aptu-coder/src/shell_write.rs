// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Shell write validation helpers: heredoc and file-write pattern detection.
//!
//! Extracted from `validation.rs` (F5, issue #1221).  This module owns the
//! pre-spawn exec_command guard; `validation.rs` retains path-safety helpers
//! used by edit_overwrite and edit_replace.

use rmcp::model::ErrorData;

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
    }

    Ok(())
}

/// Scans backward from `here_pos` (the index of the first `<` in `<<`) to
/// detect file-write heredoc patterns such as `cat > file <<` or `tee file <<`.
///
/// Returns `true` if a file-write pattern is found, `false` otherwise.
pub(crate) fn scan_backward_for_file_write(bytes: &[u8], here_pos: usize) -> bool {
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
    ///
    /// NOTE: escaped parentheses (`\(`, `\)`) are not handled.  The heredoc
    /// security gate operates on raw shell command strings before any
    /// evaluation, so escape sequences at this level are vanishingly rare in
    /// practice.  If that assumption ever changes, this function will need a
    /// backslash-lookahead before decrementing `depth`.
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
                    // Continue scanning backward after reaching depth==0 because
                    // chained constructs like $(cmd), >(proc) may be followed by
                    // more backward tokens within the same argument.
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

    /// Returns true if `cmd` is a known stdin-consuming command that
    /// accepts heredoc data and writes to a file specified by `>` or `>>`.
    ///
    /// Only commands that read from stdin are listed here because the
    /// file-write heredoc guard rejects patterns like `cmd > file << EOF`
    /// where `EOF` gets written to `file` instead of being passed to `cmd`.
    /// Commands like `cp`, `mv`, and `install` read named file arguments,
    /// not stdin, so they are excluded -- `cp > file << EOF` does not make
    /// sense as a heredoc file-write pattern.
    ///
    /// Variable-expanded command names (starting with `$`) are included as
    /// a catch-all for dynamic commands that may consume stdin.
    fn is_file_write_command(cmd: &[u8]) -> bool {
        cmd == b"cat"
            || cmd == b"tee"
            || cmd == b"printf"
            || cmd == b"dd"
            || cmd.first() == Some(&b'$')
    }

    /// Walks backward from `pos` through argument tokens, calling
    /// `is_file_write_command` on each.  Returns true if a write command
    /// is found before hitting a command separator, start of string, or
    /// an empty token.  Used by both `>>` and `>` redirect branches to
    /// avoid duplicating the scan logic.
    fn scan_args_for_write_command(bytes: &[u8], pos: &mut usize) -> bool {
        loop {
            let tok = token_before_pos(bytes, pos);
            if tok.is_empty() {
                return false;
            }
            if is_file_write_command(tok) {
                return true;
            }
            skip_ws_backward(bytes, pos);
            if *pos == 0 {
                return false;
            }
            let next = bytes[*pos - 1];
            if next == b'|' || next == b';' || next == b'&' || next == b'(' {
                return false;
            }
        }
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
        if scan_args_for_write_command(bytes, &mut pos) {
            return true;
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
        if scan_args_for_write_command(bytes, &mut pos) {
            return true;
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

/// Returns the byte slice of the token that ends at position `end` in `bytes`.
///
/// Walks backward from `end`, tracking paren depth, and stops at unquoted
/// whitespace, `(`, `|`, `;`, or `&`.  Used by both
/// `scan_backward_for_file_write` (via `token_before_pos`) and
/// `scan_backward_for_stdin_flag` to avoid duplicating the scan loop.
fn prev_token(bytes: &[u8], end: usize) -> &[u8] {
    let mut pos = end;
    let mut depth = 0i32;
    loop {
        if pos == 0 {
            break;
        }
        let b = bytes[pos - 1];
        if b == b')' {
            depth -= 1;
            pos = pos.saturating_sub(1);
            continue;
        }
        if depth < 0 {
            pos -= 1;
            break;
        }
        if depth > 0 {
            pos -= 1;
        } else if b.is_ascii_whitespace() || b == b'(' || b == b'|' || b == b';' || b == b'&' {
            break;
        } else {
            pos -= 1;
        }
    }
    &bytes[pos..end]
}

/// Returns true if `tok` is a known flag that consumes stdin from its `-` value.
fn is_stdin_consuming_flag(tok: &[u8]) -> bool {
    tok == b"--body-file"
        || tok == b"--data"
        || tok == b"--data-raw"
        || tok == b"--data-binary"
        || tok == b"--data-urlencode"
        || tok == b"-d"
        || tok == b"-F"
        || tok == b"--stdin"
}

/// Scans backward from `here_pos` (the first `<` of `<<`) looking for
/// stdin-consuming flags that would conflict with the heredoc.
///
/// Patterns detected:
///   - `--flag -` where flag is --data, --data-raw, --data-binary,
///     --data-urlencode, --body-file, -d, -F
///   - `--stdin` standalone flag
///   - `cat -` (cat with stdin argument)
fn scan_backward_for_stdin_flag(bytes: &[u8], here_pos: usize) -> bool {
    let mut pos = here_pos;

    // Skip whitespace backward
    while pos > 0 && (bytes[pos - 1] as char).is_ascii_whitespace() {
        pos -= 1;
    }
    if pos == 0 {
        return false;
    }

    let tok = prev_token(bytes, pos);
    pos -= tok.len();

    // Case 1: token is `-` (the stdin value for a preceding flag)
    if tok == b"-" {
        // Skip whitespace before `-`
        while pos > 0 && (bytes[pos - 1] as char).is_ascii_whitespace() {
            pos -= 1;
        }
        if pos == 0 {
            return false;
        }
        // Find the flag before `-`
        let flag = prev_token(bytes, pos);
        if is_stdin_consuming_flag(flag) {
            return true;
        }
        // cat - << EOF
        if flag == b"cat" {
            return true;
        }
        return false;
    }

    // Case 2: standalone flag like --stdin (no trailing `-` value)
    if is_stdin_consuming_flag(tok) {
        return true;
    }

    false
}

pub(crate) fn stdin_flag_heredoc_error() -> ErrorData {
    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "stdin-consuming flag with heredoc detected (--body-file -, --data -, etc.) -- pass content via the `stdin` parameter instead, or write to a file first with edit_overwrite".to_string(),
        Some(error_meta("validation", false, "use the stdin parameter instead of heredoc + stdin-consuming flags")),
    )
}

pub(crate) fn file_write_heredoc_error() -> ErrorData {
    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "heredoc file-write pattern detected (cat/tee/redirect + <<) -- use edit_overwrite to write files instead of shell heredocs".to_string(),
        Some(error_meta("validation", false, "use edit_overwrite to write files")),
    )
}

pub(crate) fn missing_heredoc_error() -> ErrorData {
    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "heredoc closing delimiter not found -- likely a quoting or escaping issue; use edit_overwrite to write files instead of shell heredocs".to_string(),
        Some(error_meta("validation", false, "use edit_overwrite to write files")),
    )
}

pub(crate) fn stdin_param_heredoc_error() -> ErrorData {
    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "stdin parameter and heredoc cannot be used together -- pass content via the `stdin` parameter instead".to_string(),
        Some(error_meta("validation", false, "use the stdin parameter instead of a heredoc")),
    )
}
