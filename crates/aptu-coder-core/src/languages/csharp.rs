// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use tree_sitter::Node;

use super::get_node_text;

/// Extract function name from a C# method or constructor declaration.
#[must_use]
pub fn extract_function_name(node: &Node, source: &str, _lang: &str) -> Option<String> {
    if node.kind() != "method_declaration" && node.kind() != "constructor_declaration" {
        return None;
    }
    node.child_by_field_name("name")
        .and_then(|n| get_node_text(&n, source))
}

/// Find receiver type (enclosing class/struct/interface/record/enum) for a C# method.
#[must_use]
pub fn find_receiver_type(node: &Node, source: &str) -> Option<String> {
    if node.kind() != "method_declaration" && node.kind() != "constructor_declaration" {
        return None;
    }

    // Walk ancestors to find enclosing type declaration
    let mut current = *node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration"
            | "interface_declaration"
            | "record_declaration"
            | "struct_declaration"
            | "enum_declaration" => {
                // Found the enclosing type, extract its name
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

/// Tree-sitter query for extracting C# elements (methods, constructors, classes,
/// interfaces, records, structs, and enums).
pub const ELEMENT_QUERY: &str = r"
(method_declaration name: (identifier) @method_name) @function
(constructor_declaration name: (identifier) @ctor_name) @function
(class_declaration name: (identifier) @class_name) @class
(interface_declaration name: (identifier) @interface_name) @class
(record_declaration name: (identifier) @record_name) @class
(struct_declaration name: (identifier) @struct_name) @class
(enum_declaration name: (identifier) @enum_name) @class
";

/// Tree-sitter query for extracting C# method invocations.
pub const CALL_QUERY: &str = r"
(invocation_expression
  function: (member_access_expression name: (identifier) @call))
(invocation_expression
  function: (identifier) @call)
";

/// Tree-sitter query for extracting C# type references (base types, generic args).
pub const REFERENCE_QUERY: &str = r"
(base_list (identifier) @type_ref)
(base_list (generic_name (identifier) @type_ref))
(type_argument_list (identifier) @type_ref)
(type_parameter_list (type_parameter (identifier) @type_ref))
";

/// Tree-sitter query for extracting C# `using` directives.
///
/// All `using` forms (namespace, `using static`, and `using alias = ...`)
/// are represented by a single `using_directive` node kind. There are no
/// separate `using_static_directive` or `using_alias_directive` node kinds,
/// so one pattern captures everything.
pub const IMPORT_QUERY: &str = r"
(using_directive) @import_path
";

/// Tree-sitter query for extracting definition and use sites.
pub const DEFUSE_QUERY: &str = r"
(variable_declarator name: (identifier) @write.var)
(assignment_expression left: (identifier) @write.assign)
(identifier) @read.usage
";

/// Extract base class and interface names from a C# class, interface, or record node.
///
/// The parser calls this with the class/interface/record declaration node itself.
/// We locate the `base_list` child and extract each base type name.
#[must_use]
pub fn extract_inheritance(node: &Node, source: &str) -> Vec<String> {
    let mut bases = Vec::new();

    // base_list is an unnamed child of class_declaration/interface_declaration/record_declaration
    for i in 0..node.child_count() {
        if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX))
            && child.kind() == "base_list"
        {
            bases.extend(extract_base_list(&child, source));
            break;
        }
    }

    bases
}

