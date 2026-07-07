// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Element extraction helpers for the parser module.
//!
//! This module contains internal helper functions for extracting semantic elements
//! (functions, classes, imports, calls, references, def-use sites) from parsed ASTs.
//! These functions are called by `SemanticExtractor` methods in the parent parser module.

use crate::types::{
    CallInfo, ClassInfo, FunctionInfo, ImplTraitInfo, ImportInfo, ReferenceInfo, ReferenceType,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tree_sitter::{Node, StreamingIterator};

use crate::parser::{CompiledQueries, ParserError, QUERY_CURSOR, TimeoutConfig};

/// Recursively extract `ImportInfo` entries from a use-clause node, respecting all Rust
/// use-declaration forms (`scoped_identifier`, `scoped_use_list`, `use_list`,
/// `use_as_clause`, `use_wildcard`, bare `identifier`).
#[allow(clippy::too_many_lines)] // exhaustive match over all supported Rust use-clause forms; splitting harms readability
pub(crate) fn extract_imports_from_node(
    node: &Node,
    source: &str,
    prefix: &str,
    line: usize,
    imports: &mut Vec<ImportInfo>,
) {
    match node.kind() {
        // Simple identifier: `use foo;` or an item inside `{foo, bar}`
        "identifier" | "self" | "super" | "crate" => {
            let name = source[node.start_byte()..node.end_byte()].to_string();
            imports.push(ImportInfo {
                module: prefix.to_string(),
                items: vec![name],
                line,
            });
        }
        // Qualified path: `std::collections::HashMap`
        "scoped_identifier" => {
            let item = node
                .child_by_field_name("name")
                .map(|n| source[n.start_byte()..n.end_byte()].to_string())
                .unwrap_or_default();
            let module = node.child_by_field_name("path").map_or_else(
                || prefix.to_string(),
                |p| {
                    let path_text = source[p.start_byte()..p.end_byte()].to_string();
                    if prefix.is_empty() {
                        path_text
                    } else {
                        format!("{prefix}::{path_text}")
                    }
                },
            );
            if !item.is_empty() {
                imports.push(ImportInfo {
                    module,
                    items: vec![item],
                    line,
                });
            }
        }
        // Use list: `{foo, bar, baz}`
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_imports_from_node(&child, source, prefix, line, imports);
            }
        }
        // Scoped use list: `std::{io, fs}`
        "scoped_use_list" => {
            let path = node
                .child_by_field_name("path")
                .map(|n| source[n.start_byte()..n.end_byte()].to_string())
                .unwrap_or_default();
            let new_prefix = if prefix.is_empty() {
                path
            } else {
                format!("{prefix}::{path}")
            };
            if let Some(list) = node.child_by_field_name("list") {
                let mut cursor = list.walk();
                for child in list.named_children(&mut cursor) {
                    extract_imports_from_node(&child, source, &new_prefix, line, imports);
                }
            }
        }
        // Wildcard: `use std::*;`
        "use_wildcard" => {
            let stripped = if prefix.ends_with("::*") {
                prefix.strip_suffix("::*").unwrap_or(prefix)
            } else {
                prefix
            };
            let module = if stripped.is_empty() {
                "*".to_string()
            } else if stripped.ends_with("::") || stripped.ends_with(':') {
                format!("{stripped}*")
            } else {
                format!("{stripped}::*")
            };
            imports.push(ImportInfo {
                module,
                items: vec!["*".to_string()],
                line,
            });
        }
        // `io as stdio` or `std::io as stdio`
        "use_as_clause" => {
            let alias = node
                .child_by_field_name("alias")
                .map(|n| source[n.start_byte()..n.end_byte()].to_string())
                .unwrap_or_default();
            let module = if let Some(path_node) = node.child_by_field_name("path") {
                match path_node.kind() {
                    "scoped_identifier" => path_node.child_by_field_name("path").map_or_else(
                        || prefix.to_string(),
                        |p| {
                            let p_text = source[p.start_byte()..p.end_byte()].to_string();
                            if prefix.is_empty() {
                                p_text
                            } else {
                                format!("{prefix}::{p_text}")
                            }
                        },
                    ),
                    _ => prefix.to_string(),
                }
            } else {
                prefix.to_string()
            };
            if !alias.is_empty() {
                imports.push(ImportInfo {
                    module,
                    items: vec![alias],
                    line,
                });
            }
        }
        // Python import_from_statement: `from module import name` or `from . import *`
        "import_from_statement" => {
            extract_python_import_from(node, source, line, imports);
        }
        // Fallback for non-Rust import nodes: capture full text as module
        _ => {
            let text = source[node.start_byte()..node.end_byte()]
                .trim()
                .to_string();
            if !text.is_empty() {
                imports.push(ImportInfo {
                    module: text,
                    items: vec![],
                    line,
                });
            }
        }
    }
}

