// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Pure anchor verification mirroring `scripts/bench_v19/score.py` semantics:
//! 1-based lines, a symmetric +/-2 line window, word-boundary symbol matching
//! (a raw substring hit does not verify), and fail-closed behavior: a missing,
//! unreadable, or invalid-UTF-8 file never verifies.

use crate::types::{AnchorItem, AnchorResult};
use std::collections::HashMap;
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
    verify_anchors_with(root, anchors, |p| std::fs::read_to_string(p))
}

/// Implementation of [`verify_anchors`] with an injectable file reader so tests can
/// count reads. Anchors are grouped by path so each file is read at most once per
/// call, and one regex is compiled per distinct symbol.
pub(crate) fn verify_anchors_with<F>(
    root: &Path,
    anchors: &[AnchorItem],
    mut read: F,
) -> Vec<AnchorResult>
where
    F: for<'a> FnMut(&'a Path) -> std::io::Result<String>,
{
    let canonical_root = std::fs::canonicalize(root).ok();

    // Group anchor indices by path so each distinct file is read once per call.
    let mut by_path: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, a) in anchors.iter().enumerate() {
        by_path.entry(a.path.clone()).or_default().push(i);
    }

    // One compiled regex per distinct symbol across the whole batch.
    let mut regex_cache: HashMap<String, Option<regex::Regex>> = HashMap::new();

    let mut results: Vec<Option<AnchorResult>> = anchors.iter().map(|_| None).collect();
    for (path, indices) in by_path {
        // Resolve the path once per file; fail closed on escape from the root.
        let canonical = std::fs::canonicalize(root.join(&path))
            .ok()
            .filter(|c| canonical_root.as_ref().is_some_and(|r| c.starts_with(r)));
        let content = canonical
            .as_ref()
            .and_then(|c| read(c).ok())
            .map(|c| c.lines().map(str::to_string).collect::<Vec<String>>());
        for &i in &indices {
            results[i] = Some(verify_grouped(
                &anchors[i],
                content.as_deref(),
                &mut regex_cache,
            ));
        }
    }
    results.into_iter().flatten().collect()
}

/// Verify a single anchor against the pre-read lines of its (already resolved)
/// file, compiling (or reusing) a regex for its symbol.
fn verify_grouped(
    a: &AnchorItem,
    lines: Option<&[String]>,
    regex_cache: &mut HashMap<String, Option<regex::Regex>>,
) -> AnchorResult {
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
    let Some(lines) = lines else {
        return r;
    };
    r.exists = true;
    r.in_range = a.line <= lines.len();
    let Some(sym) = a.symbol.as_deref() else {
        return r;
    };
    if !r.in_range {
        return r;
    }
    let lo = a.line.saturating_sub(1 + ANCHOR_WINDOW_LINES);
    let hi = (a.line + ANCHOR_WINDOW_LINES).min(lines.len());
    let re = regex_cache
        .entry(sym.to_string())
        .or_insert_with(|| regex::Regex::new(&format!(r"\b{}\b", regex::escape(sym))).ok());
    if let Some(re) = re {
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

    #[test]
    fn n_anchors_over_one_file_read_exactly_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixtures(
            tmp.path(),
            "a.rs",
            "fn one() {}\nfn foo() {}\nfn bar() {}\n",
        );
        let anchors: Vec<AnchorItem> = (1..=10).map(|l| item("a.rs", l, Some("foo"))).collect();
        let mut reads = 0usize;
        let out = verify_anchors_with(tmp.path(), &anchors, |p| {
            reads += 1;
            std::fs::read_to_string(p)
        });
        assert_eq!(reads, 1, "one file must be read exactly once per call");
        assert!(out.iter().all(|r| r.exists));
        assert_eq!(out[1].symbol_found, Some(true));
    }

    #[test]
    fn m_files_perform_at_most_m_reads() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixtures(tmp.path(), "a.rs", "fn foo() {}\n");
        write_fixtures(tmp.path(), "b.rs", "fn bar() {}\n");
        write_fixtures(tmp.path(), "c.rs", "fn baz() {}\n");
        let anchors = vec![
            item("a.rs", 1, Some("foo")),
            item("a.rs", 1, Some("foo")),
            item("b.rs", 1, Some("bar")),
            item("b.rs", 1, Some("bar")),
            item("c.rs", 1, Some("baz")),
        ];
        let mut reads = 0usize;
        let out = verify_anchors_with(tmp.path(), &anchors, |p| {
            reads += 1;
            std::fs::read_to_string(p)
        });
        assert_eq!(reads, 3, "M distinct files must be read at most M times");
        assert_eq!(out.len(), 5);
        assert!(out.iter().all(|r| r.symbol_found == Some(true)));
    }
}
