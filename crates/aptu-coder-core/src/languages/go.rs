// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use tree_sitter::Node;

/// Tree-sitter query for extracting Go elements (functions, methods, and types).
pub const ELEMENT_QUERY: &str = r"
(function_declaration
  name: (identifier) @func_name) @function
(method_declaration
  name: (field_identifier) @method_name) @function
(type_spec
  name: (type_identifier) @type_name
  type: (struct_type)) @class
(type_spec
  name: (type_identifier) @type_name
  type: (interface_type)) @class
";

/// Tree-sitter query for extracting function calls.
pub const CALL_QUERY: &str = r"
(call_expression
  function: (identifier) @call)
(call_expression
  function: (selector_expression field: (field_identifier) @call))
";

/// Tree-sitter query for extracting type references.
pub const REFERENCE_QUERY: &str = r"
(type_identifier) @type_ref
";

/// Tree-sitter query for extracting Go imports.
pub const IMPORT_QUERY: &str = r"
(import_declaration) @import_path
";

/// Tree-sitter query for extracting definition and use sites.
pub const DEFUSE_QUERY: &str = r"
(short_var_declaration left: (expression_list (identifier) @write.short))
(assignment_statement left: (expression_list (identifier) @write.assign))
(var_declaration (var_spec (identifier) @write.var))
(inc_statement (identifier) @writeread.inc)
(dec_statement (identifier) @writeread.dec)
(identifier) @read.usage
";