/// Extract an item name from a `dotted_name` or `aliased_import` child node.
pub(crate) fn extract_import_item_name(child: &Node, source: &str) -> Option<String> {
    match child.kind() {
        "dotted_name" => {
            let name = source[child.start_byte()..child.end_byte()]
                .trim()
                .to_string();
            if name.is_empty() { None } else { Some(name) }
        }
        "aliased_import" => child.child_by_field_name("name").and_then(|n| {
            let name = source[n.start_byte()..n.end_byte()].trim().to_string();
            if name.is_empty() { None } else { Some(name) }
        }),
        _ => None,
    }
}

/// Collect wildcard/named imports from an `import_list` node or from direct named children.
pub(crate) fn collect_import_items(
    node: &Node,
    source: &str,
    is_wildcard: &mut bool,
    items: &mut Vec<String>,
) {
    // Prefer import_list child (wraps `from x import a, b`)
    if let Some(import_list) = node.child_by_field_name("import_list") {
        let mut cursor = import_list.walk();
        for child in import_list.named_children(&mut cursor) {
            if child.kind() == "wildcard_import" {
                *is_wildcard = true;
            } else if let Some(name) = extract_import_item_name(&child, source) {
                items.push(name);
            }
        }
        return;
    }
    // No import_list: single-name or wildcard as direct child (skip first named child = module_name)
    let mut cursor = node.walk();
    let mut first = true;
    for child in node.named_children(&mut cursor) {
        if first {
            first = false;
            continue;
        }
        if child.kind() == "wildcard_import" {
            *is_wildcard = true;
        } else if let Some(name) = extract_import_item_name(&child, source) {
            items.push(name);
        }
    }
}

/// Handle Python `import_from_statement` node.
pub(crate) fn extract_python_import_from(
    node: &Node,
    source: &str,
    line: usize,
    imports: &mut Vec<ImportInfo>,
) {
    let module = if let Some(m) = node.child_by_field_name("module_name") {
        source[m.start_byte()..m.end_byte()].trim().to_string()
    } else if let Some(r) = node.child_by_field_name("relative_import") {
        source[r.start_byte()..r.end_byte()].trim().to_string()
    } else {
        String::new()
    };

    let mut is_wildcard = false;
    let mut items = Vec::new();
    collect_import_items(node, source, &mut is_wildcard, &mut items);

    if !module.is_empty() {
        imports.push(ImportInfo {
            module,
            items: if is_wildcard {
                vec!["*".to_string()]
            } else {
                items
            },
            line,
        });
    }
}

