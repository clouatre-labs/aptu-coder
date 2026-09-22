// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Strip ANSI escape sequences from terminal output.

/// Strip ANSI escape sequences so emitted output never contains them, even if
/// the input does. Handles CSI sequences (`ESC [ ... final-byte @-~`), OSC
/// sequences (`ESC ] ... BEL` or `ESC \`), DCS sequences, and two-character
/// `ESC <byte>` sequences.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: consume through the final byte in the @..~ range.
                for c2 in chars.by_ref() {
                    if ('@'..='~').contains(&c2) {
                        break;
                    }
                }
            }
            Some(']') | Some('P') => {
                chars.next();
                // OSC/DCS: terminated by BEL or by ESC \ (ST).
                while let Some(c2) = chars.by_ref().next() {
                    if c2 == '\u{7}' {
                        break;
                    }
                    if c2 == '\u{1b}' {
                        chars.next();
                        break;
                    }
                }
            }
            // Two-character ESC sequence: consume the following byte.
            _ => {
                chars.next();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn removes_csi_sequences() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
    }

    #[test]
    fn removes_osc_terminated_by_bel() {
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}text"), "text");
    }

    #[test]
    fn removes_osc_terminated_by_st() {
        assert_eq!(
            strip_ansi("\u{1b}]8;;https://example.com\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
    }

    #[test]
    fn removes_dcs_sequences() {
        assert_eq!(strip_ansi("\u{1b}P+q544e\u{1b}\\ok"), "ok");
    }

    #[test]
    fn removes_two_char_esc_sequences() {
        assert_eq!(strip_ansi("\u{1b}Mline\u{1b}7more"), "linemore");
    }

    #[test]
    fn passes_through_text_without_esc() {
        assert_eq!(
            strip_ansi("plain text\nwith lines"),
            "plain text\nwith lines"
        );
    }

    #[test]
    fn malformed_st_terminates_without_panic() {
        // ESC mid-OSC not followed by backslash: swallows exactly the escape
        // and the one following char, then terminates the sequence.
        assert_eq!(strip_ansi("\u{1b}]title\u{1b}Xrest"), "rest");
    }
}
