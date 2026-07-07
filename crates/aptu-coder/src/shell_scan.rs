// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Shell scan helpers: backward token scanning for heredoc validation.
//!
//! Extracted from `shell_write.rs` to isolate the token-scanning primitives
//! used by `heredoc_validation.rs`.

/// Scans backward from `here_pos` (the first `<` of `<<`) looking for
/// file-write patterns that would conflict with the heredoc.
///
/// Patterns detected:
///   - `cat > file <<`, `cat >> file <<` (redirect + heredoc)
///   - `tee file <<`, `tee -a file <<` (tee with file argument + heredoc)
///   - `> file <<`, `>> file <<` (bare redirect + heredoc)
///   - `(cat > file <<` (subshell grouping)
///   - `cat > >(proc) <<` (process substitution)
///   - `cat > $(cmd) <<` (command substitution)
///   - `dd of=file <<`, `printf 'content' > file <<`
pub(crate) fn scan_backward_for_file_write(bytes: &[u8], here_pos: usize) -> bool {
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

    if pos > 0 && bytes[pos - 1] == b'>' {
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
fn token_before_pos<'a>(bytes: &'a [u8], pos: &mut usize) -> &'a [u8] {
    // Skip trailing whitespace
    while *pos > 0 && (bytes[*pos - 1] as char).is_ascii_whitespace() {
        *pos -= 1;
    }

    if *pos == 0 {
        return b"";
    }

    paren_aware_token(bytes, pos)
}

/// Extracts a single token backward from `pos`, handling parentheses and
/// process/command substitution groups.
fn paren_aware_token<'a>(bytes: &'a [u8], pos: &mut usize) -> &'a [u8] {
    let end = *pos;

    // Check if the character before pos is `)` (closing paren)
    if bytes[*pos - 1] == b')' {
        // Walk backward to find matching `(`
        let mut depth = 1;
        *pos -= 1;
        while *pos > 0 && depth > 0 {
            *pos -= 1;
            if bytes[*pos] == b'(' {
                depth -= 1;
            } else if bytes[*pos] == b')' {
                depth += 1;
            }
        }
        return &bytes[*pos..end];
    }

    // Check if the character before pos is `>` (process substitution start)
    if bytes[*pos - 1] == b'>' {
        *pos -= 1;
        if *pos > 0 && bytes[*pos - 1] == b'>' {
            *pos -= 1; // `>>`
        }
        return &bytes[*pos..end];
    }

    // Regular token: walk backward over non-whitespace characters
    while *pos > 0 && !(bytes[*pos - 1] as char).is_ascii_whitespace() {
        *pos -= 1;
    }

    &bytes[*pos..end]
}

/// Skips whitespace scanning backward from `pos`.
fn skip_ws_backward(bytes: &[u8], pos: &mut usize) {
    while *pos > 0 && (bytes[*pos - 1] as char).is_ascii_whitespace() {
        *pos -= 1;
    }
}

/// Returns `true` if `cmd` is a known stdin-consuming command that
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
    cmd == b"cat" || cmd == b"tee" || cmd == b"printf" || cmd == b"dd" || cmd.first() == Some(&b'$')
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

/// Returns the token immediately before `end` without modifying `end`.
fn prev_token(bytes: &[u8], end: usize) -> &[u8] {
    let mut pos = end;
    // Skip trailing whitespace
    while pos > 0 && (bytes[pos - 1] as char).is_ascii_whitespace() {
        pos -= 1;
    }
    if pos == 0 {
        return b"";
    }
    let end_pos = pos;
    // Walk backward over non-whitespace
    while pos > 0 && !(bytes[pos - 1] as char).is_ascii_whitespace() {
        pos -= 1;
    }
    &bytes[pos..end_pos]
}

/// Returns `true` if `tok` is a flag that consumes stdin (e.g., `--data -`).
fn is_stdin_consuming_flag(tok: &[u8]) -> bool {
    tok == b"--data"
        || tok == b"--data-raw"
        || tok == b"--data-binary"
        || tok == b"--data-urlencode"
        || tok == b"--body-file"
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
pub(crate) fn scan_backward_for_stdin_flag(bytes: &[u8], here_pos: usize) -> bool {
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

#[cfg(test)]
mod tests {
    use super::scan_backward_for_file_write;

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