/// Extract function and class definitions from the parsed AST.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract_elements(
    source: &str,
    compiled: &CompiledQueries,
    root: Node<'_>,
    max_depth: Option<u32>,
    functions: &mut Vec<FunctionInfo>,
    classes: &mut Vec<ClassInfo>,
    tc: TimeoutConfig,
    lang_info: &crate::languages::LanguageInfo,
) -> Result<(), ParserError> {
    let mut seen_functions = HashSet::new();
    let mut timed_out = false;

    QUERY_CURSOR.with(|c| {
        let mut cursor = c.borrow_mut();
        cursor.set_max_start_depth(None);
        if let Some(depth) = max_depth {
            cursor.set_max_start_depth(Some(depth));
        }

        let mut matches = cursor.matches(&compiled.element, root, source.as_bytes());

        while let Some(mat) = matches.next() {
            // Check if we've hit the deadline
            if tc.is_exceeded() {
                timed_out = true;
                break;
            }

            let mut func_node: Option<Node> = None;
            let mut class_node: Option<Node> = None;
            let mut func_name_text: Option<String> = None;
            let mut class_name_text: Option<String> = None;

            for capture in mat.captures {
                let capture_name = compiled.element.capture_names()[capture.index as usize];
                let node = capture.node;
                match capture_name {
                    "function" => {
                        func_node = Some(node);
                        func_name_text = node
                            .child_by_field_name("name")
                            .map(|n| source[n.start_byte()..n.end_byte()].to_string());
                    }
                    "class" => {
                        class_node = Some(node);
                        class_name_text = node
                            .child_by_field_name("name")
                            .map(|n| source[n.start_byte()..n.end_byte()].to_string());
                    }
                    _ => {}
                }
            }

            if let Some(func_node) = func_node {
                let func_def = if func_node.kind() == "decorated_definition" {
                    func_node
                        .child_by_field_name("definition")
                        .unwrap_or(func_node)
                } else {
                    func_node
                };

                let name = func_name_text
                    .or_else(|| {
                        func_def
                            .child_by_field_name("name")
                            .map(|n| source[n.start_byte()..n.end_byte()].to_string())
                    })
                    .unwrap_or_default();

                let func_key = (name.clone(), func_node.start_position().row);
                if !name.is_empty() && seen_functions.insert(func_key) {
                    // For C/C++: parameters live under declarator -> parameters.
                    // For other languages: parameters is a direct child field.
                    let params = func_def
                        .child_by_field_name("declarator")
                        .and_then(|d| d.child_by_field_name("parameters"))
                        .or_else(|| func_def.child_by_field_name("parameters"))
                        .map(|p| source[p.start_byte()..p.end_byte()].to_string())
                        .unwrap_or_default();

                    // Try "type" first (C/C++ uses this field for the return type);
                    // fall back to "return_type" (Rust, Python, TypeScript, etc.).
                    let return_type = func_def
                        .child_by_field_name("type")
                        .or_else(|| func_def.child_by_field_name("return_type"))
                        .map(|r| source[r.start_byte()..r.end_byte()].to_string());

                    // Walk backward through contiguous attribute_item siblings
                    // to find the first attribute line (Rust only).
                    let first_line = if func_node.kind() == "function_item" {
                        let mut attrs: Vec<Node> = Vec::new();
                        let mut sib = func_node.prev_named_sibling();
                        while let Some(s) = sib {
                            if s.kind() == "attribute_item" {
                                attrs.push(s);
                                sib = s.prev_named_sibling();
                            } else {
                                break;
                            }
                        }
                        attrs
                            .last()
                            .map(|n| n.start_position().row + 1)
                            .unwrap_or_else(|| func_node.start_position().row + 1)
                    } else {
                        func_node.start_position().row + 1
                    };

                    functions.push(FunctionInfo {
                        name,
                        line: first_line,
                        end_line: func_node.end_position().row + 1,
                        parameters: if params.is_empty() {
                            Vec::new()
                        } else {
                            vec![params]
                        },
                        return_type,
                    });
                }
            }

            if let Some(class_node) = class_node {
                let name = class_name_text
                    .or_else(|| {
                        class_node
                            .child_by_field_name("name")
                            .map(|n| source[n.start_byte()..n.end_byte()].to_string())
                    })
                    .unwrap_or_default();

                if !name.is_empty() {
                    let inherits = if let Some(handler) = lang_info.extract_inheritance {
                        handler(&class_node, source)
                    } else {
                        Vec::new()
                    };
                    classes.push(ClassInfo {
                        name,
                        line: class_node.start_position().row + 1,
                        end_line: class_node.end_position().row + 1,
                        methods: Vec::new(),
                        fields: Vec::new(),
                        inherits,
                    });
                }
            }
        }
    });

    if timed_out {
        return Err(ParserError::Timeout(tc.micros));
    }

    Ok(())
}

/// Returns the name of the enclosing function/method/subroutine for a given AST node,
/// by walking ancestors and matching all language-specific function container kinds.
pub(crate) fn enclosing_function_name(mut node: Node<'_>, source: &str) -> Option<String> {
    let mut depth = 0;
    loop {
        let parent = node.parent()?;
        depth += 1;
        if depth > 64 {
            return None;
        }
        let name_node = match parent.kind() {
            // Direct name field: Rust, Python, Go, Java, TypeScript/TSX
            "function_item"
            | "method_item"
            | "function_definition"
            | "function_declaration"
            | "method_declaration"
            | "method_definition" => parent.child_by_field_name("name"),
            // Fortran subroutine: name is inside subroutine_statement child
            "subroutine" => {
                let mut cursor = parent.walk();
                parent
                    .children(&mut cursor)
                    .find(|c| c.kind() == "subroutine_statement")
                    .and_then(|s| s.child_by_field_name("name"))
            }
            // Fortran function: name is inside function_statement child
            "function" => {
                let mut cursor = parent.walk();
                parent
                    .children(&mut cursor)
                    .find(|c| c.kind() == "function_statement")
                    .and_then(|s| s.child_by_field_name("name"))
            }
            _ => {
                node = parent;
                continue;
            }
        };
        return name_node.map(|n| source[n.start_byte()..n.end_byte()].to_string());
    }
}

