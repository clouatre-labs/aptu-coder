// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Def-use pagination formatting.

use std::fmt::Write;
use std::path::Path;

use crate::formatter::{snippet_one_line, strip_base_path};

/// Format a page of def-use sites for pagination.
/// Renders a DEF-USE SITES section with WRITES and READS sub-sections.
#[must_use]
pub fn format_focused_paginated_defuse(
    paginated_sites: &[crate::types::DefUseSite],
    total: usize,
    symbol: &str,
    offset: usize,
    base_path: Option<&Path>,
    _verbose: bool,
) -> String {
    let mut output = String::new();

    let page_size = paginated_sites.len();
    let (start, end) = if page_size == 0 {
        (0, 0)
    } else {
        (offset + 1, offset + page_size)
    };

    let _ = writeln!(
        output,
        "DEF-USE SITES  {symbol}  ({start}-{end} of {total})"
    );

    // Render writes (Write and WriteRead)
    let write_sites: Vec<_> = paginated_sites
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                crate::types::DefUseKind::Write | crate::types::DefUseKind::WriteRead
            )
        })
        .collect();

    if !write_sites.is_empty() {
        output.push_str("  WRITES\n");
        for site in write_sites {
            let file_display = strip_base_path(Path::new(&site.file), base_path);
            let scope_str = site
                .enclosing_scope
                .as_ref()
                .map(|s| format!("{}()", s))
                .unwrap_or_default();
            let snippet = snippet_one_line(&site.snippet);
            let wr_label = if site.kind == crate::types::DefUseKind::WriteRead {
                " [write_read]"
            } else {
                ""
            };
            let _ = writeln!(
                output,
                "    {file_display}:{}  {scope_str}  {snippet}{wr_label}",
                site.line
            );
        }
    }

    // Render reads
    let read_sites: Vec<_> = paginated_sites
        .iter()
        .filter(|s| matches!(s.kind, crate::types::DefUseKind::Read))
        .collect();

    if !read_sites.is_empty() {
        output.push_str("  READS\n");
        for site in read_sites {
            let file_display = strip_base_path(Path::new(&site.file), base_path);
            let scope_str = site
                .enclosing_scope
                .as_ref()
                .map(|s| format!("{}()", s))
                .unwrap_or_default();
            let snippet = snippet_one_line(&site.snippet);
            let _ = writeln!(
                output,
                "    {file_display}:{}  {scope_str}  {snippet}",
                site.line
            );
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DefUseKind, DefUseSite};

    fn site(kind: DefUseKind, line: usize, scope: Option<&str>, file: &str) -> DefUseSite {
        DefUseSite {
            kind,
            symbol: "x".to_string(),
            file: file.to_string(),
            line,
            column: 0,
            snippet: "prev\nlet x = 1;\nnext".to_string(),
            enclosing_scope: scope.map(String::from),
        }
    }

    #[test]
    fn test_format_paginated_defuse_writes_and_reads() {
        // Arrange
        let sites = vec![
            site(DefUseKind::Write, 10, Some("init"), "src/main.rs"),
            site(DefUseKind::WriteRead, 20, None, "src/lib.rs"),
            site(DefUseKind::Read, 30, Some("run"), "src/main.rs"),
        ];
        let base = Path::new("/project");

        // Act
        let output = format_focused_paginated_defuse(&sites, 3, "x", 0, Some(base), false);

        // Assert
        assert!(output.contains("DEF-USE SITES  x  (1-3 of 3)"));
        assert!(output.contains("WRITES"));
        assert!(output.contains("src/main.rs:10  init()"));
        assert!(output.contains("[write_read]"));
        assert!(output.contains("READS"));
        assert!(output.contains("src/main.rs:30  run()"));
    }

    #[test]
    fn test_format_paginated_defuse_no_base_path() {
        // Arrange: single read, no base path, no enclosing scope
        let sites = vec![site(DefUseKind::Read, 5, None, "/abs/path/file.rs")];

        // Act
        let output = format_focused_paginated_defuse(&sites, 1, "x", 0, None, false);

        // Assert
        assert!(output.contains("/abs/path/file.rs:5"));
        assert!(!output.contains("WRITES"));
        assert!(output.contains("READS"));
    }
}
