// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Swift language handler: queries and inheritance extraction.

/// Tree-sitter query for extracting Swift elements (classes, structs, functions, protocols).
pub const ELEMENT_QUERY: &str = r"
(class_declaration
  name: (type_identifier) @class_name) @class
(function_declaration
  name: (simple_identifier) @function_name) @function
(protocol_declaration
  name: (type_identifier) @protocol_name) @class
";

/// Tree-sitter query for extracting function calls.
pub const CALL_QUERY: &str = r"
(call_expression
  (simple_identifier) @call)
";

/// Tree-sitter query for extracting type references.
pub const REFERENCE_QUERY: &str = r"
(type_identifier) @type_ref
";

/// Tree-sitter query for extracting Swift imports.
pub const IMPORT_QUERY: &str = r"
(import_declaration) @import_path
";

/// Tree-sitter query for extracting definition and use sites.
pub const DEFUSE_QUERY: &str = r"
(property_declaration
  (pattern
    (simple_identifier) @write.property))
(simple_identifier) @read.usage
";

use tree_sitter::Node;

use crate::languages::get_node_text;

/// Extract inheritance information from a Swift class declaration node.
///
/// Walks `inheritance_specifier` children of the class declaration. The first
/// specifier is conventionally the superclass; subsequent specifiers are
/// protocol conformances. Results use `extends`/`conforms` prefixes.
#[must_use]
pub fn extract_inheritance(node: &Node, source: &str) -> Vec<String> {
    if node.kind() != "class_declaration" {
        return Vec::new();
    }

    let mut inherits = Vec::new();
    let mut seen_superclass = false;

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i)
            && child.kind() == "inheritance_specifier"
            && let Some(type_node) = child.named_child(0)
            && let Some(text) = get_node_text(&type_node, source)
        {
            // The superclass may carry generic arguments; keep the base name only.
            let base = text.split('<').next().unwrap_or(&text).trim().to_string();
            if seen_superclass {
                inherits.push(format!("conforms {base}"));
            } else {
                inherits.push(format!("extends {base}"));
                seen_superclass = true;
            }
        }
    }

    inherits
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::{Parser, StreamingIterator};

    fn parse_swift(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("Error loading Swift language");
        parser.parse(src, None).expect("Failed to parse Swift")
    }

    #[test]
    fn queries_compile() {
        let lang = tree_sitter_swift::LANGUAGE.into();
        tree_sitter::Query::new(&lang, ELEMENT_QUERY).expect("ELEMENT_QUERY must be valid");
        tree_sitter::Query::new(&lang, CALL_QUERY).expect("CALL_QUERY must be valid");
        tree_sitter::Query::new(&lang, REFERENCE_QUERY).expect("REFERENCE_QUERY must be valid");
        tree_sitter::Query::new(&lang, IMPORT_QUERY).expect("IMPORT_QUERY must be valid");
        tree_sitter::Query::new(&lang, DEFUSE_QUERY).expect("DEFUSE_QUERY must be valid");
    }

    #[test]
    fn set_language_smoke() {
        let tree = parse_swift("class Foo {}");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn element_query_matches_class_and_function() {
        let src = "class Greeter { func greet() { print(\"hi\") } }";
        let tree = parse_swift(src);
        let query = tree_sitter::Query::new(&tree_sitter_swift::LANGUAGE.into(), ELEMENT_QUERY)
            .expect("ELEMENT_QUERY must be valid");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), src.as_bytes());

        let mut classes: Vec<String> = Vec::new();
        let mut functions: Vec<String> = Vec::new();
        while let Some(mat) = matches.next() {
            for capture in mat.captures() {
                let name = query.capture_names()[capture.index as usize];
                let node = capture.node;
                if let Some(n) = node.child_by_field_name("name") {
                    let text = src[n.start_byte()..n.end_byte()].to_string();
                    match name {
                        "class" => classes.push(text),
                        "function" => functions.push(text),
                        _ => {}
                    }
                }
            }
        }
        assert!(classes.contains(&"Greeter".to_string()));
        assert!(functions.contains(&"greet".to_string()));
    }

    fn find_class<'a>(root: Node<'a>) -> Option<Node<'a>> {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "class_declaration" {
                return Some(node);
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    stack.push(child);
                }
            }
        }
        None
    }

    #[test]
    fn extract_inheritance_superclass_and_protocols() {
        let src = "class Foo: Bar, Baz { func f() {} }";
        let tree = parse_swift(src);
        let class = find_class(tree.root_node()).expect("class_declaration not found");
        let bases = extract_inheritance(&class, src);
        assert!(bases.contains(&"extends Bar".to_string()), "got {bases:?}");
        assert!(bases.contains(&"conforms Baz".to_string()), "got {bases:?}");
    }

    #[test]
    fn extract_inheritance_no_inheritance() {
        let src = "struct Plain { var x: Int = 0 }";
        let tree = parse_swift(src);
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "class_declaration" {
                assert!(extract_inheritance(&node, src).is_empty());
                return;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    stack.push(child);
                }
            }
        }
        // No class_declaration in a struct-only snippet: nothing to extract.
    }
}
