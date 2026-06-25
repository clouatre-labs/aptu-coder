// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! CSS language handler for tree-sitter-css.
//!
//! Extracts CSS rule sets (selectors) as function-equivalent elements and
//! `@import` statements as imports.

/// Tree-sitter query for extracting CSS rule sets (selectors) as elements.
///
/// Each `rule_set` node's `selectors` child is captured as the element name.
pub const ELEMENT_QUERY: &str = r"
(rule_set (selectors) @func_name) @function
";

/// Tree-sitter query for extracting CSS `@import` statements.
///
/// Captures the string value (URL) inside each `import_statement`.
pub const IMPORT_QUERY: &str = r"
(import_statement (string_value) @import_path)
";

/// Tree-sitter call query for CSS (empty -- no call sites in CSS).
pub const CALL_QUERY: &str = "";

#[cfg(test)]
mod tests {
    use tree_sitter::{Parser, StreamingIterator};

    fn parse_and_query(src: &str, query_str: &str, capture_name: &str) -> Vec<String> {
        let language = tree_sitter_css::LANGUAGE;
        let mut parser = Parser::new();
        parser
            .set_language(&language.into())
            .expect("failed to set language");
        let tree = parser.parse(src, None).expect("parse failed");
        let query = tree_sitter::Query::new(&language.into(), query_str).expect("invalid query");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), src.as_bytes());
        let capture_idx = query
            .capture_index_for_name(capture_name)
            .expect("capture not found");
        let mut results = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                if cap.index == capture_idx {
                    let text = &src[cap.node.start_byte()..cap.node.end_byte()];
                    results.push(text.trim().to_owned());
                }
            }
        }
        results
    }

    /// CSS rule sets are extracted with the correct selector text.
    #[test]
    fn test_css_rule_set_element_extraction() {
        let src = "body { color: red; }\n.container { margin: 0; }\n#header { font-size: 16px; }\n";
        let names = parse_and_query(src, super::ELEMENT_QUERY, "func_name");
        assert_eq!(names, vec!["body", ".container", "#header"]);
    }

    /// CSS @import statements are extracted as imports (string_value includes quotes).
    #[test]
    fn test_css_import_extraction() {
        let src = "@import \"reset.css\";\n@import \"theme.css\";\nbody { color: red; }\n";
        let imports = parse_and_query(src, super::IMPORT_QUERY, "import_path");
        assert_eq!(imports, vec!["\"reset.css\"", "\"theme.css\""]);
    }

    /// An empty CSS file returns empty analysis without errors.
    #[test]
    fn test_css_empty_file() {
        let names = parse_and_query("", super::ELEMENT_QUERY, "func_name");
        assert!(names.is_empty());
        let imports = parse_and_query("", super::IMPORT_QUERY, "import_path");
        assert!(imports.is_empty());
    }
}
