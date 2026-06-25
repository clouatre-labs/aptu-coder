// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
/// Tree-sitter query for extracting Fortran elements (modules, functions, subroutines).
///
/// Module name is captured via child iteration because `module_statement`
/// does not expose a named field for the identifier.
/// CONTAINS sections are captured via `internal_procedures`.
pub const ELEMENT_QUERY: &str = r"
(subroutine
  (subroutine_statement) @function)

(function
  (function_statement) @function)

(module
  (module_statement
    (name) @class) @module_wrapper)

(module
  (internal_procedures
    (subroutine
      (subroutine_statement) @function)))

(module
  (internal_procedures
    (function
      (function_statement) @function)))
";

/// Tree-sitter query for extracting Fortran function calls.
/// Includes direct calls and derived type member calls (`obj%method`).
pub const CALL_QUERY: &str = r"
(subroutine_call
  (identifier) @call)

(call_expression
  (identifier) @call)

(derived_type_member_expression
  (type_member) @call)
";

/// Tree-sitter query for extracting Fortran type references.
pub const REFERENCE_QUERY: &str = r"
(name) @type_ref
";

/// Tree-sitter query for extracting Fortran imports (USE statements).
pub const IMPORT_QUERY: &str = r"
(use_statement
  (module_name) @import_path)
";

use tree_sitter::Node;

use crate::languages::get_node_text;

/// Extract inheritance information from a Fortran node.
/// Fortran does not have classical inheritance; return empty.
#[must_use]
pub fn extract_inheritance(_node: &Node, _source: &str) -> Vec<String> {
    Vec::new()
}

/// Extract the name of a function or subroutine node.
/// Both `subroutine_statement` and `function_statement` expose a named field
/// called `name`. Return the identifier text if present.
#[must_use]
pub fn extract_function_name(node: &Node, source: &str, _lang: &str) -> Option<String> {
    match node.kind() {
        "subroutine_statement" | "function_statement" => node
            .child_by_field_name("name")
            .and_then(|n| get_node_text(&n, source)),
        _ => None,
    }
}

/// Extract the name identifier from a `module` node.
///
/// `module_statement` does not expose a named field for the identifier; the
/// `name` node is an unnamed child of `module_statement`. This helper
/// centralises the two-level child walk so callers stay readable.
fn extract_module_name<'a>(module_node: &tree_sitter::Node<'a>, source: &str) -> Option<String> {
    let mut cursor = module_node.walk();
    for child in module_node.children(&mut cursor) {
        if child.kind() == "module_statement" {
            let mut stmt_cursor = child.walk();
            for name_child in child.children(&mut stmt_cursor) {
                if name_child.kind() == "name" {
                    return get_node_text(&name_child, source);
                }
            }
        }
    }
    None
}

/// Find the enclosing module name for a given subroutine/function node.
/// Walk up the parent chain until a `module` node is found and return its name.
#[must_use]
pub fn find_receiver_type(node: &Node, source: &str) -> Option<String> {
    if !matches!(node.kind(), "subroutine_statement" | "function_statement") {
        return None;
    }
    let mut current = *node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "module" {
            return extract_module_name(&parent, source);
        }
        current = parent;
    }
    None
}

