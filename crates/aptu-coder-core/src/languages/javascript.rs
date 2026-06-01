// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
/// Tree-sitter query for extracting JavaScript elements (functions, classes, and related).
pub const ELEMENT_QUERY: &str = r"
(function_declaration) @function
(class_declaration) @class
(method_definition) @function
(generator_function_declaration) @function

";

/// Tree-sitter query for extracting function calls.
pub const CALL_QUERY: &str = r"
(call_expression
  function: (identifier) @call)
(call_expression
  function: (member_expression property: (property_identifier) @call))
";

/// Tree-sitter query for extracting JavaScript imports (ESM and CommonJS).
pub const IMPORT_QUERY: &str = r#"
(import_statement) @import_path
(call_expression
  function: (identifier) @_fn (#eq? @_fn "require")
  arguments: (arguments (string) @import_path))
"#;

/// Tree-sitter query for extracting definition and use sites.
pub const DEFUSE_QUERY: &str = r"
(variable_declarator name: (identifier) @write.declarator)
(assignment_expression left: (identifier) @write.assign)
(augmented_assignment_expression left: (identifier) @writeread.augmented)
(update_expression argument: (identifier) @writeread.update)
(identifier) @read.usage
";

// JavaScript intentionally has no REFERENCE_QUERY. JavaScript's dynamic typing
// makes static type reference extraction low-value: most "type" references in JS
// are just identifiers that appear in many non-type contexts, producing excessive
// false positives with no meaningful signal. The `reference_query` field is set
// to `None` for the JavaScript handler in `mod.rs`.

use tree_sitter::Node;

/// Extract function name from a JavaScript function or method declaration.
#[must_use]
pub fn extract_function_name(node: &Node, source: &str, _lang: &str) -> Option<String> {
    if node.kind() != "function_declaration" && node.kind() != "method_definition" {
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

/// Find receiver type (enclosing class) for a JavaScript method.
#[must_use]
pub fn find_receiver_type(node: &Node, source: &str) -> Option<String> {
    if node.kind() != "method_definition" {
        return None;
    }

    // Walk ancestors to find enclosing class_declaration
    let mut current = *node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "class_declaration" {
            // Found the enclosing class, extract its name
            return parent.child_by_field_name("name").and_then(|n| {
                let end = n.end_byte();
                if end <= source.len() {
                    Some(source[n.start_byte()..end].to_string())
                } else {
                    None
                }
            });
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
    if node.kind() != "method_definition" {
        return None;
    }

    // Verify that the method is inside a class_declaration
    let mut current = *node;
    let mut in_class = false;
    while let Some(parent) = current.parent() {
        if parent.kind() == "class_declaration" {
            in_class = true;
            break;
        }
        current = parent;
    }

    if !in_class {
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

/// Extract inheritance information from a JavaScript class node.
#[must_use]
pub fn extract_inheritance(node: &Node, source: &str) -> Vec<String> {
    let mut inherits = Vec::new();
    // Walk children to find class_heritage node
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(u32::try_from(i).unwrap_or(u32::MAX))
            && child.kind() == "class_heritage"
        {
            // Walk class_heritage children for extends_clause
            for j in 0..child.named_child_count() {
                if let Some(clause) = child.named_child(u32::try_from(j).unwrap_or(u32::MAX))
                    && clause.kind() == "extends_clause"
                    && let Some(value) = clause.child_by_field_name("value")
                {
                    let text = &source[value.start_byte()..value.end_byte()];
                    inherits.push(format!("extends {text}"));
                }
            }
        }
    }
    inherits
}

#[cfg(all(test, feature = "lang-javascript"))]
mod tests {
    use crate::DefUseKind;
    use crate::parser::SemanticExtractor;
    use tree_sitter::Parser;

    fn parse_js(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .unwrap();
        parser.parse(src, None).unwrap()
    }

    fn find_node_by_kind<'a>(
        node: tree_sitter::Node<'a>,
        kind: &str,
    ) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX))
                && let Some(found) = find_node_by_kind(child, kind)
            {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_extract_function_name() {
        // Arrange: free function declaration
        let src = "function foo() {}";
        let tree = parse_js(src);
        let root = tree.root_node();

        // Find function_declaration node
        let func_node =
            find_node_by_kind(root, "function_declaration").expect("expected function_declaration");

        // Act
        let result = super::extract_function_name(&func_node, src, "javascript");

        // Assert
        assert_eq!(result, Some("foo".to_string()));
    }

    #[test]
    fn test_extract_method_name() {
        // Arrange: method inside a class
        let src = "class C { bar() {} }";
        let tree = parse_js(src);
        let root = tree.root_node();

        // Find method_definition node
        let method_node =
            find_node_by_kind(root, "method_definition").expect("expected method_definition");

        // Act
        let result = super::extract_function_name(&method_node, src, "javascript");

        // Assert
        assert_eq!(result, Some("bar".to_string()));
    }

    #[test]
    fn test_find_receiver_type() {
        // Arrange: method inside a class
        let src = "class MyClass { baz() {} }";
        let tree = parse_js(src);
        let root = tree.root_node();

        // Find method_definition node
        let method_node =
            find_node_by_kind(root, "method_definition").expect("expected method_definition");

        // Act
        let result = super::find_receiver_type(&method_node, src);

        // Assert
        assert_eq!(result, Some("MyClass".to_string()));
    }

    #[test]
    fn test_find_method_for_receiver() {
        // Arrange: method inside a class
        let src = "class C { qux() {} }";
        let tree = parse_js(src);
        let root = tree.root_node();

        // Find method_definition node
        let method_node =
            find_node_by_kind(root, "method_definition").expect("expected method_definition");

        // Act
        let result = super::find_method_for_receiver(&method_node, src, None);

        // Assert
        assert_eq!(result, Some("qux".to_string()));
    }

    #[test]
    fn test_function_declaration() {
        let src = "function greet() { return 42; }";
        let tree = parse_js(src);
        let root = tree.root_node();
        let func = find_node_by_kind(root, "function_declaration");
        assert!(func.is_some(), "expected to find function_declaration");
    }

    #[test]
    fn test_arrow_function() {
        let src = "const add = (a, b) => a + b;";
        let tree = parse_js(src);
        let root = tree.root_node();
        let arrow = find_node_by_kind(root, "arrow_function");
        assert!(arrow.is_some(), "expected to find arrow_function");
    }

    #[test]
    fn test_class_declaration() {
        let src = "class Foo extends Bar { method() {} }";
        let tree = parse_js(src);
        let root = tree.root_node();
        let class = find_node_by_kind(root, "class_declaration");
        assert!(class.is_some(), "expected to find class_declaration");
    }

    #[test]
    fn test_es_import() {
        let src = "import {x} from 'module';";
        let tree = parse_js(src);
        let root = tree.root_node();
        let import = find_node_by_kind(root, "import_statement");
        assert!(import.is_some(), "expected to find import_statement");
    }

    #[test]
    fn test_commonjs_require() {
        let src = "const lib = require('lib');";
        let tree = parse_js(src);
        let root = tree.root_node();
        let call = find_node_by_kind(root, "call_expression");
        assert!(call.is_some(), "expected to find call_expression");
    }

    #[test]
    fn test_defuse_query_write_site() {
        // Arrange
        let src = "let y = 10;\n";
        let sites =
            SemanticExtractor::extract_def_use_for_file(src, "javascript", "y", "test.js", None);
        assert!(!sites.is_empty(), "defuse sites should not be empty");
        let has_write = sites.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(has_write, "should contain a Write DefUseSite");
    }
}
