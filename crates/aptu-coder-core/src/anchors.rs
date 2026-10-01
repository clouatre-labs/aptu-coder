// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Pure anchor verification mirroring `scripts/bench_v19/score.py` semantics:
//! 1-based lines, a symmetric +/-2 line window, word-boundary symbol matching
//! (a raw substring hit does not verify), and fail-closed behavior: a missing,
//! unreadable, or invalid-UTF-8 file never verifies.

use crate::types::{AnchorItem, AnchorResult};
use std::path::Path;

/// Symmetric window (in lines) around the cited line; mirrors
/// `ANCHOR_WINDOW_LINES` in `scripts/bench_v19/score.py`.
pub const ANCHOR_WINDOW_LINES: usize = 2;

/// Verify a batch of anchors against `root`, one [`AnchorResult`] per input
/// anchor in input order. `exists` is true only when the file resolves under
/// `root` and reads as UTF-8; `in_range` is true only when the 1-based `line`
/// is within the file's line count (0 always fails closed). With a `symbol`,
/// lines `[line - ANCHOR_WINDOW_LINES, line + ANCHOR_WINDOW_LINES]` (clamped)
/// are searched for a word-boundary match, reported via `window_line`.
pub fn verify_anchors(root: &Path, anchors: &[AnchorItem]) -> Vec<AnchorResult> {
    let canonical_root = std::fs::canonicalize(root);
    anchors
        .iter()
        .map(|a| verify_one(root, canonical_root.as_deref().ok(), a))
        .collect()
}

fn verify_one(root: &Path, canonical_root: Option<&Path>, a: &AnchorItem) -> AnchorResult {
    let mut r = AnchorResult {
        path: a.path.clone(),
        line: a.line,
        symbol: a.symbol.clone(),
        exists: false,
        in_range: false,
        symbol_found: None,
        window_line: None,
    };
    // 1-based lines: 0 is invalid, fail closed.
    if a.line == 0 {
        return r;
    }
    let Some(canon_root) = canonical_root else {
        return r;
    };
    let Ok(canonical) = std::fs::canonicalize(root.join(&a.path)) else {
        return r;
    };
    // Fail closed when the resolved path escapes the workspace root.
    if !canonical.starts_with(canon_root) {
        return r;
    }
    let Ok(content) = std::fs::read_to_string(&canonical) else {
        return r;
    };
    r.exists = true;
    let lines: Vec<&str> = content.lines().collect();
    r.in_range = a.line <= lines.len();
    let Some(sym) = a.symbol.as_deref() else {
        return r;
    };
    if !r.in_range {
        return r;
    }
    let lo = a.line.saturating_sub(1 + ANCHOR_WINDOW_LINES);
    let hi = (a.line + ANCHOR_WINDOW_LINES).min(lines.len());
    let re = regex::Regex::new(&format!(r"\b{}\b", regex::escape(sym)));
    if let Ok(re) = re {
        for (i, line) in lines[lo..hi].iter().enumerate() {
            if re.is_match(line) {
                r.symbol_found = Some(true);
                r.window_line = Some(lo + i + 1);
                return r;
            }
        }
    }
    r.symbol_found = Some(false);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixtures(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write fixture");
    }

    fn item(path: &str, line: usize, symbol: Option<&str>) -> AnchorItem {
        AnchorItem {
            path: path.to_string(),
            line,
            symbol: symbol.map(str::to_string),
        }
    }

    #[test]
    fn happy_path_symbol_at_target_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixtures(tmp.path(), "a.rs", "fn one() {}\nfn foo() {}\n");
        let out = verify_anchors(tmp.path(), &[item("a.rs", 2, Some("foo"))]);
        assert!(out[0].exists);
        assert!(out[0].in_range);
        assert_eq!(out[0].symbol_found, Some(true));
        assert_eq!(out[0].window_line, Some(2));
    }

    #[test]
    fn word_boundary_foo_does_not_match_foobar() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixtures(tmp.path(), "a.rs", "fn foobar() {}\nalpha\nbeta\n");
        let out = verify_anchors(tmp.path(), &[item("a.rs", 1, Some("foo"))]);
        assert!(out[0].exists);
        assert!(out[0].in_range);
        assert_eq!(out[0].symbol_found, Some(false));
        assert_eq!(out[0].window_line, None);
    }

    #[test]
    fn line_at_file_end_in_range_one_past_end_out_of_range() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixtures(tmp.path(), "a.rs", "alpha\nbeta\n");
        let out = verify_anchors(tmp.path(), &[item("a.rs", 2, None), item("a.rs", 3, None)]);
        assert!(out[0].in_range);
        assert!(!out[1].in_range);
        assert!(out[1].exists);
        assert_eq!(out[1].symbol_found, None);
    }

    #[test]
    fn nonexistent_file_fails_closed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = verify_anchors(tmp.path(), &[item("missing.rs", 1, Some("foo"))]);
        assert!(!out[0].exists);
        assert!(!out[0].in_range);
        assert_eq!(out[0].symbol_found, None);
    }
}