/// Find the method name for a subroutine/function defined inside a module.
/// Returns the function/subroutine identifier if the node is enclosed by a module.
#[must_use]
pub fn find_method_for_receiver(
    node: &Node,
    source: &str,
    _depth: Option<usize>,
) -> Option<String> {
    if !matches!(node.kind(), "subroutine_statement" | "function_statement") {
        return None;
    }
    // Walk up to see if we are inside a module.
    let mut current = *node;
    let mut in_module = false;
    while let Some(parent) = current.parent() {
        if parent.kind() == "module" {
            in_module = true;
            break;
        }
        current = parent;
    }
    if !in_module {
        return None;
    }
    node.child_by_field_name("name")
        .and_then(|n| get_node_text(&n, source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;
    use tree_sitter::StreamingIterator;

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

    fn parse_fortran(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_fortran::LANGUAGE.into())
            .expect("failed to set Fortran language");
        let source_bytes = source.as_bytes().to_vec();
        let tree = parser.parse(&source_bytes, None).expect("failed to parse");
        (tree, source_bytes)
    }

    fn run_query(tree: &tree_sitter::Tree, source: &str, query_str: &str) -> Vec<(String, String)> {
        let query = tree_sitter::Query::new(&tree_sitter_fortran::LANGUAGE.into(), query_str)
            .expect("invalid query");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut captures = Vec::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches.next() {
            for c in m.captures {
                let node = c.node;
                let name = query.capture_names()[c.index as usize].to_string();
                let text = node
                    .utf8_text(source.as_bytes())
                    .unwrap_or_default()
                    .to_string();
                captures.push((name, text));
            }
        }
        captures
    }

    #[test]
    fn test_element_query_captures_module() {
        let source = "MODULE foo\nEND MODULE foo\n";
        let (tree, _) = parse_fortran(source);
        let caps = run_query(&tree, source, ELEMENT_QUERY);
        assert!(caps.iter().any(|(c, t)| c == "class" && t == "foo"));
    }

    #[test]
    fn test_element_query_empty_module() {
        let source = "MODULE foo\nEND MODULE foo\n";
        let (tree, _) = parse_fortran(source);
        let caps = run_query(&tree, source, ELEMENT_QUERY);
        // No @function captures
        assert!(!caps.iter().any(|(c, _)| c == "function"));
    }

    #[test]
    fn test_element_query_captures_subroutine() {
        let source = "SUBROUTINE bar(x)\n  x = x + 1\nEND SUBROUTINE bar\n";
        let (tree, _) = parse_fortran(source);
        let caps = run_query(&tree, source, ELEMENT_QUERY);
        assert!(
            caps.iter()
                .any(|(c, t)| c == "function" && t.contains("bar"))
        );
    }

    #[test]
    fn test_element_query_captures_function() {
        let source = "FUNCTION baz(x) RESULT(r)\n  r = x * 2\nEND FUNCTION baz\n";
        let (tree, _) = parse_fortran(source);
        let caps = run_query(&tree, source, ELEMENT_QUERY);
        assert!(
            caps.iter()
                .any(|(c, t)| c == "function" && t.contains("baz"))
        );
    }

    #[test]
    fn test_element_query_module_contains_subroutine() {
        let source =
            "MODULE mod1\nCONTAINS\nSUBROUTINE sub1()\nEND SUBROUTINE sub1\nEND MODULE mod1\n";
        let (tree, _) = parse_fortran(source);
        let caps = run_query(&tree, source, ELEMENT_QUERY);
        assert!(caps.iter().any(|(c, t)| c == "class" && t == "mod1"));
        assert!(
            caps.iter()
                .any(|(c, t)| c == "function" && t.contains("sub1"))
        );
    }

    #[test]
    fn test_import_query_captures_use_statement() {
        let source = "PROGRAM prog\nUSE iso_fortran_env\nEND PROGRAM prog\n";
        let (tree, _) = parse_fortran(source);
        let caps = run_query(&tree, source, IMPORT_QUERY);
        assert!(
            caps.iter()
                .any(|(c, t)| c == "import_path" && t == "iso_fortran_env")
        );
    }

    #[test]
    fn test_call_query_direct_call() {
        let source = "CALL compute(x, y)\n";
        let (tree, _) = parse_fortran(source);
        let caps = run_query(&tree, source, CALL_QUERY);
        assert!(caps.iter().any(|(c, t)| c == "call" && t == "compute"));
    }

    #[test]
    fn test_call_query_derived_type_member() {
        let source = "CALL obj%solve(rhs)\n";
        let (tree, _) = parse_fortran(source);
        let caps = run_query(&tree, source, CALL_QUERY);
        assert!(caps.iter().any(|(c, t)| c == "call" && t == "solve"));
    }

    #[test]
    fn test_extract_function_name_subroutine() {
        let source = "SUBROUTINE foo(a)\nEND SUBROUTINE foo\n";
        let (tree, _) = parse_fortran(source);
        let node = find_node(tree.root_node(), "subroutine_statement")
            .expect("subroutine_statement not found");
        let name = extract_function_name(&node, source, "fortran").expect("name");
        assert_eq!(name, "foo");
    }

    #[test]
    fn test_extract_function_name_function() {
        let source = "FUNCTION bar(x) RESULT(r)\nEND FUNCTION bar\n";
        let (tree, _) = parse_fortran(source);
        let node = find_node(tree.root_node(), "function_statement")
            .expect("function_statement not found");
        let name = extract_function_name(&node, source, "fortran").expect("name");
        assert_eq!(name, "bar");
    }

    #[test]
    fn test_extract_function_name_wrong_node() {
        let source = "MODULE foo\nEND MODULE foo\n";
        let (tree, _) = parse_fortran(source);
        let node = tree.root_node();
        let name = extract_function_name(&node, source, "fortran");
        assert!(name.is_none());
    }

    #[test]
    fn test_find_receiver_type_module_scoped() {
        let source =
            "MODULE mod1\nCONTAINS\nSUBROUTINE sub1()\nEND SUBROUTINE sub1\nEND MODULE mod1\n";
        let (tree, _) = parse_fortran(source);
        let node = find_node(tree.root_node(), "subroutine_statement")
            .expect("subroutine_statement not found");
        let mod_name = find_receiver_type(&node, source).expect("module name");
        assert_eq!(mod_name, "mod1");
    }

    #[test]
    fn test_find_receiver_type_top_level() {
        let source = "SUBROUTINE top()\nEND SUBROUTINE top\n";
        let (tree, _) = parse_fortran(source);
        let node = find_node(tree.root_node(), "subroutine_statement")
            .expect("subroutine_statement not found");
        let mod_name = find_receiver_type(&node, source);
        assert!(mod_name.is_none());
    }

    #[test]
    fn test_find_method_for_receiver_in_module() {
        let source =
            "MODULE mod1\nCONTAINS\nSUBROUTINE sub1()\nEND SUBROUTINE sub1\nEND MODULE mod1\n";
        let (tree, _) = parse_fortran(source);
        let node = find_node(tree.root_node(), "subroutine_statement")
            .expect("subroutine_statement not found");
        let method_name = find_method_for_receiver(&node, source, None).expect("method name");
        assert_eq!(method_name, "sub1");
    }

    #[test]
    fn test_find_method_for_receiver_top_level() {
        let source = "SUBROUTINE top()\nEND SUBROUTINE top\n";
        let (tree, _) = parse_fortran(source);
        let node = find_node(tree.root_node(), "subroutine_statement")
            .expect("subroutine_statement not found");
        let method_name = find_method_for_receiver(&node, source, None);
        assert!(method_name.is_none());
    }

    #[test]
    fn test_extract_inheritance_returns_empty() {
        // Arrange
        let source = "PROGRAM test\nEND PROGRAM test\n";
        let (tree, _source_bytes) = parse_fortran(source);
        let root = tree.root_node();

        // Act
        let result = extract_inheritance(&root, source);

        // Assert
        assert!(result.is_empty());
    }
}
