// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
/// Tree-sitter query for extracting Java elements (methods and classes).
pub const ELEMENT_QUERY: &str = r"
(method_declaration
  name: (identifier) @method_name) @function
(class_declaration
  name: (identifier) @class_name) @class
(interface_declaration
  name: (identifier) @interface_name) @class
(enum_declaration
  name: (identifier) @enum_name) @class
";

/// Tree-sitter query for extracting function calls.
pub const CALL_QUERY: &str = r"
(method_invocation
  name: (identifier) @call)
";

/// Tree-sitter query for extracting type references.
pub const REFERENCE_QUERY: &str = r"
(type_identifier) @type_ref
";

/// Tree-sitter query for extracting Java imports.
pub const IMPORT_QUERY: &str = r"
(import_declaration) @import_path
";

/// Tree-sitter query for extracting definition and use sites.
pub const DEFUSE_QUERY: &str = r"
(local_variable_declaration declarator: (variable_declarator name: (identifier) @write.local))
(assignment_expression left: (identifier) @write.assign)
(update_expression (identifier) @writeread.update)
(identifier) @read.usage
";

use tree_sitter::Node;

/// Extract function name from a Java method declaration.
#[must_use]
pub fn extract_function_name(node: &Node, source: &str, _lang: &str) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }
    node.child_by_field_name("name").and_then(|n| {
        let end = n.end_byte();
        if end <= source.len() {
            Some(source[n.start_byte()..end].to_string())
        } else {
            None
        }
    })
}

/// Find receiver type (enclosing class/interface/enum) for a Java method.
#[must_use]
pub fn find_receiver_type(node: &Node, source: &str) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }

    // Walk ancestors to find enclosing class, interface, or enum
    let mut current = *node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                // Found the enclosing type, extract its name
                return parent.child_by_field_name("name").and_then(|n| {
                    let end = n.end_byte();
                    if end <= source.len() {
                        Some(source[n.start_byte()..end].to_string())
                    } else {
                        None
                    }
                });
            }
            _ => {
                current = parent;
            }
        }
    }

    None
}

/// Find method name when inside a class/interface/enum body.
#[must_use]
pub fn find_method_for_receiver(
    node: &Node,
    source: &str,
    _depth: Option<usize>,
) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }

    // Verify that the method is inside a class, interface, or enum
    let mut current = *node;
    let mut in_type_body = false;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                in_type_body = true;
                break;
            }
            _ => {
                current = parent;
            }
        }
    }

    if !in_type_body {
        return None;
    }

    // Return the method name
    node.child_by_field_name("name").and_then(|n| {
        let end = n.end_byte();
        if end <= source.len() {
            Some(source[n.start_byte()..end].to_string())
        } else {
            None
        }
    })
}

/// Extract inheritance information from a Java class node.
#[must_use]
pub fn extract_inheritance(node: &Node, source: &str) -> Vec<String> {
    let mut inherits = Vec::new();

    // Extract superclass (extends)
    if let Some(superclass) = node.child_by_field_name("superclass") {
        for i in 0..superclass.named_child_count() {
            if let Some(child) = superclass.named_child(u32::try_from(i).unwrap_or(u32::MAX))
                && child.kind() == "type_identifier"
            {
                let text = &source[child.start_byte()..child.end_byte()];
                inherits.push(format!("extends {text}"));
            }
        }
    }

    // Extract interfaces (implements)
    if let Some(interfaces) = node.child_by_field_name("interfaces") {
        for i in 0..interfaces.named_child_count() {
            if let Some(type_list) = interfaces.named_child(u32::try_from(i).unwrap_or(u32::MAX)) {
                for j in 0..type_list.named_child_count() {
                    if let Some(type_node) =
                        type_list.named_child(u32::try_from(j).unwrap_or(u32::MAX))
                        && type_node.kind() == "type_identifier"
                    {
                        let text = &source[type_node.start_byte()..type_node.end_byte()];
                        inherits.push(format!("implements {text}"));
                    }
                }
            }
        }
    }

    inherits
}

#[cfg(all(test, feature = "lang-java"))]
mod tests {
    use super::*;
    use crate::DefUseKind;
    use crate::parser::SemanticExtractor;
    use tree_sitter::{Parser, StreamingIterator};

    fn parse_java(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .expect("Error loading Java language");
        parser.parse(src, None).expect("Failed to parse Java")
    }

    #[test]
    fn test_extract_function_name() {
        // Arrange: method inside a class
        let src = "class C { void foo() {} }";
        let tree = parse_java(src);
        let root = tree.root_node();

        // Find method_declaration node using stack traversal
        let mut method_node = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "method_declaration" {
                method_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }
        let method_node = method_node.expect("expected method_declaration");

        // Act
        let result = extract_function_name(&method_node, src, "java");

        // Assert
        assert_eq!(result, Some("foo".to_string()));
    }

    #[test]
    fn test_find_receiver_type() {
        // Arrange: method inside a class
        let src = "class MyClass { void bar() {} }";
        let tree = parse_java(src);
        let root = tree.root_node();

        // Find method_declaration node using stack traversal
        let mut method_node = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "method_declaration" {
                method_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }
        let method_node = method_node.expect("expected method_declaration");

        // Act
        let result = find_receiver_type(&method_node, src);

        // Assert
        assert_eq!(result, Some("MyClass".to_string()));
    }

    #[test]
    fn test_find_method_for_receiver() {
        // Arrange: method inside a class
        let src = "class C { void baz() {} }";
        let tree = parse_java(src);
        let root = tree.root_node();

        // Find method_declaration node using stack traversal
        let mut method_node = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "method_declaration" {
                method_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }
        let method_node = method_node.expect("expected method_declaration");

        // Act
        let result = find_method_for_receiver(&method_node, src, None);

        // Assert
        assert_eq!(result, Some("baz".to_string()));
    }

    #[test]
    fn test_java_element_query_happy_path() {
        // Arrange
        let src = "class Animal { void eat() {} }";
        let tree = parse_java(src);
        let root = tree.root_node();

        // Act -- verify ELEMENT_QUERY compiles and matches class + method
        let query = tree_sitter::Query::new(&tree_sitter_java::LANGUAGE.into(), ELEMENT_QUERY)
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
    fn test_java_extract_inheritance() {
        // Arrange
        let src = "class Dog extends Animal implements ICanRun, ICanSwim {}";
        let tree = parse_java(src);
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
            bases.iter().any(|b| b.contains("ICanRun")),
            "expected implements ICanRun, got {:?}",
            bases
        );
        assert!(
            bases.iter().any(|b| b.contains("ICanSwim")),
            "expected implements ICanSwim, got {:?}",
            bases
        );
    }

    #[test]
    fn test_defuse_query_write_site() {
        // Arrange
        let src = "class C { void m() { int z = 5; } }\n";
        let sites =
            SemanticExtractor::extract_def_use_for_file(src, "java", "z", "test.java", None);
        assert!(!sites.is_empty(), "defuse sites should not be empty");
        let has_write = sites.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(has_write, "should contain a Write DefUseSite");
    }
}
