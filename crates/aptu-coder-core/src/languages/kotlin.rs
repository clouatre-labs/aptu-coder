// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2025 Hugues Clouatre and contributors

/// Tree-sitter query for extracting Kotlin elements (functions and classes).
pub const ELEMENT_QUERY: &str = r"
(function_declaration
  name: (identifier) @function_name) @function
(class_declaration
  name: (identifier) @class_name) @class
(object_declaration
  name: (identifier) @object_name) @class
";

/// Tree-sitter query for extracting function calls.
pub const CALL_QUERY: &str = r"
(call_expression
  (identifier) @call)
";

/// Tree-sitter query for extracting type references.
pub const REFERENCE_QUERY: &str = r"
(identifier) @type_ref
";

/// Tree-sitter query for extracting Kotlin imports.
pub const IMPORT_QUERY: &str = r"
(import) @import_path
";

/// Tree-sitter query for extracting definition and use sites.
pub const DEFUSE_QUERY: &str = r"
(property_declaration
  name: (simple_identifier) @write.property)
(simple_identifier) @read.usage
";

use tree_sitter::Node;

use crate::languages::get_node_text;

/// Extract inheritance information from a Kotlin class node.
#[must_use]
pub fn extract_inheritance(node: &Node, source: &str) -> Vec<String> {
    let mut inherits = Vec::new();

    // Find the delegation_specifiers child of the class node.
    // Grammar: optional(seq(':', $.delegation_specifiers))
    let Some(delegation) = (0..node.child_count())
        .filter_map(|i| node.child(u32::try_from(i).ok()?))
        .find(|n| n.kind() == "delegation_specifiers")
    else {
        return inherits;
    };

    // Each delegation_specifier holds either a constructor_invocation (superclass)
    // or a user_type (interface).
    for spec in (0..delegation.child_count())
        .filter_map(|j| delegation.child(u32::try_from(j).ok()?))
        .filter(|n| n.kind() == "delegation_specifier")
    {
        for spec_child in (0..spec.child_count()).filter_map(|k| spec.child(u32::try_from(k).ok()?))
        {
            match spec_child.kind() {
                "constructor_invocation" => {
                    // Superclass: constructor_invocation = type + value_arguments.
                    // The first child is the type node.
                    if let Some(type_node) = spec_child.child(0)
                        && let Some(text) = get_node_text(&type_node, source)
                    {
                        inherits.push(format!("extends {text}"));
                    }
                }
                "type" | "user_type" => {
                    // Interface: direct type without constructor call.
                    if let Some(text) = get_node_text(&spec_child, source) {
                        inherits.push(format!("implements {text}"));
                    }
                }
                _ => {}
            }
        }
    }

    inherits
}

#[cfg(all(test, feature = "lang-kotlin"))]
mod tests {
    use super::*;
    use tree_sitter::{Parser, StreamingIterator};

    fn parse_kotlin(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .expect("Error loading Kotlin language");
        parser.parse(src, None).expect("Failed to parse Kotlin")
    }

    #[test]
    fn test_element_query_free_function() {
        // Arrange: free function at top level
        let src = "fun greet(name: String): String { return \"Hello, $name\" }";
        let tree = parse_kotlin(src);
        let root = tree.root_node();

        // Act -- verify ELEMENT_QUERY compiles and matches function
        let query = tree_sitter::Query::new(&tree_sitter_kotlin_ng::LANGUAGE.into(), ELEMENT_QUERY)
            .expect("ELEMENT_QUERY must be valid");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, src.as_bytes());

        let mut captured_functions: Vec<String> = Vec::new();
        while let Some(mat) = matches.next() {
            for capture in mat.captures {
                let name = query.capture_names()[capture.index as usize];
                let node = capture.node;
                if name == "function" {
                    if let Some(n) = node.child_by_field_name("name") {
                        captured_functions.push(src[n.start_byte()..n.end_byte()].to_string());
                    }
                }
            }
        }

