// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Capped, ANSI-free unified diff (2 KiB / 20 changed lines, context radius 3)
//! for the opt-in `include_diff` edit-tool parameter.

use similar::TextDiff;

const MAX_PATCH_BYTES: usize = 2048;
const MAX_CHANGED_LINES: usize = 20;
const CONTEXT_RADIUS: usize = 3;
const TRUNCATION_MARKER: &str = "[... diff truncated]\n";

/// Result of a capped unified diff computation.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct DiffOutcome {
    /// Unified patch text (hunks only, no file headers). Empty when inputs match.
    pub patch: String,
    /// True when hunks or bytes were dropped due to the caps.
    pub truncated: bool,
    /// UTF-8 byte length of `patch`.
    pub bytes: usize,
}

/// Strip ANSI escape sequences so emitted diffs never contain them, even if
/// the input does. Handles CSI sequences (`ESC [ ... final-byte @-~`), OSC
/// sequences (`ESC ] ... BEL` or `ESC \`), DCS sequences, and two-character
/// `ESC <byte>` sequences.
fn strip_ansi(input: &str) -> String {
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

/// A hunk under construction; the header is derived only when flushing, from
/// the lines actually emitted, so counts always match the body.
struct HunkBuilder {
    old_start: usize,
    new_start: usize,
    old_count: usize,
    new_count: usize,
    changed: usize,
    body: String,
}

impl HunkBuilder {
    fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@\n",
            self.old_start + 1,
            self.old_count,
            self.new_start + 1,
            self.new_count
        )
    }
}