/// Extract function call sites from the parsed AST.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract_calls(
    source: &str,
    compiled: &CompiledQueries,
    root: Node<'_>,
    max_depth: Option<u32>,
    calls: &mut Vec<CallInfo>,
    call_frequency: &mut HashMap<String, usize>,
    tc: TimeoutConfig,
) -> Result<(), ParserError> {
    let mut timed_out = false;

    QUERY_CURSOR.with(|c| {
        let mut cursor = c.borrow_mut();
        cursor.set_max_start_depth(None);
        if let Some(depth) = max_depth {
            cursor.set_max_start_depth(Some(depth));
        }

        let mut matches = cursor.matches(&compiled.call, root, source.as_bytes());

        while let Some(mat) = matches.next() {
            // Check if we've hit the deadline
            if tc.is_exceeded() {
                timed_out = true;
                break;
            }
            for capture in mat.captures {
                let capture_name = compiled.call.capture_names()[capture.index as usize];
                if capture_name != "call" {
                    continue;
                }
                let node = capture.node;
                let call_name = source[node.start_byte()..node.end_byte()].to_string();
                *call_frequency.entry(call_name.clone()).or_insert(0) += 1;

                let caller = enclosing_function_name(node, source)
                    .unwrap_or_else(|| "<module>".to_string());

                let mut arg_count = None;
                let mut arg_node = node;
                let mut hop = 0u32;
                while let Some(parent) = arg_node.parent() {
                    hop += 1;
                    // Bounded parent traversal: cap at 16 hops to guard against pathological
                    // walks on malformed/degenerate trees. Real call-expression nesting is
                    // shallow (typically 1-3 levels). When the cap is hit we stop searching and
                    // leave arg_count as None; the caller is still recorded, just without
                    // argument-count information.
                    if hop > 16 {
                        tracing::debug!(hop, callee = %call_name, "extract_calls: parent traversal cap reached; arg_count will be None");
                        break;
                    }
                    if parent.kind() == "call_expression" {
                        if let Some(args) = parent.child_by_field_name("arguments") {
                            arg_count = Some(args.named_child_count());
                        }
                        break;
                    }
                    arg_node = parent;
                }
                calls.push(CallInfo {
                    caller,
                    callee: call_name,
                    line: node.start_position().row + 1,
                    column: node.start_position().column,
                    arg_count,
                });
            }
        }
    });

    if timed_out {
        return Err(ParserError::Timeout(tc.micros));
    }

    Ok(())
}

/// Extract import statements from the parsed AST.
pub(crate) fn extract_imports(
    source: &str,
    compiled: &CompiledQueries,
    root: Node<'_>,
    max_depth: Option<u32>,
    imports: &mut Vec<ImportInfo>,
    tc: TimeoutConfig,
) -> Result<(), ParserError> {
    let Some(import_query) = &compiled.import else {
        return Ok(());
    };
    let mut timed_out = false;

    QUERY_CURSOR.with(|c| {
        let mut cursor = c.borrow_mut();
        cursor.set_max_start_depth(None);
        if let Some(depth) = max_depth {
            cursor.set_max_start_depth(Some(depth));
        }

        let mut matches = cursor.matches(import_query, root, source.as_bytes());

        while let Some(mat) = matches.next() {
            // Check if we've hit the deadline
            if tc.is_exceeded() {
                timed_out = true;
                break;
            }

            for capture in mat.captures {
                let capture_name = import_query.capture_names()[capture.index as usize];
                let node = capture.node;
                let line = node.start_position().row + 1;

                if capture_name == "import_path" {
                    extract_imports_from_node(&node, source, "", line, imports);
                }
            }
        }
    });

    if timed_out {
        return Err(ParserError::Timeout(tc.micros));
    }

    Ok(())
}

