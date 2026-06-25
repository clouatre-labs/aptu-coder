// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
/// Tree-sitter query for extracting Python elements (functions and classes).
pub const ELEMENT_QUERY: &str = r"
[(decorated_definition
   definition: (function_definition
     name: (identifier) @func_name)) @function
 (function_definition
   name: (identifier) @func_name) @function]
(class_definition
  name: (identifier) @class_name) @class
";

/// Tree-sitter query for extracting function calls.
pub const CALL_QUERY: &str = r"
(call
  function: (identifier) @call)
(call
  function: (attribute attribute: (identifier) @call))
";

/// Tree-sitter query for extracting type references.
/// Python grammar has no `type_identifier` node; use `(type (identifier) @type_ref)`
/// to capture type names in annotations and `generic_type` for parameterized types.
pub const REFERENCE_QUERY: &str = r"
(type (identifier) @type_ref)
(generic_type (identifier) @type_ref)
";

/// Tree-sitter query for extracting Python imports.
pub const IMPORT_QUERY: &str = r"
(import_statement) @import_path
(import_from_statement) @import_path
";

/// Tree-sitter query for extracting definition and use sites.
pub const DEFUSE_QUERY: &str = r"
(assignment left: (identifier) @write.assign)
(assignment left: (pattern_list (identifier) @write.tuple))
(assignment left: (tuple_pattern (identifier) @write.tuple))
(assignment left: (list_pattern (identifier) @write.list))
(augmented_assignment left: (identifier) @writeread.augmented)
(named_expression name: (identifier) @write.named)
(identifier) @read.usage
";

use tree_sitter::Node;

/// Extract inheritance information from a Python class node.
#[must_use]
pub fn extract_inheritance(node: &Node, source: &str) -> Vec<String> {
    let mut inherits = Vec::new();

    // Get superclasses field from class_definition
    if let Some(superclasses) = node.child_by_field_name("superclasses") {
        // superclasses contains an argument_list
        for i in 0..superclasses.named_child_count() {
            if let Some(child) = superclasses.named_child(u32::try_from(i).unwrap_or(u32::MAX))
                && matches!(child.kind(), "identifier" | "attribute")
            {
                let text = &source[child.start_byte()..child.end_byte()];
                inherits.push(text.to_string());
            }
        }
    }

    inherits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DefUseKind;
    use crate::parser::SemanticExtractor;
    use tree_sitter::{Parser, StreamingIterator};

    fn parse_python(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("Error loading Python language");
        parser.parse(src, None).expect("Failed to parse Python")
    }

    #[test]
    fn test_python_element_query_happy_path() {
        // Arrange
        let src = "def greet(name): pass\nclass Greeter:\n    pass\n";
        let tree = parse_python(src);
        let root = tree.root_node();

        // Act
        let query = tree_sitter::Query::new(&tree_sitter_python::LANGUAGE.into(), ELEMENT_QUERY)
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
            captured_classes.contains(&"Greeter".to_string()),
            "expected Greeter class, got {:?}",
            captured_classes
        );
        assert!(
            captured_functions.contains(&"greet".to_string()),
            "expected greet function, got {:?}",
            captured_functions
        );
    }

    #[test]
    fn test_python_extract_inheritance() {
        // Arrange
        let src = "class Cat(Animal, Domestic): pass\n";
        let tree = parse_python(src);
        let root = tree.root_node();

        // Act -- find class_definition node
        let mut class_node: Option<tree_sitter::Node> = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "class_definition" {
                class_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }
        let class = class_node.expect("class_definition not found");
        let bases = extract_inheritance(&class, src);

        // Assert
        assert!(
            bases.contains(&"Animal".to_string()),
            "expected Animal, got {:?}",
            bases
        );
        assert!(
            bases.contains(&"Domestic".to_string()),
            "expected Domestic, got {:?}",
            bases
        );
    }

    #[test]
    fn test_defuse_query_write_site() {
        // Arrange
        let src = "x = 1\n";
        let sites =
            SemanticExtractor::extract_def_use_for_file(src, "python", "x", "test.py", None);
        assert!(!sites.is_empty(), "defuse sites should not be empty");
        let has_write = sites.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(has_write, "should contain a Write DefUseSite");
    }

    #[test]
    fn test_defuse_python_augmented_assignment() {
        // Arrange: augmented assignment += is WriteRead
        let src = "x = 1\nx += 2\n";
        // Act
        let sites =
            SemanticExtractor::extract_def_use_for_file(src, "python", "x", "test.py", None);
        // Assert
        assert!(
            !sites.is_empty(),
            "augmented assignment should produce defuse sites"
        );
        let has_writeread = sites
            .iter()
            .any(|s| matches!(s.kind, DefUseKind::WriteRead));
        assert!(has_writeread, "augmented assignment should be WriteRead");
    }

    #[test]
    fn test_defuse_python_tuple_unpack() {
        // Arrange: tuple unpack captures all LHS identifiers as Write
        let src = "a, b = (1, 2)\n";
        // Act
        let sites_a =
            SemanticExtractor::extract_def_use_for_file(src, "python", "a", "test.py", None);
        let sites_b =
            SemanticExtractor::extract_def_use_for_file(src, "python", "b", "test.py", None);
        // Assert
        assert!(
            !sites_a.is_empty(),
            "tuple unpack a should produce defuse sites"
        );
        assert!(
            !sites_b.is_empty(),
            "tuple unpack b should produce defuse sites"
        );
        let a_write = sites_a.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        let b_write = sites_b.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(a_write, "tuple unpack a should be Write");
        assert!(b_write, "tuple unpack b should be Write");
    }

    #[test]
    fn test_defuse_python_walrus() {
        // Arrange: walrus operator := is Write (named_expression)
        let src = "if (x := 42):\n    pass\n";
        // Act
        let sites =
            SemanticExtractor::extract_def_use_for_file(src, "python", "x", "test.py", None);
        // Assert
        assert!(
            !sites.is_empty(),
            "walrus operator should produce defuse sites"
        );
        let has_write = sites.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(has_write, "walrus operator should be Write");
    }
}