/// Compute a capped unified diff between `old` and `new`.
///
/// CRLF line endings in either input are normalized to LF and ANSI escape
/// sequences are stripped before diffing; the output never contains ANSI
/// escapes. Every emitted body line begins with a unified-diff prefix
/// (` `, `-`, or `+`) and ends with a newline, so inputs lacking a trailing
/// newline never concatenate records. When the byte or changed-line cap is
/// hit, emission stops at line granularity, hunk headers are recomputed from
/// the lines actually emitted, and a `[... diff truncated]` marker is
/// appended. The marker's bytes are reserved within the cap, so the final
/// patch is always at most 2 KiB. Identical inputs yield an empty patch.
#[must_use]
pub fn unified_diff(old: &str, new: &str) -> DiffOutcome {
    if old == new {
        return DiffOutcome::default();
    }
    let old = strip_ansi(&old.replace("\r\n", "\n"));
    let new = strip_ansi(&new.replace("\r\n", "\n"));
    let diff = TextDiff::from_lines(&old, &new);

    let mut patch = String::new();
    let mut changed_total = 0usize;
    let mut truncated = false;
    let mut stop = false;

    for group in &diff.grouped_ops(CONTEXT_RADIUS) {
        let mut hunk = HunkBuilder {
            old_start: group.first().map_or(0, |op| op.old_range().start),
            new_start: group.first().map_or(0, |op| op.new_range().start),
            old_count: 0,
            new_count: 0,
            changed: 0,
            body: String::new(),
        };
        'ops: for op in group {
            for change in diff.iter_changes(op) {
                if change.tag() != similar::ChangeTag::Equal
                    && changed_total + hunk.changed >= MAX_CHANGED_LINES
                {
                    truncated = true;
                    stop = true;
                    break 'ops;
                }
                let prefix = match change.tag() {
                    similar::ChangeTag::Equal => ' ',
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                };
                let mut line = String::with_capacity(change.value().len() + 1);
                line.push(prefix);
                line.push_str(change.value());
                // Normalize so records lacking a trailing newline never
                // concatenate with the next emitted line.
                if !line.ends_with('\n') {
                    line.push('\n');
                }
                // Reserve room for the truncation marker so the FINAL
                // emitted patch (including the marker) stays within the cap.
                if patch.len() + hunk.header().len() + hunk.body.len() + line.len()
                    > MAX_PATCH_BYTES - TRUNCATION_MARKER.len()
                {
                    truncated = true;
                    stop = true;
                    break 'ops;
                }
                match change.tag() {
                    similar::ChangeTag::Equal => {
                        hunk.old_count += 1;
                        hunk.new_count += 1;
                    }
                    similar::ChangeTag::Delete => {
                        hunk.old_count += 1;
                        hunk.changed += 1;
                    }
                    similar::ChangeTag::Insert => {
                        hunk.new_count += 1;
                        hunk.changed += 1;
                    }
                }
                hunk.body.push_str(&line);
            }
        }
        // Flush even when stopped mid-group: the header is recomputed from
        // the lines actually emitted, so it always matches the body.
        patch.push_str(&hunk.header());
        patch.push_str(&hunk.body);
        changed_total += hunk.changed;
        if stop {
            break;
        }
    }

    if truncated {
        if !patch.is_empty() && !patch.ends_with('\n') {
            patch.push('\n');
        }
        patch.push_str(TRUNCATION_MARKER);
    }

    let bytes = patch.len();
    DiffOutcome {
        patch,
        truncated,
        bytes,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_yield_empty_patch() {
        let outcome = unified_diff("same\n", "same\n");
        assert!(outcome.patch.is_empty());
        assert!(!outcome.truncated);
        assert_eq!(outcome.bytes, 0);
    }

    #[test]
    fn single_line_change_normalizes_crlf_and_avoids_ansi() {
        let old = "a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\n";
        let new = "a\r\nb\r\nC\r\nd\r\ne\r\nf\r\ng\r\n";
        let outcome = unified_diff(old, new);
        assert!(outcome.patch.starts_with("@@ -"));
        assert!(outcome.patch.contains("-c\n"));
        assert!(outcome.patch.contains("+C\n"));
        assert!(!outcome.patch.contains('\r'));
        assert!(!outcome.patch.contains('\u{1b}'));
        assert!(!outcome.truncated);
        assert_eq!(outcome.bytes, outcome.patch.len());
    }

    #[test]
    fn context_lines_have_single_leading_space() {
        let outcome = unified_diff("a\nb\nc\nd\ne\nf\ng\n", "a\nb\nC\nd\ne\nf\ng\n");
        assert!(outcome.patch.contains("\n a\n"));
        assert!(outcome.patch.contains("\n b\n"));
        assert!(outcome.patch.contains("\n d\n"));
        for line in outcome.patch.lines() {
            if !line.starts_with("@@") {
                let first = line.chars().next().unwrap_or_default();
                assert!(
                    first == ' ' || first == '-' || first == '+',
                    "bad line prefix: {line:?}"
                );
                if first == ' ' {
                    assert!(!line[1..].starts_with(' '), "double space: {line:?}");
                }
            }
        }
    }

    #[test]
    fn missing_trailing_newline_does_not_concatenate_records() {
        let outcome = unified_diff("a\nb", "a\nc");
        assert!(outcome.patch.contains("-b\n"));
        assert!(outcome.patch.contains("+c\n"));
        assert!(!outcome.patch.contains("bc"));
        for line in outcome.patch.lines() {
            if !line.starts_with("@@") {
                assert!(
                    line.starts_with(' ') || line.starts_with('-') || line.starts_with('+'),
                    "concatenated record: {line:?}"
                );
            }
        }
    }

    #[test]
    fn strips_csi_osc_and_two_char_ansi_sequences() {
        let outcome = unified_diff(
            "\u{1b}[31mred\u{1b}[0m\n\u{1b}]0;title\u{7}plain\n\u{1b}Mreverse\n",
            "red\nPLAIN\nlink\n\u{1b}]8;;url\u{1b}\\tail\n",
        );
        assert!(!outcome.patch.contains('\u{1b}'));
        assert!(!outcome.patch.contains('\u{7}'));
        assert!(outcome.patch.contains("-plain\n"));
        assert!(outcome.patch.contains("+PLAIN\n"));
        assert!(outcome.patch.contains("-reverse\n"));
        assert!(outcome.patch.contains("+tail\n"));
    }

    #[test]
    fn final_patch_never_exceeds_cap_when_truncation_fires() {
        let filler = "y".repeat(60);
        let old: String = (0..60).map(|i| format!("{filler}{i:04}\n")).collect();
        let new: String = (0..60).map(|i| format!("{filler}Z{i:03}\n")).collect();
        let outcome = unified_diff(&old, &new);
        assert!(outcome.truncated);
        assert!(outcome.patch.ends_with(TRUNCATION_MARKER));
        assert!(outcome.patch.len() <= MAX_PATCH_BYTES);
    }

    #[test]
    fn truncates_over_changed_line_and_byte_caps() {
        // 30 single-line changes exceed the 20 changed-line cap.
        let mut old = String::new();
        let mut new = String::new();
        for i in 0..30 {
            old.push_str(&format!("line{i}x\n"));
            new.push_str(&format!("line{i}y\n"));
        }
        let outcome = unified_diff(&old, &new);
        assert!(outcome.truncated);
        assert!(outcome.patch.ends_with(TRUNCATION_MARKER));
        assert!(outcome.bytes <= MAX_PATCH_BYTES);
        // Every hunk header must describe exactly the emitted lines.
        let mut emitted_changed = 0usize;
        let mut in_hunk = false;
        let (mut header_old, mut header_new) = (0usize, 0usize);
        let (mut count_old, mut count_new) = (0usize, 0usize);
        let mut check_line = |line: &str, in_hunk: &mut bool| {
            if let Some(ranges) = line
                .strip_prefix("@@ -")
                .and_then(|h| h.strip_suffix(" @@"))
            {
                if *in_hunk {
                    assert_eq!(count_old, header_old, "old count mismatch: {outcome:?}");
                    assert_eq!(count_new, header_new, "new count mismatch: {outcome:?}");
                }
                let mut parts = ranges.split(" +");
                let old_part = parts.next().unwrap_or_default();
                let new_part = parts.next().unwrap_or_default();
                header_old = old_part
                    .split(',')
                    .next_back()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(1);
                header_new = new_part
                    .split(',')
                    .next_back()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(1);
                count_old = 0;
                count_new = 0;
                *in_hunk = true;
                return;
            }
            assert!(!line.starts_with(TRUNCATION_MARKER.trim_end()));
            match line.chars().next() {
                Some('-') => {
                    count_old += 1;
                    emitted_changed += 1;
                }
                Some('+') => {
                    count_new += 1;
                    emitted_changed += 1;
                }
                Some(' ') => {
                    count_old += 1;
                    count_new += 1;
                }
                _ => {}
            }
        };
        for line in outcome.patch.lines() {
            if line == TRUNCATION_MARKER.trim_end() {
                break;
            }
            check_line(line, &mut in_hunk);
        }
        if in_hunk {
            assert_eq!(count_old, header_old);
            assert_eq!(count_new, header_new);
        }
        assert!(emitted_changed <= MAX_CHANGED_LINES);

        // One change whose context exceeds 2 KiB hits the byte cap; emission
        // stops at line granularity and headers still match the body.
        let filler = "x".repeat(80);
        let context: String = (0..40).map(|i| format!("{filler}{i:04}\n")).collect();
        let new = format!("{context}changed\n{context}");
        let outcome = unified_diff(&context, &new);
        assert!(outcome.truncated);
        assert!(outcome.bytes <= MAX_PATCH_BYTES);
        assert!(outcome.patch.ends_with(TRUNCATION_MARKER));
        let mut count = 0usize;
        for line in outcome.patch.lines() {
            if line.starts_with("@@") {
                continue;
            }
            if line == TRUNCATION_MARKER.trim_end() {
                break;
            }
            count += 1;
            assert!(line.len() < MAX_PATCH_BYTES, "partial line emitted");
        }
        assert!(count > 0, "no lines emitted before truncation marker");
    }
}
