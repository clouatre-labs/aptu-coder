// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use tree_sitter::Node;

/// Tree-sitter query for extracting Rust elements (functions and structs/enums/traits).
pub const ELEMENT_QUERY: &str = r"
(function_item
  name: (identifier) @func_name
  parameters: (parameters) @params) @function
(struct_item) @class
(enum_item) @class
(trait_item) @class
";

/// Tree-sitter query for extracting function calls.
pub const CALL_QUERY: &str = r"
(call_expression function: (identifier) @call)
(call_expression function: (field_expression field: (field_identifier) @call))
(call_expression function: (scoped_identifier name: (identifier) @call))
";

/// Tree-sitter query for extracting type references.
pub const REFERENCE_QUERY: &str = r"
(type_identifier) @type_ref
";

/// Tree-sitter query for extracting imports.
pub const IMPORT_QUERY: &str = r"
(use_declaration argument: (_) @import_path) @import
";

/// Tree-sitter query for extracting `impl Trait for Type` blocks.
/// Captures the trait name and the concrete implementor type.
// Note: matches only simple trait names (type_identifier). Scoped traits
// (e.g. `impl io::Sink for T`) are not matched; scoped coverage is out of scope for v1.
pub const IMPL_TRAIT_QUERY: &str = r"
(impl_item
  trait: (type_identifier) @trait_name
  type: (type_identifier) @impl_type)
";

/// Tree-sitter query for extracting impl blocks and methods.
pub const IMPL_QUERY: &str = r"
(impl_item
  type: (type_identifier) @impl_type
  body: (declaration_list
    (function_item
      name: (identifier) @method_name
      parameters: (parameters) @method_params) @method))
";

/// Tree-sitter query for extracting definition and use sites.
/// Captures write sites (let declarations, assignment LHS), read sites (identifiers in expression context),
/// and write-read sites (compound assignments, +=, etc.).
pub const DEFUSE_QUERY: &str = r"
(let_declaration pattern: (identifier) @write.decl)
(assignment_expression left: (identifier) @write.assign)
(compound_assignment_expr left: (identifier) @writeread.compound)
(identifier) @read.usage
";

/// Extract function name from a function node.
#[must_use]
pub fn extract_function_name(node: &Node, source: &str, _query_name: &str) -> Option<String> {
    if node.kind() != "function_item" {
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

/// Find method name for a receiver type.
#[must_use]
pub fn find_method_for_receiver(
    node: &Node,
    source: &str,
    _depth: Option<usize>,
) -> Option<String> {
    if node.kind() != "method_item" && node.kind() != "function_item" {
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

/// Find receiver type for a method.
#[must_use]
pub fn find_receiver_type(node: &Node, source: &str) -> Option<String> {
    if node.kind() != "impl_item" {
        return None;
    }
    node.child_by_field_name("type").and_then(|n| {
        let start = n.start_byte();
        let end = n.end_byte();
        if end <= source.len() {
            Some(source[start..end].to_string())
        } else {
            None
        }
    })
}

/// Extract inheritance information from a Rust class node.
/// Rust class nodes (`struct_item`, `enum_item`, `trait_item`) have no syntactic inheritance.
/// Inheritance is via `impl` blocks, not on the type declaration itself.
#[must_use]
pub fn extract_inheritance(_node: &Node, _source: &str) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::SemanticExtractor;
    use crate::types::DefUseKind;
    use tree_sitter::Parser;

    fn parse_rust(source: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("failed to set Rust language");
        parser.parse(source, None).expect("failed to parse source")
    }

    #[test]
    fn test_extract_function_name_happy_path() {
        // Arrange
        let source = "fn foo() {}";
        let tree = parse_rust(source);
        let root = tree.root_node();
        // find the function_item node
        let func_node = root.named_child(0).expect("expected child");
        assert_eq!(func_node.kind(), "function_item");
        // Act
        let result = extract_function_name(&func_node, source, "function");
        // Assert
        assert_eq!(result, Some("foo".to_string()));
    }

    #[test]
    fn test_extract_function_name_wrong_kind() {
        // Arrange: parse a struct_item; not a function_item
        let source = "struct Bar {}";
        let tree = parse_rust(source);
        let root = tree.root_node();
        let node = root.named_child(0).expect("expected child");
        assert_eq!(node.kind(), "struct_item");
        // Act
        let result = extract_function_name(&node, source, "class");
        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_find_method_for_receiver_happy_path() {
        // Arrange: function_item works for find_method_for_receiver
        let source = "fn bar() {}";
        let tree = parse_rust(source);
        let root = tree.root_node();
        let node = root.named_child(0).expect("expected child");
        // Act
        let result = find_method_for_receiver(&node, source, None);
        // Assert
        assert_eq!(result, Some("bar".to_string()));
    }

    #[test]
    fn test_find_method_for_receiver_wrong_kind() {
        // Arrange: struct_item is not method_item or function_item
        let source = "struct Baz {}";
        let tree = parse_rust(source);
        let root = tree.root_node();
        let node = root.named_child(0).expect("expected child");
        // Act
        let result = find_method_for_receiver(&node, source, None);
        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_find_receiver_type_happy_path() {
        // Arrange: impl block is an impl_item
        let source = "struct Foo; impl Foo { fn x() {} }";
        let tree = parse_rust(source);
        let root = tree.root_node();
        // find impl_item node
        let impl_node = (0..root.named_child_count())
            .filter_map(|i| root.named_child(i as u32))
            .find(|n| n.kind() == "impl_item")
            .expect("expected impl_item node");
        // Act
        let result = find_receiver_type(&impl_node, source);
        // Assert
        assert_eq!(result, Some("Foo".to_string()));
    }

    #[test]
    fn test_find_receiver_type_wrong_kind() {
        // Arrange: function_item is not impl_item
        let source = "fn qux() {}";
        let tree = parse_rust(source);
        let root = tree.root_node();
        let node = root.named_child(0).expect("expected child");
        // Act
        let result = find_receiver_type(&node, source);
        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_defuse_query_write_site() {
        // Arrange
        let source = "fn foo() { let x = 5; }";
        let sites =
            SemanticExtractor::extract_def_use_for_file(source, "rust", "x", "test.rs", None);
        // Act & Assert
        assert!(!sites.is_empty(), "defuse sites should not be empty");
        let has_write = sites.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(has_write, "should contain a Write DefUseSite");
    }
}
