// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use tree_sitter::Node;

/// Tree-sitter query for extracting C/C++ elements (functions, classes, and structures).
pub const ELEMENT_QUERY: &str = r"
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @func_name)) @function
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (identifier) @method_name))) @function
(class_specifier
  name: (type_identifier) @class_name) @class
(struct_specifier
  name: (type_identifier) @class_name) @class
(template_declaration
  (function_definition
    declarator: (function_declarator
      declarator: (identifier) @func_name))) @function
";

/// Tree-sitter query for extracting function calls.
pub const CALL_QUERY: &str = r"
(call_expression
  function: (identifier) @call)
(call_expression
  function: (field_expression field: (field_identifier) @call))
";

/// Tree-sitter query for extracting type references.
pub const REFERENCE_QUERY: &str = r"
(type_identifier) @type_ref
";

/// Tree-sitter query for extracting C/C++ preprocessor directives (#include).
pub const IMPORT_QUERY: &str = r"
(preproc_include
  path: (string_literal) @import_path)
(preproc_include
  path: (system_lib_string) @import_path)
";

/// Tree-sitter query for extracting definition and use sites.
pub const DEFUSE_QUERY: &str = r"
(init_declarator declarator: (identifier) @write.decl)
(assignment_expression left: (identifier) @write.assign)
(update_expression argument: (identifier) @writeread.update)
(identifier) @read.usage
";

/// Extract the function name from a C/C++ `function_definition` node by
/// walking the declarator chain: declarator -> function_declarator -> declarator -> identifier.
#[must_use]
pub fn extract_function_name(node: &Node, source: &str, _lang: &str) -> Option<String> {
    node.child_by_field_name("declarator")
        .and_then(|decl| extract_declarator_name(decl, source))
}

/// Find method name for a receiver type (class/struct context).
#[must_use]
pub fn find_method_for_receiver(
    node: &Node,
    source: &str,
    _depth: Option<usize>,
) -> Option<String> {
    if node.kind() != "function_definition" {
        return None;
    }

    // Walk up to find if we're in a class_specifier or struct_specifier
    let mut parent = node.parent();
    let mut in_class = false;
    while let Some(p) = parent {
        if p.kind() == "class_specifier" || p.kind() == "struct_specifier" {
            in_class = true;
            break;
        }
        parent = p.parent();
    }

    if !in_class {
        return None;
    }

    // Extract the method name from function_declarator
    if let Some(decl) = node.child_by_field_name("declarator") {
        extract_declarator_name(decl, source)
    } else {
        None
    }
}

/// Extract inheritance information from a class_specifier or struct_specifier node.
#[must_use]
pub fn extract_inheritance(node: &Node, source: &str) -> Vec<String> {
    let mut inherits = Vec::new();

    if node.kind() != "class_specifier" && node.kind() != "struct_specifier" {
        return inherits;
    }

    // Look for base_class_clause child
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(u32::try_from(i).unwrap_or(u32::MAX))
            && child.kind() == "base_class_clause"
        {
            // Walk base_class_clause for type_identifier nodes
            for j in 0..child.named_child_count() {
                if let Some(base) = child.named_child(u32::try_from(j).unwrap_or(u32::MAX))
                    && base.kind() == "type_identifier"
                {
                    let text = &source[base.start_byte()..base.end_byte()];
                    inherits.push(text.to_string());
                }
            }
        }
    }

    inherits
}

