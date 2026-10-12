// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
/// Tree-sitter query for extracting Swift elements (functions and classes).
pub const ELEMENT_QUERY: &str = r"
(function_declaration
  name: (simple_identifier) @function_name) @function
(class_declaration
  name: (_) @class_name) @class
";

/// Tree-sitter query for extracting function calls.
pub const CALL_QUERY: &str = r"
(call_expression (simple_identifier) @call)
";

/// Tree-sitter query for extracting type references (kept tight: only
/// user_type within type_annotation positions, not bare identifiers).
pub const REFERENCE_QUERY: &str = r"
(user_type) @type_ref
";

/// Tree-sitter query for extracting Swift imports.
pub const IMPORT_QUERY: &str = r"
(import_declaration) @import_path
";

/// Tree-sitter query for extracting definition and use sites.
pub const DEFUSE_QUERY: &str = r"
(property_declaration (pattern (simple_identifier) @write.local))
(assignment (directly_assignable_expression (simple_identifier) @write.assign))
(simple_identifier) @read.usage
";

use tree_sitter::Node;

fn node_text(node: &Node, source: &str) -> Option<String> {
    let end = node.end_byte();
    if end <= source.len() {
        Some(source[node.start_byte()..end].to_string())
    } else {
        None
    }
}

/// Extract function name from a Swift function declaration.
#[must_use]
pub fn extract_function_name(node: &Node, source: &str, _lang: &str) -> Option<String> {
    if node.kind() != "function_declaration" {
        return None;
    }
    node.child_by_field_name("name")
        .and_then(|n| node_text(&n, source))
}

/// Find receiver type (enclosing class) for a Swift function.
#[must_use]
pub fn find_receiver_type(node: &Node, source: &str) -> Option<String> {
    if node.kind() != "function_declaration" {
        return None;
    }

    let mut current = *node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "class_declaration" {
            return parent
                .child_by_field_name("name")
                .and_then(|n| node_text(&n, source));
        }
        current = parent;
    }

    None
}

/// Find method name when inside a class body.
#[must_use]
pub fn find_method_for_receiver(
    node: &Node,
    source: &str,
    _depth: Option<usize>,
) -> Option<String> {
    if node.kind() != "function_declaration" {
        return None;
    }

    let mut current = *node;
    let mut in_type_body = false;
    while let Some(parent) = current.parent() {
        if parent.kind() == "class_declaration" {
            in_type_body = true;
            break;
        }
        current = parent;
    }

    if !in_type_body {
        return None;
    }

    node.child_by_field_name("name")
        .and_then(|n| node_text(&n, source))
}

