// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Capped, ANSI-free unified diff (2 KiB / 20 changed lines, context radius 3)
//! for the opt-in `include_diff` edit-tool parameter.

use similar::TextDiff;

const MAX_PATCH_BYTES: usize = 2048;
const MAX_CHANGED_LINES: usize = 20;
const CONTEXT_RADIUS: usize = 3;

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

/// Strip ANSI escape sequences (ESC CSI ... final byte, and two-character ESC
/// sequences) so emitted diffs never contain them, even if the input does.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for c2 in chars.by_ref() {
                if ('@'..='~').contains(&c2) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

/// Compute a capped unified diff between `old` and `new`.
///
/// CRLF line endings in either input are normalized to LF and ANSI escape
/// sequences are stripped before diffing; the output never contains ANSI
/// escapes. Identical inputs yield an empty patch.
#[must_use]
pub fn unified_diff(old: &str, new: &str) -> DiffOutcome {
    if old == new {
        return DiffOutcome {
            patch: String::new(),
            truncated: false,
            bytes: 0,
        };
    }
    let old = strip_ansi(&old.replace("\r\n", "\n"));
    let new = strip_ansi(&new.replace("\r\n", "\n"));
    let diff = TextDiff::from_lines(&old, &new);

    let mut patch = String::new();
    let mut changed_total = 0usize;
    let mut truncated = false;

    for group in &diff.grouped_ops(CONTEXT_RADIUS) {
        // Build the hunk body first so caps apply at hunk granularity; the
        // header is derived from the lines actually emitted, so a truncated
        // hunk still has a valid unified-diff header.
        let mut hunk_body = String::new();
        let mut hunk_changed = 0usize;
        let mut emitted_old = 0usize;
        let mut emitted_new = 0usize;
        let old_start = group.first().map_or(0, |op| op.old_range().start);
        let new_start = group.first().map_or(0, |op| op.new_range().start);
        'outer: for op in group {
            for change in diff.iter_changes(op) {
                if change.tag() != similar::ChangeTag::Equal
                    && changed_total + hunk_changed >= MAX_CHANGED_LINES
                {
                    truncated = true;
                    break 'outer;
                }
                match change.tag() {
                    similar::ChangeTag::Equal => {
                        emitted_old += 1;
                        emitted_new += 1;
                    }
                    similar::ChangeTag::Delete => {
                        hunk_changed += 1;
                        emitted_old += 1;
                    }
                    similar::ChangeTag::Insert => {
                        hunk_changed += 1;
                        emitted_new += 1;
                    }
                }
                hunk_body.push_str(change.tag().to_string().trim());
                hunk_body.push_str(change.value());
            }
        }
        let header = format!(
            "@@ -{},{} +{},{} @@\n",
            old_start + 1,
            emitted_old,
            new_start + 1,
            emitted_new
        );
        let mut hunk = String::with_capacity(header.len() + hunk_body.len());
        hunk.push_str(&header);
        hunk.push_str(&hunk_body);

        let would_change = changed_total + hunk_changed;
        if would_change > MAX_CHANGED_LINES {
            truncated = true;
        }
        if patch.len() + hunk.len() > MAX_PATCH_BYTES {
            truncated = true;
            if patch.is_empty() {
                // Hard-truncate the first (oversized) hunk at a char boundary.
                let mut take = MAX_PATCH_BYTES.saturating_sub(header.len());
                while !hunk.is_char_boundary(take.min(hunk.len())) {
                    take -= 1;
                }
                patch.push_str(&hunk[..take.min(hunk.len())]);
            }
            break;
        }
        patch.push_str(&hunk);
        changed_total = would_change;
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
    fn strips_ansi_from_inputs() {
        let outcome = unified_diff(
            "\u{1b}[31mred\u{1b}[0m\nplain\n",
            "red\nPLAIN\n\u{1b}[1mbold\u{1b}[0m\n",
        );
        assert!(!outcome.patch.contains('\u{1b}'));
        assert!(outcome.patch.contains("-plain"));
        assert!(outcome.patch.contains("+PLAIN"));
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
        assert!(outcome.bytes <= MAX_PATCH_BYTES);
        // Every hunk header must describe exactly the emitted lines.
        let mut emitted_changed = 0usize;
        let mut in_hunk = false;
        let (mut header_old, mut header_new) = (0usize, 0usize);
        let (mut count_old, mut count_new) = (0usize, 0usize);
        for line in outcome.patch.lines() {
            if let Some(ranges) = line
                .strip_prefix("@@ -")
                .and_then(|h| h.strip_suffix(" @@"))
            {
                if in_hunk {
                    assert_eq!(count_old, header_old, "old count mismatch: {outcome:?}");
                    assert_eq!(count_new, header_new, "new count mismatch: {outcome:?}");
                }
                let mut parts = ranges.split(" +");
                let old_part = parts.next().unwrap_or_default();
                let new_part = parts.next().unwrap_or_default();
                let mut old_it = old_part.split(',');
                let mut new_it = new_part.split(',');
                let _ = old_it.next();
                let _ = new_it.next();
                header_old = old_it.next().and_then(|n| n.parse().ok()).unwrap_or(1);
                header_new = new_it.next().and_then(|n| n.parse().ok()).unwrap_or(1);
                count_old = 0;
                count_new = 0;
                in_hunk = true;
                continue;
            }
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
        }
        if in_hunk {
            assert_eq!(count_old, header_old);
            assert_eq!(count_new, header_new);
        }
        assert!(emitted_changed <= MAX_CHANGED_LINES);

        // One change whose context exceeds 2 KiB hits the byte cap.
        let filler = "x".repeat(80);
        let context: String = (0..40).map(|i| format!("{filler}{i:04}\n")).collect();
        let new = format!("{context}changed\n{context}");
        let outcome = unified_diff(&context, &new);
        assert!(outcome.truncated);
        assert!(outcome.bytes <= MAX_PATCH_BYTES);
    }
}
