// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Markdown language handler for tree-sitter-md.
//!
//! Extracts ATX headings (`# Heading`) and setext headings (underlined with `===` or `---`)
//! as function-equivalent elements. Fenced code block contents are not extracted.

/// Tree-sitter query for extracting Markdown headings as elements.
///
/// Both ATX headings (`# Title`) and setext headings (text underlined with `=` or `-`)
/// are captured via the `heading_content` field. The field syntax is required because
/// `heading_content` is a field name on the heading nodes, not a standalone node type.
pub const ELEMENT_QUERY: &str = r"
(atx_heading heading_content: (_) @func_name) @function
(setext_heading heading_content: (_) @func_name) @function
";

/// Tree-sitter call query for Markdown (empty -- no call sites in Markdown).
pub const CALL_QUERY: &str = "";

#[cfg(test)]
mod tests {
    use tree_sitter::{Parser, StreamingIterator};

    fn parse_and_query(src: &str, query_str: &str) -> Vec<String> {
        let language = tree_sitter_md::LANGUAGE;
        let mut parser = Parser::new();
        parser
            .set_language(&language.into())
            .expect("failed to set language");
        let tree = parser.parse(src, None).expect("parse failed");
        let query = tree_sitter::Query::new(&language.into(), query_str).expect("invalid query");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), src.as_bytes());
        let func_name_idx = query
            .capture_index_for_name("func_name")
            .expect("no func_name capture");
        let mut names = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                if cap.index == func_name_idx {
                    let text = &src[cap.node.start_byte()..cap.node.end_byte()];
                    names.push(text.trim().to_owned());
                }
            }
        }
        names
    }

    /// ATX headings are extracted with the correct heading text.
    #[test]
    fn test_atx_headings_extracted() {
        let src = "# Introduction\n\n## Installation\n\n### Details\n";
        let names = parse_and_query(src, super::ELEMENT_QUERY);
        assert_eq!(names, vec!["Introduction", "Installation", "Details"]);
    }

    /// Setext headings are extracted as functions.
    #[test]
    fn test_setext_heading_extracted() {
        let src = "Overview\n========\n\nSetup\n-----\n";
        let names = parse_and_query(src, super::ELEMENT_QUERY);
        assert_eq!(names, vec!["Overview", "Setup"]);
    }

    /// A file with no headings returns zero functions and no error.
    #[test]
    fn test_no_headings_returns_empty() {
        let src = "Just some prose.\n\nNo headings here.\n";
        let names = parse_and_query(src, super::ELEMENT_QUERY);
        assert!(names.is_empty());
    }

    /// A `#` inside a fenced code block is NOT extracted as a heading.
    #[test]
    fn test_code_fence_not_extracted() {
        let src = "```python\n# not a heading\nprint('hello')\n```\n";
        let names = parse_and_query(src, super::ELEMENT_QUERY);
        assert!(names.is_empty());
    }
}