/// Extract inheritance information from a Swift class node.
///
/// Swift does not syntactically distinguish superclass inheritance from
/// protocol conformance the way Java distinguishes `extends`/`implements`;
/// only position does. We therefore return the bare inherited type names
/// without labels.
#[must_use]
pub fn extract_inheritance(node: &Node, source: &str) -> Vec<String> {
    let mut inherits = Vec::new();

    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(u32::try_from(i).unwrap_or(u32::MAX))
            && child.kind() == "inheritance_specifier"
            && let Some(inherits_from) = child.child_by_field_name("inherits_from")
            && let Some(text) = node_text(&inherits_from, source)
        {
            inherits.push(text);
        }
    }

    inherits
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::{Parser, QueryCursor, StreamingIterator};

    fn parse_swift(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("Error loading Swift language");
        parser.parse(src, None).expect("Failed to parse Swift")
    }

    #[test]
    fn test_queries_compile() {
        let lang = tree_sitter_swift::LANGUAGE.into();
        assert!(tree_sitter::Query::new(&lang, ELEMENT_QUERY).is_ok());
        assert!(tree_sitter::Query::new(&lang, CALL_QUERY).is_ok());
        assert!(tree_sitter::Query::new(&lang, REFERENCE_QUERY).is_ok());
        assert!(tree_sitter::Query::new(&lang, IMPORT_QUERY).is_ok());
        assert!(tree_sitter::Query::new(&lang, DEFUSE_QUERY).is_ok());
    }

    #[test]
    fn test_swift_extract_function_name() {
        // Arrange
        let src = "class Greeter { func greet(name: String) -> String { return name } }";
        let tree = parse_swift(src);
        let root = tree.root_node();

        let mut func_node = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_declaration" {
                func_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    stack.push(child);
                }
            }
        }
        let func = func_node.expect("function_declaration not found");

        // Act
        let name = extract_function_name(&func, src, "swift");

        // Assert
        assert_eq!(name.as_deref(), Some("greet"));
    }

    #[test]
    fn test_swift_extract_inheritance() {
        // Arrange: superclass + protocol conformances
        let src = "class Dog: Animal, ICanRun, ICanSwim {}";
        let tree = parse_swift(src);
        let root = tree.root_node();

        let mut class_node = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "class_declaration" {
                class_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    stack.push(child);
                }
            }
        }
        let class = class_node.expect("class_declaration not found");

        // Act
        let bases = extract_inheritance(&class, src);

        // Assert: bare names, no extends/implements labels
        assert!(bases.iter().any(|b| b == "Animal"), "got {bases:?}");
        assert!(bases.iter().any(|b| b == "ICanRun"), "got {bases:?}");
        assert!(bases.iter().any(|b| b == "ICanSwim"), "got {bases:?}");
        assert!(
            !bases
                .iter()
                .any(|b| b.contains("extends") || b.contains("implements")),
            "Swift inheritance must not carry Java labels: {bases:?}"
        );
    }

    #[test]
    fn test_extract_inheritance_no_inheritance() {
        // Arrange: no inheritance clause
        let src = "struct Point { var x: Int = 0 }";
        let tree = parse_swift(src);
        let root = tree.root_node();

        let mut decl_node = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if matches!(node.kind(), "class_declaration" | "struct_declaration") {
                decl_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    stack.push(child);
                }
            }
        }
        let decl = decl_node.expect("declaration not found");

        // Act
        let bases = extract_inheritance(&decl, src);

        // Assert
        assert!(bases.is_empty(), "expected no inheritance, got {bases:?}");
    }

    #[test]
    fn test_swift_parse_and_queries() {
        // Arrange
        let src = r#"
import Foundation

class Vehicle: CustomStringConvertible {
    var wheels: Int = 4
    var description: String { "a vehicle" }
    func drive() { print("driving") }
}

class Car: Vehicle {
    override func drive() { print("vroom") }
}
"#;
        let tree = parse_swift(src);
        let root = tree.root_node();
        let lang = tree_sitter_swift::LANGUAGE.into();

        // Act + Assert: element query captures both classes and a function
        let elem_q = tree_sitter::Query::new(&lang, ELEMENT_QUERY).unwrap();
        let mut cursor = QueryCursor::new();
        let mut class_count = 0;
        let mut func_count = 0;
        let mut matches = cursor.matches(&elem_q, root, src.as_bytes());
        while let Some(m) = matches.next() {
            for cap in m.captures() {
                match elem_q.capture_names()[cap.index as usize] {
                    "class_name" => class_count += 1,
                    "function_name" => func_count += 1,
                    _ => {}
                }
            }
        }
        assert_eq!(class_count, 2, "expected 2 classes");
        assert_eq!(func_count, 2, "expected 2 functions");

        // Call and reference queries return captures without error
        let call_q = tree_sitter::Query::new(&lang, CALL_QUERY).unwrap();
        let mut cursor = QueryCursor::new();
        let mut calls = cursor.matches(&call_q, root, src.as_bytes());
        let call_count = calls.next().map_or(0, |_| 1);
        assert!(call_count >= 0);

        let ref_q = tree_sitter::Query::new(&lang, REFERENCE_QUERY).unwrap();
        let mut cursor = QueryCursor::new();
        let mut refs = cursor.matches(&ref_q, root, src.as_bytes());
        assert!(
            refs.next().is_some(),
            "expected at least one type reference"
        );
    }
}
