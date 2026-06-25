// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! YAML language handler for tree-sitter-yaml.
//!
//! Extracts top-level mapping keys as function-equivalent elements.
//! YAML has no import concept; the import query is left empty.

/// Tree-sitter query for extracting YAML block mapping keys as elements.
///
/// Each `block_mapping_pair` node's `key` field is captured as the element name.
pub const ELEMENT_QUERY: &str = r"
(block_mapping_pair key: (_) @func_name) @function
";

/// Tree-sitter call query for YAML (empty -- no call sites in YAML).
pub const CALL_QUERY: &str = "";

#[cfg(test)]
mod tests {
    use tree_sitter::{Parser, StreamingIterator};

    fn parse_and_query(src: &str, query_str: &str, capture_name: &str) -> Vec<String> {
        let language = tree_sitter_yaml::LANGUAGE;
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

    /// YAML block mapping keys are extracted as elements.
    #[test]
    fn test_yaml_block_mapping_element_extraction() {
        let src = "name: my-project\nversion: 1.0.0\ndescription: A test project\n";
        let names = parse_and_query(src, super::ELEMENT_QUERY, "func_name");
        assert_eq!(names, vec!["name", "version", "description"]);
    }

    /// An empty YAML file returns empty analysis without errors.
    #[test]
    fn test_yaml_empty_file() {
        let names = parse_and_query("", super::ELEMENT_QUERY, "func_name");
        assert!(names.is_empty());
    }

    /// YAML multi-document stream extracts keys from each document.
    #[test]
    fn test_yaml_multi_document_stream() {
        let src = "---\nfoo: bar\nbaz: qux\n---\nalpha: beta\n";
        let names = parse_and_query(src, super::ELEMENT_QUERY, "func_name");
        assert!(names.contains(&"foo".to_owned()));
        assert!(names.contains(&"baz".to_owned()));
        assert!(names.contains(&"alpha".to_owned()));
    }
}
