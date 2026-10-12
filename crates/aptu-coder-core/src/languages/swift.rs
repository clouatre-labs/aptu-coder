// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
/// Tree-sitter query for extracting Swift elements (functions and types).
///
/// Note: in tree-sitter-swift 0.7.x, classes, structs, actors, enums and
/// extensions all parse as `class_declaration` (distinguished by the
/// `declaration_kind` child), while protocols use `protocol_declaration`.
pub const ELEMENT_QUERY: &str = r"
(function_declaration
  name: (simple_identifier) @func_name) @function
(class_declaration
  name: (type_identifier) @class_name) @class
(protocol_declaration
  name: (type_identifier) @class_name) @class
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
/// Write-site patterns capture the identifier node within property declarations:
/// `let x = ...` or `var x = ...`.
pub const DEFUSE_QUERY: &str = r"
; write site: let/var x = ...
(property_declaration
  name: (pattern
    (simple_identifier) @write.property))
; read site: any simple identifier reference -- intentionally broad, consistent
; with the Kotlin/Python/Go/Rust patterns
(simple_identifier) @read.usage
";

use tree_sitter::Node;

use crate::languages::get_node_text;

/// Extract inheritance information from a Swift class/struct/protocol node.
///
/// Walks `inheritance_specifier` children and reads their `inherits_from`
/// field (a `user_type` in practice), collecting the base type names.
#[must_use]
pub fn extract_inheritance(node: &Node, source: &str) -> Vec<String> {
    let mut inherits = Vec::new();

    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(u32::try_from(i).unwrap_or(u32::MAX))
            && child.kind() == "inheritance_specifier"
            && let Some(extends) = child.child_by_field_name("inherits_from")
        {
            // user_type wraps one or more type_identifier children
            if extends.kind() == "type_identifier" {
                if let Some(text) = get_node_text(&extends, source) {
                    inherits.push(text);
                }
            } else {
                for j in 0..extends.named_child_count() {
                    if let Some(part) = extends.named_child(u32::try_from(j).unwrap_or(u32::MAX))
                        && part.kind() == "type_identifier"
                        && let Some(text) = get_node_text(&part, source)
                    {
                        inherits.push(text);
                    }
                }
            }
        }
    }

    inherits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::SemanticExtractor;
    use tree_sitter::{Parser, StreamingIterator};

    fn parse_swift(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("Error loading Swift language");
        parser.parse(src, None).expect("Failed to parse Swift")
    }

    fn find_node<'t>(
        root: tree_sitter::Node<'t>,
        kind: &str,
        name: &str,
        src: &str,
    ) -> tree_sitter::Node<'t> {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == kind
                && let Some(n) = node.child_by_field_name("name")
                && let Some(text) = get_node_text(&n, src)
                && text == name
            {
                return node;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    stack.push(child);
                }
            }
        }
        panic!("node {kind} named {name} not found");
    }

    #[test]
    fn test_element_query_class_and_function() {
        // Arrange: class, struct, actor, enum, extension, protocol, and a function
        let src = r#"
import Foundation

class Dog: Animal, Pet { func bark() {} }
struct Cat: Pet { }
actor Server { func start() {} }
enum Color { case red }
extension String { func trim() {} }
protocol Pet { func feed() }
"#;
        let tree = parse_swift(src);
        let root = tree.root_node();

        let query = tree_sitter::Query::new(&tree_sitter_swift::LANGUAGE.into(), ELEMENT_QUERY)
            .expect("ELEMENT_QUERY must compile");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, src.as_bytes());

        let mut classes: Vec<String> = Vec::new();
        let mut functions: Vec<String> = Vec::new();
        while let Some(mat) = matches.next() {
            for capture in mat.captures() {
                let name = query.capture_names()[capture.index as usize];
                let node = capture.node;
                match name {
                    "class" | "function" => {
                        if let Some(n) = node.child_by_field_name("name")
                            && let Some(text) = get_node_text(&n, src)
                        {
                            if name == "class" {
                                classes.push(text);
                            } else {
                                functions.push(text);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Assert: non-empty names via @class_name and @func_name.
        // Note: `extension String` is excluded because the grammar parses a
        // user_type-extension name differently from a type_identifier.
        for expected in ["Dog", "Cat", "Server", "Color", "Pet"] {
            assert!(
                classes.iter().any(|c| c == expected),
                "expected class {expected}, got {classes:?}"
            );
        }
        for expected in ["bark", "start", "trim"] {
            assert!(
                functions.iter().any(|f| f == expected),
                "expected function {expected}, got {functions:?}"
            );
        }
    }

    #[test]
    fn test_extract_inheritance_class_extends() {
        // Arrange
        let src = "class A: B {}";
        let tree = parse_swift(src);
        let class_node = find_node(tree.root_node(), "class_declaration", "A", src);

        // Act
        let bases = extract_inheritance(&class_node, src);

        // Assert
        assert_eq!(bases, vec!["B".to_string()]);
    }

    #[test]
    fn test_extract_inheritance_protocol_conformance() {
        // Arrange
        let src = "struct S: P {}";
        let tree = parse_swift(src);
        let struct_node = find_node(tree.root_node(), "class_declaration", "S", src);

        // Act
        let bases = extract_inheritance(&struct_node, src);

        // Assert
        assert_eq!(bases, vec!["P".to_string()]);
    }

    #[test]
    fn test_extract_inheritance_multiple_bases() {
        // Arrange
        let src = "class Vehicle {}
class Car: Vehicle, Drivable, Insurable {}";
        let tree = parse_swift(src);
        let car = find_node(tree.root_node(), "class_declaration", "Car", src);

        // Act
        let bases = extract_inheritance(&car, src);

        // Assert
        assert_eq!(
            bases,
            vec![
                "Vehicle".to_string(),
                "Drivable".to_string(),
                "Insurable".to_string()
            ]
        );
    }

    #[test]
    fn test_extract_inheritance_no_bases() {
        // Arrange: no inheritance at all
        let src = "struct Plain { var x = 1 }";
        let tree = parse_swift(src);
        let node = find_node(tree.root_node(), "class_declaration", "Plain", src);

        // Act
        let bases = extract_inheritance(&node, src);

        // Assert
        assert!(bases.is_empty());
    }

    #[test]
    fn test_import_query() {
        // Arrange
        let src = "import Foundation\nimport UIKit\nlet x = 1\n";
        let tree = parse_swift(src);

        let query = tree_sitter::Query::new(&tree_sitter_swift::LANGUAGE.into(), IMPORT_QUERY)
            .expect("IMPORT_QUERY must compile");
        let mut cursor = tree_sitter::QueryCursor::new();
        let count = cursor
            .matches(&query, tree.root_node(), src.as_bytes())
            .count();

        // Assert
        assert_eq!(count, 2, "expected 2 import matches");
    }

    #[test]
    fn test_round_trip_parse_no_panics() {
        // Arrange: complex nested constructs
        let src = r#"
import Foundation

public final class NetworkManager: BaseService {
    private var session: URLSession = URLSession.shared

    public func fetch<T: Decodable>(_ url: URL) async throws -> T {
        do {
            let (data, _) = try await session.data(from: url)
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw NetworkError.badResponse
        }
    }

    private enum NetworkError: Error { case badResponse }
}

extension NetworkManager: CustomStringConvertible {
    public var description: String { "NetworkManager" }
}
"#;
        // Act: parse and run element extraction; must not panic
        let tree = parse_swift(src);
        let query = tree_sitter::Query::new(&tree_sitter_swift::LANGUAGE.into(), ELEMENT_QUERY)
            .expect("ELEMENT_QUERY must compile");
        let mut cursor = tree_sitter::QueryCursor::new();
        let count = cursor
            .matches(&query, tree.root_node(), src.as_bytes())
            .count();

        // Assert
        assert!(count > 0, "expected matches on complex fixture");

        // Empty source edge case: no declarations, no panics
        let empty = parse_swift("let x = 1\n");
        let mut cursor = tree_sitter::QueryCursor::new();
        let count = cursor
            .matches(&query, empty.root_node(), "let x = 1\n".as_bytes())
            .count();
        assert_eq!(count, 0, "expected no element matches on trivial source");
    }

    #[test]
    fn test_defuse_query_write_site() {
        // Arrange
        let src = "class C { var count = 5 }";
        let sites =
            SemanticExtractor::extract_def_use_for_file(src, "swift", "count", "t.swift", None);

        // Assert
        assert!(!sites.is_empty(), "defuse sites should not be empty");
        assert!(
            sites
                .iter()
                .any(|s| matches!(s.kind, crate::DefUseKind::Write)),
            "should contain a Write DefUseSite"
        );
    }
}