/// Extract base type names from a `base_list` node.
fn extract_base_list(node: &Node, source: &str) -> Vec<String> {
    let mut bases = Vec::new();

    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(u32::try_from(i).unwrap_or(u32::MAX)) {
            match child.kind() {
                "identifier" => {
                    let end = child.end_byte();
                    if end <= source.len() {
                        bases.push(source[child.start_byte()..end].to_string());
                    }
                }
                "generic_name" => {
                    // First named child of generic_name is the identifier.
                    if let Some(id) = child.named_child(0)
                        && id.kind() == "identifier"
                    {
                        let end = id.end_byte();
                        if end <= source.len() {
                            bases.push(source[id.start_byte()..end].to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    bases
}

/// Return the method or constructor name when `node` is a `method_declaration`
/// or `constructor_declaration` that is nested inside a class, interface, or
/// record body.
///
/// This follows the same contract as the Rust, Go, and C++ handlers: return
/// the **method name** (the `name` field of the declaration node), or `None`
/// when the node is not a class-level method.
#[must_use]
pub fn find_method_for_receiver(
    node: &Node,
    source: &str,
    _depth: Option<usize>,
) -> Option<String> {
    if node.kind() != "method_declaration" && node.kind() != "constructor_declaration" {
        return None;
    }

    // Only return a name when the node is nested inside a type body.
    let mut current = *node;
    let mut in_type_body = false;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration"
            | "interface_declaration"
            | "record_declaration"
            | "struct_declaration"
            | "enum_declaration" => {
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
    use crate::DefUseKind;
    use crate::parser::SemanticExtractor;
    use tree_sitter::Parser;

    fn parse_csharp(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .expect("Error loading C# language");
        parser.parse(src, None).expect("Failed to parse C#")
    }

    #[test]
    fn test_extract_function_name() {
        // Arrange: method inside a class
        let src = "class C { void foo() {} }";
        let tree = parse_csharp(src);
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
        let result = extract_function_name(&method_node, src, "csharp");

        // Assert
        assert_eq!(result, Some("foo".to_string()));
    }

    #[test]
    fn test_find_receiver_type() {
        // Arrange: method inside a class
        let src = "class MyClass { void bar() {} }";
        let tree = parse_csharp(src);
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
    fn test_csharp_method_in_class() {
        // Arrange
        let src = "class Foo { void Bar() { Baz(); } void Baz() {} }";
        let tree = parse_csharp(src);
        let root = tree.root_node();

        // Act -- collect method names by reading the `name` field of each
        // `method_declaration` node directly (testing name field extraction).
        let mut methods: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "method_declaration" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    methods.push(src[name_node.start_byte()..name_node.end_byte()].to_string());
                }
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }
        methods.sort();

        // Assert
        assert_eq!(methods, vec!["Bar", "Baz"]);
    }

    #[test]
    fn test_csharp_constructor() {
        // Arrange
        let src = "class Foo { public Foo() {} }";
        let tree = parse_csharp(src);
        let root = tree.root_node();

        // Act
        let mut ctors: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "constructor_declaration" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    ctors.push(src[name_node.start_byte()..name_node.end_byte()].to_string());
                }
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }

        // Assert
        assert_eq!(ctors, vec!["Foo"]);
    }

    #[test]
    fn test_csharp_interface() {
        // Arrange
        let src = "interface IBar { void Do(); }";
        let tree = parse_csharp(src);
        let root = tree.root_node();

        // Act
        let mut interfaces: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "interface_declaration" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    interfaces.push(src[name_node.start_byte()..name_node.end_byte()].to_string());
                }
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }

        // Assert
        assert_eq!(interfaces, vec!["IBar"]);
    }

    #[test]
    fn test_csharp_using_directive() {
        // Arrange
        let src = "using System;";
        let tree = parse_csharp(src);
        let root = tree.root_node();

        // Act
        let mut imports: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "using_directive" {
                imports.push(src[node.start_byte()..node.end_byte()].to_string());
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }

        // Assert
        assert_eq!(imports, vec!["using System;"]);
    }

    #[test]
    fn test_csharp_async_method() {
        // Arrange -- async modifier is a sibling of the return type; name field unchanged
        let src = "class C { async Task Foo() { await Bar(); } Task Bar() { return Task.CompletedTask; } }";
        let tree = parse_csharp(src);
        let root = tree.root_node();

        // Act
        let mut methods: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "method_declaration" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    methods.push(src[name_node.start_byte()..name_node.end_byte()].to_string());
                }
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }

        // Assert -- Foo must be extracted even with async modifier
        assert!(methods.contains(&"Foo".to_string()));
    }

    #[test]
    fn test_csharp_generic_class() {
        // Arrange -- type_parameter_list is a child of class_declaration; class name unchanged
        let src = "class Generic<T> { T value; }";
        let tree = parse_csharp(src);
        let root = tree.root_node();

        // Act
        let mut classes: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "class_declaration" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    classes.push(src[name_node.start_byte()..name_node.end_byte()].to_string());
                }
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }

        // Assert -- generic name captured without type parameters, consistent with Go
        assert_eq!(classes, vec!["Generic"]);
    }

    #[test]
    fn test_csharp_inheritance_extraction() {
        // Arrange
        let src = "class Dog : Animal, ICanRun {}";
        let tree = parse_csharp(src);
        let root = tree.root_node();

        // Act -- find base_list node under class_declaration
        let mut base_list_node: Option<Node> = None;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "base_list" {
                base_list_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack.push(child);
                }
            }
        }

        // The parser passes the class_declaration node, not the base_list
        let mut class_node: Option<Node> = None;
        let mut stack2 = vec![root];
        while let Some(node) = stack2.pop() {
            if node.kind() == "class_declaration" {
                class_node = Some(node);
                break;
            }
            for i in 0..node.child_count() {
                if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                    stack2.push(child);
                }
            }
        }
        let class = class_node.expect("class_declaration not found");
        let _ = base_list_node; // retained for context clarity
        let bases = extract_inheritance(&class, src);

        // Assert
        assert_eq!(bases, vec!["Animal", "ICanRun"]);
    }

    #[test]
    fn test_csharp_find_method_for_receiver() {
        // Arrange
        let src = "class MyClass { void MyMethod() {} }";
        let tree = parse_csharp(src);
        let root = tree.root_node();

        // Act -- find method_declaration node and check it returns the method name
        let mut method_node: Option<Node> = None;
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

        let method = method_node.expect("method_declaration not found");
        let name = find_method_for_receiver(&method, src, None);

        // Assert -- returns the method name, not the enclosing type name
        assert_eq!(name, Some("MyMethod".to_string()));
    }

    #[test]
    fn test_defuse_query_write_site() {
        // Arrange
        let src = "class C { void M() { int b = 3; } }\n";
        let sites =
            SemanticExtractor::extract_def_use_for_file(src, "csharp", "b", "test.cs", None);
        assert!(!sites.is_empty(), "defuse sites should not be empty");
        let has_write = sites.iter().any(|s| matches!(s.kind, DefUseKind::Write));
        assert!(has_write, "should contain a Write DefUseSite");
    }
}