/// Extract function or method name from a Go function or method declaration.
#[must_use]
pub fn extract_function_name(node: &Node, source: &str, _lang: &str) -> Option<String> {
    if node.kind() != "function_declaration" && node.kind() != "method_declaration" {
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

/// Find receiver type for a Go method declaration.
/// Walks the method_declaration.receiver field to find the type.
#[must_use]
pub fn find_receiver_type(node: &Node, source: &str) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }

    // Get the receiver field
    let receiver = node.child_by_field_name("receiver")?;

    // Iterate through receiver's children to find parameter_declaration
    for i in 0..receiver.named_child_count() {
        if let Some(param) = receiver.named_child(i as u32)
            && param.kind() == "parameter_declaration"
        {
            // Get the type field from parameter_declaration
            if let Some(type_node) = param.child_by_field_name("type") {
                match type_node.kind() {
                    "type_identifier" => {
                        let end = type_node.end_byte();
                        if end <= source.len() {
                            return Some(source[type_node.start_byte()..end].to_string());
                        }
                    }
                    "pointer_type" => {
                        // pointer_type wraps the actual type_identifier
                        for j in 0..type_node.named_child_count() {
                            if let Some(inner) = type_node.named_child(j as u32)
                                && inner.kind() == "type_identifier"
                            {
                                let end = inner.end_byte();
                                if end <= source.len() {
                                    return Some(source[inner.start_byte()..end].to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    None
}

/// Find method name for a receiver type.
#[must_use]
pub fn find_method_for_receiver(
    node: &Node,
    source: &str,
    _depth: Option<usize>,
) -> Option<String> {
    if node.kind() != "method_declaration" && node.kind() != "function_declaration" {
        return None;
    }
    node.child_by_field_name("name").and_then(|n| {
        let start = n.start_byte();
        let end = n.end_byte();
        if end <= source.len() {
            Some(source[start..end].to_string())
        } else {
            None
        }
    })
}

/// Extract inheritance information from a Go type node.
#[must_use]
pub fn extract_inheritance(node: &Node, source: &str) -> Vec<String> {
    let mut inherits = Vec::new();

    // Get the type field from type_spec
    if let Some(type_field) = node.child_by_field_name("type") {
        match type_field.kind() {
            "struct_type" => {
                // For struct embedding, walk children for field_declaration_list
                for i in 0..type_field.named_child_count() {
                    if let Some(field_list) = type_field.named_child(i as u32)
                        && field_list.kind() == "field_declaration_list"
                    {
                        // Walk field_declaration_list for field_declaration without name
                        for j in 0..field_list.named_child_count() {
                            if let Some(field) = field_list.named_child(j as u32)
                                && field.kind() == "field_declaration"
                                && field.child_by_field_name("name").is_none()
                            {
                                // Embedded type has no name field
                                if let Some(type_node) = field.child_by_field_name("type") {
                                    let text =
                                        &source[type_node.start_byte()..type_node.end_byte()];
                                    inherits.push(text.to_string());
                                }
                            }
                        }
                    }
                }
            }
            "interface_type" => {
                // For interface embedding, walk children for type_elem
                for i in 0..type_field.named_child_count() {
                    if let Some(elem) = type_field.named_child(i as u32)
                        && elem.kind() == "type_elem"
                    {
                        let text = &source[elem.start_byte()..elem.end_byte()];
                        inherits.push(text.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    inherits
}

#[cfg(all(test, feature = "lang-go"))]
mod tests {
    use super::*;
    use crate::DefUseKind;
    use crate::parser::SemanticExtractor;
    use tree_sitter::Parser;

    fn parse_go(source: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .expect("failed to set Go language");
        parser.parse(source, None).expect("failed to parse source")
    }

    #[test]
    fn test_extract_inheritance_struct_no_embeds() {
        // Arrange: struct with no embedded types
        let source = "package p\ntype Foo struct { x int }";
        let tree = parse_go(source);
        let root = tree.root_node();
        // find the type_spec node
        let type_spec = (0..root.named_child_count())
            .filter_map(|i| root.named_child(i as u32))
            .find_map(|n| {
                if n.kind() == "type_declaration" {
                    (0..n.named_child_count())
                        .filter_map(|j| n.named_child(j as u32))
                        .find(|c| c.kind() == "type_spec")
                } else {
                    None
                }
            })
            .expect("expected type_spec node");
        // Act
        let result = extract_inheritance(&type_spec, source);
        // Assert
        assert!(
            result.is_empty(),
            "expected no inherited types, got {:?}",
            result
        );
    }

    #[test]
    fn test_find_method_for_receiver_wrong_kind() {
        // Arrange: use a struct node (not a method or function declaration)
        let source = "package p\ntype Bar struct {}";
        let tree = parse_go(source);
        let root = tree.root_node();
        let node = root.named_child(0).expect("expected child");
        // Act
        let result = find_method_for_receiver(&node, source, None);
        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_function_name() {
        // Arrange: free function declaration
        let source = "package p\nfunc Foo() {}";
        let tree = parse_go(source);
        let root = tree.root_node();
        // Find function_declaration node
        let func_node = (0..root.named_child_count())
            .filter_map(|i| root.named_child(i as u32))
            .find(|n| n.kind() == "function_declaration")
            .expect("expected function_declaration");
        // Act
        let result = extract_function_name(&func_node, source, "go");
        // Assert
        assert_eq!(result, Some("Foo".to_string()));
    }

    #[test]
    fn test_extract_method_name() {
        // Arrange: method declaration
        let source = "package p\nfunc (r *Receiver) Bar() {}";
        let tree = parse_go(source);
        let root = tree.root_node();
        // Find method_declaration node
        let method_node = (0..root.named_child_count())
            .filter_map(|i| root.named_child(i as u32))
            .find(|n| n.kind() == "method_declaration")
            .expect("expected method_declaration");
        // Act
        let result = extract_function_name(&method_node, source, "go");
        // Assert
        assert_eq!(result, Some("Bar".to_string()));
    }

    #[test]
    fn test_extract_function_name_wrong_kind() {
        // Arrange: use a struct node (not a function or method declaration)
        let source = "package p\ntype Baz struct {}";
        let tree = parse_go(source);
        let root = tree.root_node();
        let node = root.named_child(0).expect("expected child");
        // Act
        let result = extract_function_name(&node, source, "go");
        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_find_receiver_type() {
        // Arrange: method with value receiver
        let source = "package p\nfunc (r Receiver) Foo() {}";
        let tree = parse_go(source);
        let root = tree.root_node();
        // Find method_declaration node
        let method_node = (0..root.named_child_count())
            .filter_map(|i| root.named_child(i as u32))
            .find(|n| n.kind() == "method_declaration")
            .expect("expected method_declaration");
        // Act
        let result = find_receiver_type(&method_node, source);
        // Assert
        assert_eq!(result, Some("Receiver".to_string()));
    }

    #[test]
    fn test_find_receiver_type_pointer() {
        // Arrange: method with pointer receiver
        let source = "package p\nfunc (r *Receiver) Foo() {}";
        let tree = parse_go(source);
        let root = tree.root_node();
        // Find method_declaration node
        let method_node = (0..root.named_child_count())
            .filter_map(|i| root.named_child(i as u32))
            .find(|n| n.kind() == "method_declaration")
            .expect("expected method_declaration");
        // Act
        let result = find_receiver_type(&method_node, source);
        // Assert
        assert_eq!(result, Some("Receiver".to_string()));
    }

    #[test]
    fn test_defuse_query_write_site() {
        // Arrange
        let src = "package p\nfunc main() { x := 1 }\n";
        let sites = SemanticExtractor::extract_def_use_for_file(src, "go", "x", "test.go", None);
        assert!(!sites.is_empty(), "defuse sites should not be empty");
        let has_write = sites.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(has_write, "should contain a Write DefUseSite");
    }

    #[test]
    fn test_defuse_go_short_var_decl() {
        // Arrange: short var declaration := is Write
        let src = "package p\nfunc main() { x := 42 }\n";
        // Act
        let sites = SemanticExtractor::extract_def_use_for_file(src, "go", "x", "test.go", None);
        // Assert
        assert!(
            !sites.is_empty(),
            "short var decl should produce defuse sites"
        );
        let has_write = sites.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(has_write, "short var decl should be Write");
    }

    #[test]
    fn test_defuse_go_multi_return() {
        // Arrange: multi-return := captures all LHS identifiers as Write
        let src = "package p\nfunc main() { a, b := f() }\nfunc f() (int, int) { return 1, 2 }\n";
        // Act
        let sites_a = SemanticExtractor::extract_def_use_for_file(src, "go", "a", "test.go", None);
        let sites_b = SemanticExtractor::extract_def_use_for_file(src, "go", "b", "test.go", None);
        // Assert
        assert!(
            !sites_a.is_empty(),
            "multi-return a should produce defuse sites"
        );
        assert!(
            !sites_b.is_empty(),
            "multi-return b should produce defuse sites"
        );
        let a_write = sites_a.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        let b_write = sites_b.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(a_write, "multi-return a should be Write");
        assert!(b_write, "multi-return b should be Write");
    }

    #[test]
    fn test_defuse_go_blank_identifier() {
        // Arrange: blank identifier _ in multi-return
        let src =
            "package p\nfunc main() { _, err := f() }\nfunc f() (int, error) { return 1, nil }\n";
        // Act
        let sites = SemanticExtractor::extract_def_use_for_file(src, "go", "_", "test.go", None);
        // Assert: blank identifier may be captured or excluded; test documents behavior
        // If captured, it should be Write; if not captured, sites will be empty
        if !sites.is_empty() {
            let has_write = sites.iter().any(|s| matches!(s.kind, DefUseKind::Write));
            assert!(has_write, "blank identifier if captured should be Write");
        }
    }
}