        // Assert
        assert!(
            captured_functions.contains(&"greet".to_string()),
            "expected greet function, got {:?}",
            captured_functions
        );
    }

    #[test]
    fn test_element_query_method_in_class() {
        // Arrange: method inside a class
        let src = "class Animal { fun eat() {} }";
        let tree = parse_kotlin(src);
        let root = tree.root_node();

        // Act -- verify ELEMENT_QUERY compiles and matches class + method
        let query = tree_sitter::Query::new(&tree_sitter_kotlin_ng::LANGUAGE.into(), ELEMENT_QUERY)
            .expect("ELEMENT_QUERY must be valid");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, src.as_bytes());

        let mut captured_classes: Vec<String> = Vec::new();
        let mut captured_functions: Vec<String> = Vec::new();
        while let Some(mat) = matches.next() {
            for capture in mat.captures {
                let name = query.capture_names()[capture.index as usize];
                let node = capture.node;
                match name {
                    "class" => {
                        if let Some(n) = node.child_by_field_name("name") {
                            captured_classes.push(src[n.start_byte()..n.end_byte()].to_string());
                        }
                    }
                    "function" => {
                        if let Some(n) = node.child_by_field_name("name") {
                            captured_functions.push(src[n.start_byte()..n.end_byte()].to_string());
                        }
                    }
                    _ => {}
                }
            }
        }

        // Assert
        assert!(
            captured_classes.contains(&"Animal".to_string()),
            "expected Animal class, got {:?}",
            captured_classes
        );
        assert!(
            captured_functions.contains(&"eat".to_string()),
            "expected eat function, got {:?}",
            captured_functions
        );
    }

    #[test]
    fn test_call_query() {
        // Arrange: function call
        let src = "fun main() { println(\"hello\") }";
        let tree = parse_kotlin(src);
        let root = tree.root_node();

        // Act -- verify CALL_QUERY compiles and matches call
        let query = tree_sitter::Query::new(&tree_sitter_kotlin_ng::LANGUAGE.into(), CALL_QUERY)
            .expect("CALL_QUERY must be valid");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, src.as_bytes());

        let mut captured_calls: Vec<String> = Vec::new();
        while let Some(mat) = matches.next() {
            for capture in mat.captures {
                let name = query.capture_names()[capture.index as usize];
                if name == "call" {
                    let node = capture.node;
                    captured_calls.push(src[node.start_byte()..node.end_byte()].to_string());
                }
            }
        }

        // Assert
        assert!(
            captured_calls.contains(&"println".to_string()),
            "expected println call, got {:?}",
            captured_calls
        );
    }

    #[test]
    fn test_element_query_class_declarations() {
        // Arrange: various class types (data class is just a class with data modifier)
        let src = "class Dog {} object Singleton {}";
        let tree = parse_kotlin(src);
        let root = tree.root_node();

        // Act -- verify ELEMENT_QUERY matches all declaration types
        let query = tree_sitter::Query::new(&tree_sitter_kotlin_ng::LANGUAGE.into(), ELEMENT_QUERY)
            .expect("ELEMENT_QUERY must be valid");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, src.as_bytes());

        let mut captured_classes: Vec<String> = Vec::new();
        while let Some(mat) = matches.next() {
            for capture in mat.captures {
                let name = query.capture_names()[capture.index as usize];
                let node = capture.node;
                if name == "class" {
                    if let Some(n) = node.child_by_field_name("name") {
                        captured_classes.push(src[n.start_byte()..n.end_byte()].to_string());
                    }
                }
            }
        }

        // Assert
        assert!(
            captured_classes.contains(&"Dog".to_string()),
            "expected Dog class, got {:?}",
            captured_classes
        );
        assert!(
            captured_classes.contains(&"Singleton".to_string()),
            "expected Singleton object, got {:?}",
            captured_classes
        );
    }

    #[test]
    fn test_import_query() {
        // Arrange: import statements
        let src = "import java.util.List\nimport kotlin.io.println";
        let tree = parse_kotlin(src);
        let root = tree.root_node();

        // Act -- verify IMPORT_QUERY compiles and matches imports
        let query = tree_sitter::Query::new(&tree_sitter_kotlin_ng::LANGUAGE.into(), IMPORT_QUERY)
            .expect("IMPORT_QUERY must be valid");
        let mut cursor = tree_sitter::QueryCursor::new();
        let matches = cursor.matches(&query, root, src.as_bytes());

        let import_count = matches.count();

        // Assert
        assert!(
            import_count >= 2,
            "expected at least 2 imports, got {}",
            import_count
        );
    }

    #[test]
    fn test_extract_inheritance_single_superclass() {
        // Arrange: class with single superclass (constructor invocation with parens)
        let src = "class Dog : Animal() {}";
        let tree = parse_kotlin(src);
        let root = tree.root_node();

        // Act -- find the class_declaration node and call extract_inheritance
        let mut class_node: Option<tree_sitter::Node> = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "class_declaration" {
                class_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }
        let class = class_node.expect("class_declaration not found");
        let bases = extract_inheritance(&class, src);

        // Assert
        assert!(
            bases.iter().any(|b| b.contains("Animal")),
            "expected extends Animal, got {:?}",
            bases
        );
    }

    #[test]
    fn test_extract_inheritance_multiple_interfaces() {
        // Arrange: class with multiple interfaces (no parens)
        let src = "class Dog : Runnable, Comparable<Dog> {}";
        let tree = parse_kotlin(src);
        let root = tree.root_node();

        // Act -- find the class_declaration node and call extract_inheritance
        let mut class_node: Option<tree_sitter::Node> = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "class_declaration" {
                class_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }
        let class = class_node.expect("class_declaration not found");
        let bases = extract_inheritance(&class, src);

        // Assert
        assert!(
            bases.iter().any(|b| b.contains("Runnable")),
            "expected implements Runnable, got {:?}",
            bases
        );
        assert!(
            bases.iter().any(|b| b.contains("Comparable")),
            "expected implements Comparable, got {:?}",
            bases
        );
    }

    #[test]
    fn test_extract_inheritance_mixed() {
        // Arrange: class with superclass and interfaces
        let src = "class Dog : Animal(), Runnable, Comparable<Dog> {}";
        let tree = parse_kotlin(src);
        let root = tree.root_node();

        // Act -- find the class_declaration node and call extract_inheritance
        let mut class_node: Option<tree_sitter::Node> = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "class_declaration" {
                class_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }
        let class = class_node.expect("class_declaration not found");
        let bases = extract_inheritance(&class, src);

        // Assert
        assert!(
            bases.iter().any(|b| b.contains("Animal")),
            "expected extends Animal, got {:?}",
            bases
        );
        assert!(
            bases.iter().any(|b| b.contains("Runnable")),
            "expected implements Runnable, got {:?}",
            bases
        );
        assert!(
            bases.iter().any(|b| b.contains("Comparable")),
            "expected implements Comparable, got {:?}",
            bases
        );
    }
}
