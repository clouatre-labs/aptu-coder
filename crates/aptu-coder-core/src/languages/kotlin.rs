// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

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
/// Write-site patterns capture the identifier node within property declarations:
/// - (single): captures `val x = ...` or `var x = ...` via variable_declaration
/// - (multi): captures `val (a, b) = ...` via multi_variable_declaration
pub const DEFUSE_QUERY: &str = r"
; write site: val/var x = ... (single variable declaration)
(property_declaration
  (variable_declaration
    (identifier) @write.property))
; write site: val (a, b) = ... (destructuring declaration)
(property_declaration
  (multi_variable_declaration
    (variable_declaration
      (identifier) @write.property)))
; read site: any identifier reference -- intentionally broad, consistent with Python/Go/Rust patterns
(identifier) @read.usage
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

/// Extract the function name from a Kotlin `function_declaration` node.
#[must_use]
pub fn extract_function_name(node: &Node, source: &str, _lang: &str) -> Option<String> {
    if node.kind() != "function_declaration" {
        return None;
    }
    node.child_by_field_name("name")
        .and_then(|n| get_node_text(&n, source))
}

/// Find the receiver type (enclosing class or object) for a Kotlin function.
///
/// Returns `None` for top-level functions (including extension functions) and
/// functions whose only enclosing type is a `companion_object`.
#[must_use]
pub fn find_receiver_type(node: &Node, source: &str) -> Option<String> {
    if node.kind() != "function_declaration" {
        return None;
    }
    let mut current = *node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration" | "object_declaration" => {
                return parent
                    .child_by_field_name("name")
                    .and_then(|n| get_node_text(&n, source));
            }
            _ => {
                current = parent;
            }
        }
    }
    None
}

/// Find the method name when a function lives inside a named type body.
///
/// Returns `None` for top-level functions and functions inside `companion_object`
/// that have no enclosing `class_declaration` or `object_declaration`.
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
        match parent.kind() {
            "class_declaration" | "object_declaration" => {
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
    node.child_by_field_name("name")
        .and_then(|n| get_node_text(&n, source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::{Parser, StreamingIterator};

    fn find_node<'a>(root: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
        if root.kind() == kind {
            return Some(root);
        }
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if let Some(n) = find_node(child, kind) {
                return Some(n);
            }
        }
        None
    }

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

    #[test]
    fn test_extract_function_name_free_function() {
        let src = "fun greet() {}";
        let tree = parse_kotlin(src);
        let root = tree.root_node();
        let node = find_node(root, "function_declaration").expect("function_declaration not found");
        let result = extract_function_name(&node, src, "kotlin");
        assert_eq!(result, Some("greet".to_string()));
    }

    #[test]
    fn test_extract_function_name_method_in_class() {
        let src = "class Foo { fun bar() {} }";
        let tree = parse_kotlin(src);
        let root = tree.root_node();
        // find the inner function_declaration (bar), not the class
        let class_node = find_node(root, "class_declaration").expect("class_declaration not found");
        let node =
            find_node(class_node, "function_declaration").expect("function_declaration not found");
        let result = extract_function_name(&node, src, "kotlin");
        assert_eq!(result, Some("bar".to_string()));
    }

    #[test]
    fn test_find_receiver_type_top_level_returns_none() {
        let src = "fun greet() {}";
        let tree = parse_kotlin(src);
        let root = tree.root_node();
        let node = find_node(root, "function_declaration").expect("function_declaration not found");
        let result = find_receiver_type(&node, src);
        assert_eq!(result, None);
    }

    #[test]
    fn test_find_receiver_type_method_in_class() {
        let src = "class Foo { fun bar() {} }";
        let tree = parse_kotlin(src);
        let root = tree.root_node();
        let class_node = find_node(root, "class_declaration").expect("class_declaration not found");
        let node =
            find_node(class_node, "function_declaration").expect("function_declaration not found");
        let result = find_receiver_type(&node, src);
        assert_eq!(result, Some("Foo".to_string()));
    }

    #[test]
    fn test_find_receiver_type_extension_function_returns_none() {
        let src = "fun String.greet() {}";
        let tree = parse_kotlin(src);
        let root = tree.root_node();
        let node = find_node(root, "function_declaration").expect("function_declaration not found");
        let result = find_receiver_type(&node, src);
        assert_eq!(result, None);
    }

    #[test]
    fn test_find_method_for_receiver_top_level_returns_none() {
        let src = "fun greet() {}";
        let tree = parse_kotlin(src);
        let root = tree.root_node();
        let node = find_node(root, "function_declaration").expect("function_declaration not found");
        let result = find_method_for_receiver(&node, src, None);
        assert_eq!(result, None);
    }

    #[test]
    fn test_find_method_for_receiver_method_in_class() {
        let src = "class Foo { fun bar() {} }";
        let tree = parse_kotlin(src);
        let root = tree.root_node();
        let class_node = find_node(root, "class_declaration").expect("class_declaration not found");
        let node =
            find_node(class_node, "function_declaration").expect("function_declaration not found");
        let result = find_method_for_receiver(&node, src, None);
        assert_eq!(result, Some("bar".to_string()));
    }

    #[test]
    fn test_defuse_kotlin_val_declaration() {
        // Arrange: val declaration with write and read
        let source = r#"
fun main() {
    val x = 42
    val y = x + 1
}
"#;
        // Act
        let sites = crate::parser::SemanticExtractor::extract_def_use_for_file(
            source,
            "kotlin",
            "x",
            "src/main.kt",
            None,
        );

        // Assert
        assert!(
            !sites.is_empty(),
            "expected at least one def-use site for 'x'"
        );
        let has_write = sites
            .iter()
            .any(|s| s.kind == crate::types::DefUseKind::Write);
        let has_read = sites
            .iter()
            .any(|s| s.kind == crate::types::DefUseKind::Read);
        assert!(has_write, "expected a write site for 'x'");
        assert!(has_read, "expected a read site for 'x'");
    }

    #[test]
    fn test_defuse_kotlin_multi_variable_declaration() {
        // Arrange: destructuring assignment with multiple write sites
        let source = r#"
fun main() {
    val (a, b) = Pair(1, 2)
}
"#;
        // Act
        let sites_a = crate::parser::SemanticExtractor::extract_def_use_for_file(
            source,
            "kotlin",
            "a",
            "src/main.kt",
            None,
        );

        // Assert
        assert!(
            !sites_a.is_empty(),
            "expected at least one def-use site for 'a'"
        );
        let has_write_a = sites_a
            .iter()
            .any(|s| s.kind == crate::types::DefUseKind::Write);
        assert!(has_write_a, "expected a write site for 'a'");
    }
}