/// Extract impl block methods from the parsed AST.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract_impl_methods(
    source: &str,
    compiled: &CompiledQueries,
    root: Node<'_>,
    max_depth: Option<u32>,
    classes: &mut [ClassInfo],
    tc: TimeoutConfig,
) -> Result<(), ParserError> {
    let Some(impl_query) = &compiled.impl_block else {
        return Ok(());
    };
    let mut timed_out = false;

    QUERY_CURSOR.with(|c| {
        let mut cursor = c.borrow_mut();
        cursor.set_max_start_depth(None);
        if let Some(depth) = max_depth {
            cursor.set_max_start_depth(Some(depth));
        }

        let mut matches = cursor.matches(impl_query, root, source.as_bytes());

        while let Some(mat) = matches.next() {
            // Check if we've hit the deadline
            if tc.is_exceeded() {
                timed_out = true;
                break;
            }

            let mut impl_type_name = String::new();
            let mut method_name = String::new();
            let mut method_line = 0usize;
            let mut method_end_line = 0usize;
            let mut method_params = String::new();
            let mut method_return_type: Option<String> = None;

            for capture in mat.captures {
                let capture_name = impl_query.capture_names()[capture.index as usize];
                let node = capture.node;
                match capture_name {
                    "impl_type" => {
                        impl_type_name = source[node.start_byte()..node.end_byte()].to_string();
                    }
                    "method_name" => {
                        method_name = source[node.start_byte()..node.end_byte()].to_string();
                    }
                    "method_params" => {
                        method_params = source[node.start_byte()..node.end_byte()].to_string();
                    }
                    "method" => {
                        let mut method_attrs: Vec<Node> = Vec::new();
                        let mut msib = node.prev_named_sibling();
                        while let Some(s) = msib {
                            if s.kind() == "attribute_item" {
                                method_attrs.push(s);
                                msib = s.prev_named_sibling();
                            } else {
                                break;
                            }
                        }
                        method_line = method_attrs
                            .last()
                            .map(|n| n.start_position().row + 1)
                            .unwrap_or_else(|| node.start_position().row + 1);
                        method_end_line = node.end_position().row + 1;
                        method_return_type = node
                            .child_by_field_name("return_type")
                            .map(|r| source[r.start_byte()..r.end_byte()].to_string());
                    }
                    _ => {}
                }
            }

            if !impl_type_name.is_empty() && !method_name.is_empty() {
                let func = FunctionInfo {
                    name: method_name,
                    line: method_line,
                    end_line: method_end_line,
                    parameters: if method_params.is_empty() {
                        Vec::new()
                    } else {
                        vec![method_params]
                    },
                    return_type: method_return_type,
                };
                if let Some(class) = classes.iter_mut().find(|c| c.name == impl_type_name) {
                    class.methods.push(func);
                }
            }
        }
    });

    if timed_out {
        return Err(ParserError::Timeout(tc.micros));
    }

    Ok(())
}

/// Extract type references from the parsed AST.
pub(crate) fn extract_references(
    source: &str,
    compiled: &CompiledQueries,
    root: Node<'_>,
    max_depth: Option<u32>,
    references: &mut Vec<ReferenceInfo>,
    tc: TimeoutConfig,
) -> Result<(), ParserError> {
    let Some(ref ref_query) = compiled.reference else {
        return Ok(());
    };
    let mut seen_refs = HashSet::new();
    let mut timed_out = false;

    QUERY_CURSOR.with(|c| {
        let mut cursor = c.borrow_mut();
        cursor.set_max_start_depth(None);
        if let Some(depth) = max_depth {
            cursor.set_max_start_depth(Some(depth));
        }

        let mut matches = cursor.matches(ref_query, root, source.as_bytes());

        while let Some(mat) = matches.next() {
            // Check if we've hit the deadline
            if tc.is_exceeded() {
                timed_out = true;
                break;
            }

            for capture in mat.captures {
                let capture_name = ref_query.capture_names()[capture.index as usize];
                if capture_name == "type_ref" {
                    let node = capture.node;
                    let type_ref = source[node.start_byte()..node.end_byte()].to_string();
                    if seen_refs.insert(type_ref.clone()) {
                        references.push(ReferenceInfo {
                            symbol: type_ref,
                            reference_type: ReferenceType::Usage,
                            // location is intentionally empty here; set by the caller (analyze_file)
                            location: String::new(),
                            line: node.start_position().row + 1,
                        });
                    }
                }
            }
        }
    });

    if timed_out {
        return Err(ParserError::Timeout(tc.micros));
    }

    Ok(())
}