/// Helper: extract name from a declarator node (handles identifiers and qualified identifiers).
fn extract_declarator_name(node: Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => {
            let start = node.start_byte();
            let end = node.end_byte();
            if end <= source.len() {
                Some(source[start..end].to_string())
            } else {
                None
            }
        }
        "qualified_identifier" => node.child_by_field_name("name").and_then(|n| {
            let start = n.start_byte();
            let end = n.end_byte();
            if end <= source.len() {
                Some(source[start..end].to_string())
            } else {
                None
            }
        }),
        "function_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|n| extract_declarator_name(n, source)),
        "pointer_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|n| extract_declarator_name(n, source)),
        "reference_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|n| extract_declarator_name(n, source)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DefUseKind;
    use crate::parser::SemanticExtractor;
    use tree_sitter::Parser;

    fn parse_cpp(source: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("failed to set C++ language");
        parser.parse(source, None).expect("failed to parse source")
    }

    #[test]
    fn test_free_function() {
        // Arrange: free function definition
        let source = "int add(int a, int b) { return a + b; }";
        let tree = parse_cpp(source);
        let root = tree.root_node();
        let func_node = root.named_child(0).expect("expected function_definition");
        // Act
        let result = find_method_for_receiver(&func_node, source, None);
        // Assert: free function should not be a method
        assert_eq!(result, None);
    }

    #[test]
    fn test_class_with_method() {
        // Arrange: class with method
        let source = "class Foo { public: int getValue() { return 42; } };";
        let tree = parse_cpp(source);
        let root = tree.root_node();
        // Find the function_definition inside the class
        let func_node = find_node_by_kind(root, "function_definition").expect("expected function");
        // Act
        let result = find_method_for_receiver(&func_node, source, None);
        // Assert: method inside class should be recognized
        assert_eq!(result, Some("getValue".to_string()));
    }

    #[test]
    fn test_struct() {
        // Arrange: struct with no base class
        let source = "struct Point { int x; int y; };";
        let tree = parse_cpp(source);
        let root = tree.root_node();
        let struct_node =
            find_node_by_kind(root, "struct_specifier").expect("expected struct_specifier");
        // Assert: node kind is correct
        assert_eq!(struct_node.kind(), "struct_specifier");
        // Act + Assert: struct with no inheritance returns empty
        let result = extract_inheritance(&struct_node, source);
        assert!(
            result.is_empty(),
            "expected no inheritance, got: {result:?}"
        );
    }

    #[test]
    fn test_include_directive() {
        use tree_sitter::StreamingIterator;
        // Arrange
        let source = "#include <stdio.h>\n#include \"myfile.h\"\n";
        let tree = parse_cpp(source);
        // Act: run IMPORT_QUERY
        let lang: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        let query = tree_sitter::Query::new(&lang, super::IMPORT_QUERY)
            .expect("IMPORT_QUERY must be valid");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut iter = cursor.captures(&query, tree.root_node(), source.as_bytes());
        let mut captures: Vec<String> = Vec::new();
        while let Some((m, _)) = iter.next() {
            for c in m.captures {
                let text = c
                    .node
                    .utf8_text(source.as_bytes())
                    .unwrap_or("")
                    .to_string();
                captures.push(text);
            }
        }
        // Assert: both includes captured
        assert!(
            captures.iter().any(|s| s.contains("stdio.h")),
            "expected stdio.h in captures: {captures:?}"
        );
        assert!(
            captures.iter().any(|s| s.contains("myfile.h")),
            "expected myfile.h in captures: {captures:?}"
        );
    }

    #[test]
    fn test_template_function() {
        use tree_sitter::StreamingIterator;
        // Arrange: template function definition
        let source = "template<typename T> T max(T a, T b) { return a > b ? a : b; }";
        let tree = parse_cpp(source);
        // Act: run ELEMENT_QUERY
        let lang: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        let query = tree_sitter::Query::new(&lang, super::ELEMENT_QUERY)
            .expect("ELEMENT_QUERY must be valid");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut iter = cursor.captures(&query, tree.root_node(), source.as_bytes());
        let mut func_names: Vec<String> = Vec::new();
        while let Some((m, _)) = iter.next() {
            for c in m.captures {
                let name = query.capture_names()[c.index as usize];
                if name == "func_name" {
                    if let Ok(text) = c.node.utf8_text(source.as_bytes()) {
                        func_names.push(text.to_string());
                    }
                }
            }
        }
        // Assert: "max" captured as func_name
        assert!(
            func_names.iter().any(|s| s == "max"),
            "expected 'max' in func_names: {func_names:?}"
        );
    }

    #[test]
    fn test_class_with_inheritance() {
        // Arrange: class with base class
        let source = "class Derived : public Base { };";
        let tree = parse_cpp(source);
        let root = tree.root_node();
        let class_node = find_node_by_kind(root, "class_specifier").expect("expected class");
        // Act
        let result = extract_inheritance(&class_node, source);
        // Assert: should have "Base" as inheritance
        assert!(!result.is_empty(), "expected inheritance information");
        assert!(
            result.iter().any(|s| s.contains("Base")),
            "expected 'Base' in inheritance: {:?}",
            result
        );
    }

    /// Helper to find the first node of a given kind
    fn find_node_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
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
    fn test_defuse_query_write_site() {
        // Arrange
        let src = "void f() { int a = 7; }\n";
        let sites = SemanticExtractor::extract_def_use_for_file(src, "cpp", "a", "test.cpp", None);
        assert!(!sites.is_empty(), "defuse sites should not be empty");
        let has_write = sites.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(has_write, "should contain a Write DefUseSite");
    }
}