/// Extract impl-trait blocks from an already-parsed tree.
///
/// Called during `extract()` for Rust files to avoid a second parse.
/// Returns an empty vec if the query is not available.
pub(crate) fn extract_impl_traits_from_tree(
    source: &str,
    compiled: &CompiledQueries,
    root: Node<'_>,
    tc: TimeoutConfig,
) -> Result<Vec<ImplTraitInfo>, ParserError> {
    let Some(query) = &compiled.impl_trait else {
        return Ok(vec![]);
    };

    let mut results = Vec::new();
    let mut timed_out = false;

    QUERY_CURSOR.with(|c| {
        let mut cursor = c.borrow_mut();
        cursor.set_max_start_depth(None);

        let mut matches = cursor.matches(query, root, source.as_bytes());

        while let Some(mat) = matches.next() {
            // Check if we've hit the deadline
            if tc.is_exceeded() {
                timed_out = true;
                break;
            }

            let mut trait_name = String::new();
            let mut impl_type = String::new();
            let mut line = 0usize;

            for capture in mat.captures {
                let capture_name = query.capture_names()[capture.index as usize];
                let node = capture.node;
                let text = source[node.start_byte()..node.end_byte()].to_string();
                match capture_name {
                    "trait_name" => {
                        trait_name = text;
                        line = node.start_position().row + 1;
                    }
                    "impl_type" => {
                        impl_type = text;
                    }
                    _ => {}
                }
            }

            if !trait_name.is_empty() && !impl_type.is_empty() {
                results.push(ImplTraitInfo {
                    trait_name,
                    impl_type,
                    path: PathBuf::new(), // Path will be set by caller
                    line,
                });
            }
        }
    });

    if timed_out {
        return Err(ParserError::Timeout(tc.micros));
    }

    Ok(results)
}

/// Extract def-use sites for a symbol from the parsed AST.
pub(crate) fn extract_def_use(
    source: &str,
    compiled: &CompiledQueries,
    root: Node<'_>,
    symbol_name: &str,
    file_path: &str,
    max_depth: Option<u32>,
) -> Vec<crate::types::DefUseSite> {
    let Some(defuse_query) = &compiled.defuse else {
        return vec![];
    };

    let mut sites = Vec::new();
    let mut write_offsets = HashSet::new();
    let source_lines: Vec<&str> = source.lines().collect();

    QUERY_CURSOR.with(|c| {
        let mut cursor = c.borrow_mut();
        cursor.set_max_start_depth(None);
        if let Some(depth) = max_depth {
            cursor.set_max_start_depth(Some(depth));
        }
        let mut matches = cursor.matches(defuse_query, root, source.as_bytes());

        while let Some(mat) = matches.next() {
            for capture in mat.captures {
                let capture_name = defuse_query.capture_names()[capture.index as usize];
                let node = capture.node;
                let node_text = node.utf8_text(source.as_bytes()).unwrap_or_default();

                // Only collect if the captured node matches the target symbol
                if node_text != symbol_name {
                    continue;
                }

                // Classify capture by prefix
                let kind = if capture_name.starts_with("write.") {
                    crate::types::DefUseKind::Write
                } else if capture_name.starts_with("read.") {
                    crate::types::DefUseKind::Read
                } else if capture_name.starts_with("writeread.") {
                    crate::types::DefUseKind::WriteRead
                } else {
                    continue;
                };

                let byte_offset = node.start_byte();

                // De-duplicate: skip read captures for offsets already captured as write/writeread
                if kind == crate::types::DefUseKind::Read && write_offsets.contains(&byte_offset) {
                    continue;
                }
                if kind != crate::types::DefUseKind::Read {
                    write_offsets.insert(byte_offset);
                }

                // Get line number (1-indexed) and center-line snippet.
                // Always produce a 3-line window so snippet_one_line (index 1) is safe.
                let line = node.start_position().row + 1;
                let snippet = {
                    let row = node.start_position().row;
                    let last_line = source_lines.len().saturating_sub(1);
                    let prev = if row > 0 { row - 1 } else { 0 };
                    let next = std::cmp::min(row + 1, last_line);
                    let prev_text = if row == 0 {
                        ""
                    } else {
                        source_lines[prev].trim_end()
                    };
                    let cur_text = source_lines[row].trim_end();
                    let next_text = if row >= last_line {
                        ""
                    } else {
                        source_lines[next].trim_end()
                    };
                    format!("{prev_text}\n{cur_text}\n{next_text}")
                };

                // Get enclosing function scope
                let enclosing_scope = enclosing_function_name(node, source);

                let column = node.start_position().column;
                sites.push(crate::types::DefUseSite {
                    kind,
                    symbol: node_text.to_string(),
                    file: file_path.to_string(),
                    line,
                    column,
                    snippet,
                    enclosing_scope,
                });
            }
        }
    });

    sites
}
