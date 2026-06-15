// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Tree-sitter-based parser for extracting semantic structure from source code.
//!
//! This module provides language-agnostic parsing using tree-sitter queries to extract
//! functions, classes, imports, references, and other semantic elements from source files.
//! Two main extractors handle different use cases:
//!
//! - [`ElementExtractor`]: Quick extraction of function and class counts.
//! - [`SemanticExtractor`]: Detailed semantic analysis with calls, imports, and references.

use crate::languages::{get_language_info, try_regex_fallback};
use crate::types::{
    CallInfo, ClassInfo, FunctionInfo, ImplTraitInfo, ImportInfo, ReferenceInfo, ReferenceType,
    SemanticAnalysis,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use thiserror::Error;
use tracing::instrument;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ParserError {
    #[error("Unsupported language: {0}")]
    UnsupportedLanguage(String),
    #[error("Failed to parse file: {0}")]
    ParseError(String),
    #[error("Invalid UTF-8 in file")]
    InvalidUtf8,
    #[error("Query error: {0}")]
    QueryError(String),
    #[error("Parse timeout exceeded: {0} microseconds")]
    Timeout(u64),
}

/// Groups a query deadline with the configured timeout duration for use in private extract helpers.
/// Avoids threading two separate values through every helper signature.
#[derive(Clone, Copy)]
struct TimeoutConfig {
    /// Absolute deadline; `None` means no timeout.
    pub deadline: Option<std::time::Instant>,
    /// The configured timeout in microseconds (used in `ParserError::Timeout`).
    pub micros: u64,
}

impl TimeoutConfig {
    fn new(timeout_micros: Option<u64>) -> Self {
        let deadline = timeout_micros
            .map(|us| std::time::Instant::now() + std::time::Duration::from_micros(us));
        Self {
            deadline,
            micros: timeout_micros.unwrap_or(0),
        }
    }

    /// Returns `true` if the deadline has been reached.
    fn is_exceeded(self) -> bool {
        self.deadline
            .is_some_and(|d| std::time::Instant::now() >= d)
    }
}

/// Compiled tree-sitter queries for a language.
/// Stores all query types: mandatory (element, call) and optional (import, impl, reference).
struct CompiledQueries {
    pub element: Query,
    pub call: Query,
    pub import: Option<Query>,
    pub impl_block: Option<Query>,
    pub reference: Option<Query>,
    pub impl_trait: Option<Query>,
    pub defuse: Option<Query>,
}

/// Build compiled queries for a given language.
///
/// The `map_err` closures inside are only reachable if a hardcoded query string is
/// invalid, which cannot happen at runtime -- exclude them from coverage instrumentation.
#[cfg_attr(coverage_nightly, coverage(off))]
fn build_compiled_queries(
    lang_info: &crate::languages::LanguageInfo,
) -> Result<CompiledQueries, ParserError> {
    let element = Query::new(&lang_info.language, lang_info.element_query).map_err(|e| {
        ParserError::QueryError(format!(
            "Failed to compile element query for {}: {}",
            lang_info.name, e
        ))
    })?;

    let call = Query::new(&lang_info.language, lang_info.call_query).map_err(|e| {
        ParserError::QueryError(format!(
            "Failed to compile call query for {}: {}",
            lang_info.name, e
        ))
    })?;

    let import = if let Some(import_query_str) = lang_info.import_query {
        Some(
            Query::new(&lang_info.language, import_query_str).map_err(|e| {
                ParserError::QueryError(format!(
                    "Failed to compile import query for {}: {}",
                    lang_info.name, e
                ))
            })?,
        )
    } else {
        None
    };

    let impl_block = if let Some(impl_query_str) = lang_info.impl_query {
        Some(
            Query::new(&lang_info.language, impl_query_str).map_err(|e| {
                ParserError::QueryError(format!(
                    "Failed to compile impl query for {}: {}",
                    lang_info.name, e
                ))
            })?,
        )
    } else {
        None
    };

    let reference = if let Some(ref_query_str) = lang_info.reference_query {
        Some(Query::new(&lang_info.language, ref_query_str).map_err(|e| {
            ParserError::QueryError(format!(
                "Failed to compile reference query for {}: {}",
                lang_info.name, e
            ))
        })?)
    } else {
        None
    };

    let impl_trait = if let Some(impl_trait_query_str) = lang_info.impl_trait_query {
        Some(
            Query::new(&lang_info.language, impl_trait_query_str).map_err(|e| {
                ParserError::QueryError(format!(
                    "Failed to compile impl_trait query for {}: {}",
                    lang_info.name, e
                ))
            })?,
        )
    } else {
        None
    };

    let defuse = if let Some(defuse_query_str) = lang_info.defuse_query {
        Some(
            Query::new(&lang_info.language, defuse_query_str).map_err(|e| {
                ParserError::QueryError(format!(
                    "Failed to compile defuse query for {}: {}",
                    lang_info.name, e
                ))
            })?,
        )
    } else {
        None
    };

    Ok(CompiledQueries {
        element,
        call,
        import,
        impl_block,
        reference,
        impl_trait,
        defuse,
    })
}

/// Initialize the query cache with compiled queries for all supported languages.
///
/// Excluded from coverage: the `Err` arm is unreachable because `build_compiled_queries`
/// only fails on invalid hardcoded query strings.
#[cfg_attr(coverage_nightly, coverage(off))]
fn init_query_cache() -> HashMap<&'static str, CompiledQueries> {
    let mut cache = HashMap::new();

    for lang_name in crate::lang::supported_languages() {
        if let Some(lang_info) = get_language_info(lang_name) {
            match build_compiled_queries(&lang_info) {
                Ok(compiled) => {
                    cache.insert(*lang_name, compiled);
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to compile queries for language {}: {}",
                        lang_name,
                        e
                    );
                }
            }
        }
    }

    cache
}

/// Lazily initialized cache of compiled queries per language.
static QUERY_CACHE: LazyLock<HashMap<&'static str, CompiledQueries>> =
    LazyLock::new(init_query_cache);

/// Get compiled queries for a language from the cache.
fn get_compiled_queries(language: &str) -> Result<&'static CompiledQueries, ParserError> {
    QUERY_CACHE
        .get(language)
        .ok_or_else(|| ParserError::UnsupportedLanguage(language.to_string()))
}

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new(Parser::new());
    static QUERY_CURSOR: RefCell<QueryCursor> = RefCell::new(QueryCursor::new());
}

/// Canonical API for extracting element counts from source code.
pub struct ElementExtractor;

impl ElementExtractor {
    /// Extract function and class counts from source code.
    ///
    /// # Errors
    ///
    /// Returns `ParserError::UnsupportedLanguage` if the language is not recognized.
    /// Returns `ParserError::ParseError` if the source code cannot be parsed.
    /// Returns `ParserError::QueryError` if the tree-sitter query fails.
    #[instrument(skip_all, fields(language))]
    pub fn extract_with_depth(source: &str, language: &str) -> Result<(usize, usize), ParserError> {
        let lang_info = get_language_info(language)
            .ok_or_else(|| ParserError::UnsupportedLanguage(language.to_string()))?;

        let tree = PARSER.with(|p| {
            let mut parser = p.borrow_mut();
            parser
                .set_language(&lang_info.language)
                .map_err(|e| ParserError::ParseError(format!("Failed to set language: {e}")))?;
            parser
                .parse(source, None)
                .ok_or_else(|| ParserError::ParseError("Failed to parse".to_string()))
        })?;

        let compiled = get_compiled_queries(language)?;

        let (function_count, class_count) = QUERY_CURSOR.with(|c| {
            let mut cursor = c.borrow_mut();
            cursor.set_max_start_depth(None);
            let mut function_count = 0;
            let mut class_count = 0;

            let mut matches =
                cursor.matches(&compiled.element, tree.root_node(), source.as_bytes());
            while let Some(mat) = matches.next() {
                for capture in mat.captures {
                    let capture_name = compiled.element.capture_names()[capture.index as usize];
                    match capture_name {
                        "function" => function_count += 1,
                        "class" => class_count += 1,
                        _ => {}
                    }
                }
            }
            (function_count, class_count)
        });

        tracing::debug!(language = %language, functions = function_count, classes = class_count, "parse complete");

        Ok((function_count, class_count))
    }
}

/// Recursively extract `ImportInfo` entries from a use-clause node, respecting all Rust
/// use-declaration forms (`scoped_identifier`, `scoped_use_list`, `use_list`,
/// `use_as_clause`, `use_wildcard`, bare `identifier`).
#[allow(clippy::too_many_lines)] // exhaustive match over all supported Rust use-clause forms; splitting harms readability
fn extract_imports_from_node(
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
        // `std::{io, fs}` — path prefix followed by a brace list
        "scoped_use_list" => {
            let new_prefix = node.child_by_field_name("path").map_or_else(
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
            if let Some(list) = node.child_by_field_name("list") {
                extract_imports_from_node(&list, source, &new_prefix, line, imports);
            }
        }
        // `{HashMap, HashSet}` — brace-enclosed list of items
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "{" | "}" | "," => {}
                    _ => extract_imports_from_node(&child, source, prefix, line, imports),
                }
            }
        }
        // `std::io::*` — glob import
        "use_wildcard" => {
            let text = source[node.start_byte()..node.end_byte()].to_string();
            let module = if let Some(stripped) = text.strip_suffix("::*") {
                if prefix.is_empty() {
                    stripped.to_string()
                } else {
                    format!("{prefix}::{stripped}")
                }
            } else {
                prefix.to_string()
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
fn extract_import_item_name(child: &Node, source: &str) -> Option<String> {
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
fn collect_import_items(
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
fn extract_python_import_from(
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

pub struct SemanticExtractor;

impl SemanticExtractor {
    /// Extract semantic information from source code.
    ///
    /// # Errors
    ///
    /// Returns `ParserError::UnsupportedLanguage` if the language is not recognized.
    /// Returns `ParserError::ParseError` if the source code cannot be parsed.
    /// Returns `ParserError::QueryError` if the tree-sitter query fails.
    #[instrument(skip_all, fields(language))]
    pub fn extract(
        source: &str,
        language: &str,
        ast_recursion_limit: Option<usize>,
        timeout_micros: Option<u64>,
    ) -> Result<SemanticAnalysis, ParserError> {
        let tc = TimeoutConfig::new(timeout_micros);

        // Check deadline at the start before any parsing work.
        if tc.is_exceeded() {
            return Err(ParserError::Timeout(tc.micros));
        }

        // Try regex-based fallback for formats without a tree-sitter grammar.
        if let Some(analysis) = try_regex_fallback(source, language) {
            return Ok(analysis);
        }

        let lang_info = get_language_info(language)
            .ok_or_else(|| ParserError::UnsupportedLanguage(language.to_string()))?;

        let tree = PARSER.with(|p| {
            let mut parser = p.borrow_mut();
            parser
                .set_language(&lang_info.language)
                .map_err(|e| ParserError::ParseError(format!("Failed to set language: {e}")))?;
            parser
                .parse(source, None)
                .ok_or_else(|| ParserError::ParseError("Failed to parse".to_string()))
        })?;

        // 0 is not a useful depth (visits root node only, returning zero results).
        // Treat 0 as None (unlimited). See #339.
        let max_depth: Option<u32> = ast_recursion_limit
            .filter(|&limit| limit > 0)
            .map(|limit| {
                u32::try_from(limit).map_err(|_| {
                    ParserError::ParseError(format!(
                        "ast_recursion_limit {} exceeds maximum supported value {}",
                        limit,
                        u32::MAX
                    ))
                })
            })
            .transpose()?;

        let compiled = get_compiled_queries(language)?;
        let root = tree.root_node();

        let mut functions = Vec::new();
        let mut classes = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let mut call_frequency = HashMap::new();
        let mut calls = Vec::new();

        Self::extract_elements(
            source,
            compiled,
            root,
            max_depth,
            &lang_info,
            &mut functions,
            &mut classes,
            tc,
        )?;
        Self::extract_calls(
            source,
            compiled,
            root,
            max_depth,
            &mut calls,
            &mut call_frequency,
            tc,
        )?;
        Self::extract_imports(source, compiled, root, max_depth, &mut imports, tc)?;
        Self::extract_impl_methods(source, compiled, root, max_depth, &mut classes, tc)?;
        Self::extract_references(source, compiled, root, max_depth, &mut references, tc)?;

        // Extract impl-trait blocks for Rust files (empty for other languages)
        let impl_traits = if language == "rust" {
            Self::extract_impl_traits_from_tree(source, compiled, root, tc)?
        } else {
            vec![]
        };

        tracing::debug!(language = %language, functions = functions.len(), classes = classes.len(), imports = imports.len(), references = references.len(), calls = calls.len(), impl_traits = impl_traits.len(), "extraction complete");

        Ok(SemanticAnalysis {
            functions,
            classes,
            imports,
            references,
            call_frequency,
            calls,
            impl_traits,
            def_use_sites: Vec::new(),
        })
    }

    /// Fast path for extracting module metadata: functions and imports only.
    ///
    /// This method is optimized for the `analyze_module` tool, which only needs function
    /// definitions and import statements. It skips the more expensive extractors (calls,
    /// references, impl traits) and returns a lightweight `ModuleInfo` directly.
    ///
    /// # Arguments
    ///
    /// * `source` - The source code as a string
    /// * `language` - The programming language (e.g., "rust", "python")
    /// * `timeout` - Optional timeout configuration in microseconds
    ///
    /// # Returns
    ///
    /// A `ModuleInfo` containing the file name, line count, language, functions, and imports.
    ///
    /// # Errors
    ///
    /// Returns a `ParserError` if:
    /// * `ParserError::Timeout` - The operation exceeds the specified timeout
    /// * `ParserError::UnsupportedLanguage` - The language is not supported
    /// * `ParserError::ParseError` - Tree-sitter parsing fails
    #[instrument(skip_all, fields(language))]
    pub fn extract_module_info(
        source: &str,
        language: &str,
        timeout_micros: Option<u64>,
    ) -> Result<crate::types::ModuleInfo, ParserError> {
        let tc = TimeoutConfig::new(timeout_micros);

        // Check deadline at the start before any parsing work.
        if tc.is_exceeded() {
            return Err(ParserError::Timeout(tc.micros));
        }

        let lang_info = get_language_info(language)
            .ok_or_else(|| ParserError::UnsupportedLanguage(language.to_string()))?;

        let tree = PARSER.with(|p| {
            let mut parser = p.borrow_mut();
            parser
                .set_language(&lang_info.language)
                .map_err(|e| ParserError::ParseError(format!("Failed to set language: {e}")))?;
            parser
                .parse(source, None)
                .ok_or_else(|| ParserError::ParseError("Failed to parse".to_string()))
        })?;

        // Check deadline after parsing
        if tc.is_exceeded() {
            return Err(ParserError::Timeout(tc.micros));
        }

        let compiled = get_compiled_queries(language)?;
        let root = tree.root_node();

        let mut functions = Vec::new();
        let mut classes = Vec::new();
        let mut imports = Vec::new();

        // Extract functions and classes
        Self::extract_elements(
            source,
            compiled,
            root,
            None,
            &lang_info,
            &mut functions,
            &mut classes,
            tc,
        )?;

        // Check deadline after extract_elements
        if tc.is_exceeded() {
            return Err(ParserError::Timeout(tc.micros));
        }

        // Extract imports
        Self::extract_imports(source, compiled, root, None, &mut imports, tc)?;

        // Check deadline after extract_imports
        if tc.is_exceeded() {
            return Err(ParserError::Timeout(tc.micros));
        }

        // Map to ModuleInfo
        let module_functions = functions
            .into_iter()
            .map(|f| crate::types::ModuleFunctionInfo {
                name: f.name,
                line: f.line,
            })
            .collect();

        let module_imports = imports
            .into_iter()
            .map(|i| crate::types::ModuleImportInfo {
                module: i.module,
                items: i.items,
            })
            .collect();

        let line_count = source.lines().count();

        Ok(crate::types::ModuleInfo::new(
            String::new(), // Will be set by caller
            line_count,
            language.to_string(),
            module_functions,
            module_imports,
        ))
    }

    // Extracts function and class definitions from a pre-parsed syntax tree.
    #[allow(clippy::too_many_arguments)]
    fn extract_elements(
        source: &str,
        compiled: &CompiledQueries,
        root: Node<'_>,
        max_depth: Option<u32>,
        lang_info: &crate::languages::LanguageInfo,
        functions: &mut Vec<FunctionInfo>,
        classes: &mut Vec<ClassInfo>,
        tc: TimeoutConfig,
    ) -> Result<(), ParserError> {
        let mut seen_functions = std::collections::HashSet::new();
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
                let mut func_name_text: Option<String> = None;
                let mut class_node: Option<Node> = None;
                let mut class_name_text: Option<String> = None;

                for capture in mat.captures {
                    let capture_name = compiled.element.capture_names()[capture.index as usize];
                    let node = capture.node;
                    match capture_name {
                        "function" => func_node = Some(node),
                        "func_name" | "method_name" => {
                            func_name_text =
                                Some(source[node.start_byte()..node.end_byte()].to_string());
                        }
                        "class" => class_node = Some(node),
                        "class_name" | "type_name" => {
                            class_name_text =
                                Some(source[node.start_byte()..node.end_byte()].to_string());
                        }
                        _ => {}
                    }
                }

                if let Some(func_node) = func_node {
                    // When a plain function_definition is nested inside a template_declaration
                    // or decorated_definition, it is also matched by the explicit wrapper pattern.
                    // Skip it here to avoid duplicates; the wrapper match will emit it.
                    let parent_kind = func_node.parent().map(|p| p.kind());
                    let parent_is_wrapper = parent_kind
                        .map(|k| k == "template_declaration" || k == "decorated_definition")
                        .unwrap_or(false);
                    if func_node.kind() == "function_definition" && parent_is_wrapper {
                        // Handled by the template_declaration or decorated_definition @function match instead.
                    } else {
                        // Resolve template_declaration or decorated_definition to inner function_definition
                        // for declarator/field walks. The captured node may be a wrapper.
                        let func_def = if func_node.kind() == "template_declaration" {
                            let mut cursor = func_node.walk();
                            func_node
                                .children(&mut cursor)
                                .find(|n| n.kind() == "function_definition")
                                .unwrap_or(func_node)
                        } else if func_node.kind() == "decorated_definition" {
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
    fn enclosing_function_name(mut node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
        let mut depth = 0u32;
        while let Some(parent) = node.parent() {
            depth += 1;
            // Cap at 64 hops: real function nesting rarely exceeds ~10 levels; 64 is a generous
            // upper bound that guards against pathological/malformed ASTs without false negatives
            // on legitimate code. Returns None (treated as <module>) when the cap is hit.
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
        // The loop exits here only when no parent was found (i.e., we reached the tree root
        // without finding a function container). If the depth cap fired, we returned None early
        // above. Nothing to assert here.
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn extract_calls(
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

                    let caller = Self::enclosing_function_name(node, source)
                        .unwrap_or_else(|| "<module>".to_string());

                    let mut arg_count = None;
                    let mut arg_node = node;
                    let mut hop = 0u32;
                    let mut cap_hit = false;
                    while let Some(parent) = arg_node.parent() {
                        hop += 1;
                        // Bounded parent traversal: cap at 16 hops to guard against pathological
                        // walks on malformed/degenerate trees. Real call-expression nesting is
                        // shallow (typically 1-3 levels). When the cap is hit we stop searching and
                        // leave arg_count as None; the caller is still recorded, just without
                        // argument-count information.
                        if hop > 16 {
                            cap_hit = true;
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
                    debug_assert!(
                        !cap_hit,
                        "extract_calls: parent traversal cap reached (hop > 16)"
                    );

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

    // Extracts import statements from a pre-parsed syntax tree.
    fn extract_imports(
        source: &str,
        compiled: &CompiledQueries,
        root: Node<'_>,
        max_depth: Option<u32>,
        imports: &mut Vec<ImportInfo>,
        tc: TimeoutConfig,
    ) -> Result<(), ParserError> {
        let Some(ref import_query) = compiled.import else {
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
                    if capture_name == "import_path" {
                        let node = capture.node;
                        let line = node.start_position().row + 1;
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

    fn extract_impl_methods(
        source: &str,
        compiled: &CompiledQueries,
        root: Node<'_>,
        max_depth: Option<u32>,
        classes: &mut [ClassInfo],
        tc: TimeoutConfig,
    ) -> Result<(), ParserError> {
        let Some(ref impl_query) = compiled.impl_block else {
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

    fn extract_references(
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
        let mut seen_refs = std::collections::HashSet::new();
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
    fn extract_impl_traits_from_tree(
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

    /// Extract def-use sites (write/read locations) for a given symbol within a file.
    ///
    /// Runs the defuse query to find all definition and use sites of a symbol.
    /// Returns empty vec if no defuse query is available for this language.
    ///
    /// # Arguments
    ///
    /// * `source` - The source code text
    /// * `compiled` - Compiled tree-sitter queries
    /// * `root` - Root node of the AST
    /// * `symbol_name` - The symbol to search for (must match exactly)
    /// * `file_path` - Relative file path for site reporting
    fn extract_def_use(
        source: &str,
        compiled: &CompiledQueries,
        root: Node<'_>,
        symbol_name: &str,
        file_path: &str,
        max_depth: Option<u32>,
    ) -> Vec<crate::types::DefUseSite> {
        let Some(ref defuse_query) = compiled.defuse else {
            return vec![];
        };

        let mut sites = Vec::new();
        let source_lines: Vec<&str> = source.lines().collect();
        // Track byte offsets that already have a write or writeread capture so
        // duplicate read captures for the same identifier are suppressed.
        let mut write_offsets = std::collections::HashSet::new();

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
                    if kind == crate::types::DefUseKind::Read
                        && write_offsets.contains(&byte_offset)
                    {
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
                    let enclosing_scope = Self::enclosing_function_name(node, source);

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

    /// Parse `source` in `language`, run the defuse query for `symbol`, and return all sites.
    /// Returns an empty vec if the language has no defuse query or parsing fails.
    pub(crate) fn extract_def_use_for_file(
        source: &str,
        language: &str,
        symbol: &str,
        file_path: &str,
        ast_recursion_limit: Option<usize>,
    ) -> Vec<crate::types::DefUseSite> {
        let Some(lang_info) = crate::languages::get_language_info(language) else {
            return vec![];
        };
        let Ok(compiled) = get_compiled_queries(language) else {
            return vec![];
        };
        if compiled.defuse.is_none() {
            return vec![];
        }

        let tree = match PARSER.with(|p| {
            let mut parser = p.borrow_mut();
            if parser.set_language(&lang_info.language).is_err() {
                return None;
            }
            parser.parse(source, None)
        }) {
            Some(t) => t,
            None => return vec![],
        };

        let root = tree.root_node();

        // Convert ast_recursion_limit the same way extract() does:
        // 0 means unlimited (None); positive values become Some(u32).
        let max_depth: Option<u32> = ast_recursion_limit
            .filter(|&limit| limit > 0)
            .and_then(|limit| u32::try_from(limit).ok());

        Self::extract_def_use(source, compiled, root, symbol, file_path, max_depth)
    }
}

/// Extract `impl Trait for Type` blocks from Rust source.
///
/// Runs independently of `extract_references` to avoid shared deduplication state.
/// Returns an empty vec for non-Rust source (no error; caller decides).
#[must_use]
pub fn extract_impl_traits(source: &str, path: &Path) -> Vec<ImplTraitInfo> {
    let Some(lang_info) = get_language_info("rust") else {
        return vec![];
    };

    let Ok(compiled) = get_compiled_queries("rust") else {
        return vec![];
    };

    let Some(query) = &compiled.impl_trait else {
        return vec![];
    };

    let Some(tree) = PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        let _ = parser.set_language(&lang_info.language);
        parser.parse(source, None)
    }) else {
        return vec![];
    };

    let root = tree.root_node();
    let mut results = Vec::new();

    QUERY_CURSOR.with(|c| {
        let mut cursor = c.borrow_mut();
        cursor.set_max_start_depth(None);
        let mut matches = cursor.matches(query, root, source.as_bytes());

        while let Some(mat) = matches.next() {
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
                    path: path.to_path_buf(),
                    line,
                });
            }
        }
    });

    results
}

/// Execute a custom tree-sitter query against source code.
///
/// This is the internal implementation of the public `execute_query` function.
pub(crate) fn execute_query_impl(
    language: &str,
    source: &str,
    query_str: &str,
) -> Result<Vec<crate::QueryCapture>, ParserError> {
    // Get the tree-sitter language from the language name
    let ts_language = crate::languages::get_ts_language(language)
        .ok_or_else(|| ParserError::UnsupportedLanguage(language.to_string()))?;

    let mut parser = Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(|e| ParserError::QueryError(e.to_string()))?;

    let tree = parser
        .parse(source.as_bytes(), None)
        .ok_or_else(|| ParserError::QueryError("failed to parse source".to_string()))?;

    let query =
        Query::new(&ts_language, query_str).map_err(|e| ParserError::QueryError(e.to_string()))?;

    let source_bytes = source.as_bytes();

    let mut captures = Vec::new();
    QUERY_CURSOR.with(|c| {
        let mut cursor = c.borrow_mut();
        cursor.set_max_start_depth(None);
        let mut matches = cursor.matches(&query, tree.root_node(), source_bytes);
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let node = cap.node;
                let capture_name = query.capture_names()[cap.index as usize].to_string();
                let text = node.utf8_text(source_bytes).unwrap_or("").to_string();
                captures.push(crate::QueryCapture {
                    capture_name,
                    text,
                    start_line: node.start_position().row,
                    end_line: node.end_position().row,
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                });
            }
        }
    });
    Ok(captures)
}

// Language-feature-gated tests (require lang-rust); see also tests_unsupported below
#[cfg(all(test, feature = "lang-rust"))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_ast_recursion_limit_zero_is_unlimited() {
        let source = r#"fn hello() -> u32 { 42 }"#;
        let result_none = SemanticExtractor::extract(source, "rust", None, None);
        let result_zero = SemanticExtractor::extract(source, "rust", Some(0), None);
        assert!(result_none.is_ok(), "extract with None failed");
        assert!(result_zero.is_ok(), "extract with Some(0) failed");
        let analysis_none = result_none.unwrap();
        let analysis_zero = result_zero.unwrap();
        assert!(
            analysis_none.functions.len() >= 1,
            "extract with None should find at least one function in the test source"
        );
        assert_eq!(
            analysis_none.functions.len(),
            analysis_zero.functions.len(),
            "ast_recursion_limit=0 should behave identically to unset (unlimited)"
        );
    }

    #[test]
    fn test_rust_use_as_imports() {
        // Arrange
        let source = "use std::io as stdio;";
        // Act
        let result = SemanticExtractor::extract(source, "rust", None, None).unwrap();
        // Assert: alias "stdio" is captured as an import item
        assert!(
            result
                .imports
                .iter()
                .any(|imp| imp.items.iter().any(|i| i == "stdio")),
            "expected import alias 'stdio' in {:?}",
            result.imports
        );
    }

    #[test]
    fn test_rust_use_as_clause_plain_identifier() {
        // Arrange: use_as_clause with plain identifier (no scoped_identifier)
        // exercises the _ => prefix.to_string() arm
        let source = "use io as stdio;";
        // Act
        let result = SemanticExtractor::extract(source, "rust", None, None).unwrap();
        // Assert: alias "stdio" is captured as an import item
        assert!(
            result
                .imports
                .iter()
                .any(|imp| imp.items.iter().any(|i| i == "stdio")),
            "expected import alias 'stdio' from plain identifier in {:?}",
            result.imports
        );
    }

    #[test]
    fn test_rust_scoped_use_with_prefix() {
        // Arrange: scoped_use_list with non-empty prefix
        let source = "use std::{io::Read, io::Write};";
        // Act
        let result = SemanticExtractor::extract(source, "rust", None, None).unwrap();
        // Assert: both Read and Write appear as items with std::io module
        let items: Vec<String> = result
            .imports
            .iter()
            .filter(|imp| imp.module.starts_with("std::io"))
            .flat_map(|imp| imp.items.clone())
            .collect();
        assert!(
            items.contains(&"Read".to_string()) && items.contains(&"Write".to_string()),
            "expected 'Read' and 'Write' items under module with std::io, got {:?}",
            result.imports
        );
    }

    #[test]
    fn test_rust_scoped_use_imports() {
        // Arrange
        let source = "use std::{fs, io};";
        // Act
        let result = SemanticExtractor::extract(source, "rust", None, None).unwrap();
        // Assert: both "fs" and "io" appear as import items under module "std"
        let items: Vec<&str> = result
            .imports
            .iter()
            .filter(|imp| imp.module == "std")
            .flat_map(|imp| imp.items.iter().map(|s| s.as_str()))
            .collect();
        assert!(
            items.contains(&"fs") && items.contains(&"io"),
            "expected 'fs' and 'io' items under module 'std', got {:?}",
            items
        );
    }

    #[test]
    fn test_rust_wildcard_imports() {
        // Arrange
        let source = "use std::io::*;";
        // Act
        let result = SemanticExtractor::extract(source, "rust", None, None).unwrap();
        // Assert: wildcard import with module "std::io"
        let wildcard = result
            .imports
            .iter()
            .find(|imp| imp.module == "std::io" && imp.items == vec!["*"]);
        assert!(
            wildcard.is_some(),
            "expected wildcard import with module 'std::io', got {:?}",
            result.imports
        );
    }

    #[test]
    fn test_extract_impl_traits_standalone() {
        // Arrange: source with a simple impl Trait for Type
        let source = r#"
struct Foo;
trait Display {}
impl Display for Foo {}
"#;
        // Act
        let results = extract_impl_traits(source, Path::new("test.rs"));
        // Assert
        assert_eq!(
            results.len(),
            1,
            "expected one impl trait, got {:?}",
            results
        );
        assert_eq!(results[0].trait_name, "Display");
        assert_eq!(results[0].impl_type, "Foo");
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_ast_recursion_limit_overflow() {
        // Arrange: limit larger than u32::MAX triggers a ParseError on 64-bit targets
        let source = "fn foo() {}";
        let big_limit = usize::try_from(u32::MAX).unwrap() + 1;
        // Act
        let result = SemanticExtractor::extract(source, "rust", Some(big_limit), None);
        // Assert
        assert!(
            matches!(result, Err(ParserError::ParseError(_))),
            "expected ParseError for oversized limit, got {:?}",
            result
        );
    }

    #[test]
    fn test_ast_recursion_limit_some() {
        // Arrange: ast_recursion_limit with Some(depth) to exercise max_depth Some branch
        let source = r#"fn hello() -> u32 { 42 }"#;
        // Act
        let result = SemanticExtractor::extract(source, "rust", Some(5), None);
        // Assert: should succeed without error and extract functions
        assert!(result.is_ok(), "extract with Some(5) failed: {:?}", result);
        let analysis = result.unwrap();
        assert!(
            analysis.functions.len() >= 1,
            "expected at least one function with depth limit 5"
        );
    }

    #[test]
    fn test_extract_def_use_for_file_finds_write_and_read() {
        // Arrange
        let source = r#"
fn main() {
    let count = 0;
    println!("{}", count);
}
"#;
        // Act
        let sites = SemanticExtractor::extract_def_use_for_file(
            source,
            "rust",
            "count",
            "src/main.rs",
            None,
        );

        // Assert
        assert!(
            !sites.is_empty(),
            "expected at least one def-use site for 'count'"
        );
        let has_write = sites
            .iter()
            .any(|s| s.kind == crate::types::DefUseKind::Write);
        let has_read = sites
            .iter()
            .any(|s| s.kind == crate::types::DefUseKind::Read);
        assert!(has_write, "expected a write site for 'count'");
        assert!(has_read, "expected a read site for 'count'");
        assert_eq!(sites[0].file, "src/main.rs");
    }

    #[test]
    fn test_extract_def_use_for_file_no_match_returns_empty() {
        // Arrange
        let source = "fn foo() { let x = 1; }";

        // Act
        let sites = SemanticExtractor::extract_def_use_for_file(
            source,
            "rust",
            "nonexistent_symbol",
            "src/lib.rs",
            None,
        );

        // Assert
        assert!(sites.is_empty(), "expected empty for nonexistent symbol");
    }
}

// Language-feature-gated tests for Python
#[cfg(all(test, feature = "lang-python"))]
mod tests_python {
    use super::*;

    #[test]
    fn test_python_relative_import() {
        // Arrange: relative import (from . import foo)
        let source = "from . import foo\n";
        // Act
        let result = SemanticExtractor::extract(source, "python", None, None).unwrap();
        // Assert: relative import should be captured
        let relative = result.imports.iter().find(|imp| imp.module.contains("."));
        assert!(
            relative.is_some(),
            "expected relative import in {:?}",
            result.imports
        );
    }

    #[test]
    fn test_python_aliased_import() {
        // Arrange: aliased import (from os import path as p)
        // Note: tree-sitter-python extracts "path" (the original name), not the alias "p"
        let source = "from os import path as p\n";
        // Act
        let result = SemanticExtractor::extract(source, "python", None, None).unwrap();
        // Assert: "path" should be in items (alias is captured separately by aliased_import node)
        let path_import = result
            .imports
            .iter()
            .find(|imp| imp.module == "os" && imp.items.iter().any(|i| i == "path"));
        assert!(
            path_import.is_some(),
            "expected import 'path' from module 'os' in {:?}",
            result.imports
        );
    }

    #[test]
    fn test_parse_no_timeout_when_none() {
        // Arrange: simple Rust source with no deadline
        let source = r#"fn hello() -> u32 { 42 }"#;
        // Act: extract with deadline=None (no timeout)
        let result = SemanticExtractor::extract(source, "rust", None, None);
        // Assert: should succeed normally
        assert!(result.is_ok(), "extract with deadline=None should succeed");
        let analysis = result.unwrap();
        assert!(
            analysis.functions.len() >= 1,
            "should find at least one function"
        );
    }

    #[test]
    fn test_parse_timeout_triggers_error() {
        // Arrange: simple Rust source with a very short timeout (1 microsecond)
        let source = r#"fn hello() -> u32 { 42 }"#;
        // Act: extract with a very short timeout that will expire immediately
        let result = SemanticExtractor::extract(source, "rust", None, Some(1u64));
        // Assert: should return a Timeout error
        assert!(
            matches!(result, Err(ParserError::Timeout(_))),
            "expected Timeout error, got {:?}",
            result
        );
    }
}

// Tests that do not require any language feature gate
#[cfg(test)]
mod tests_unsupported {
    use super::*;

    #[test]
    fn test_element_extractor_unsupported_language() {
        // Arrange + Act
        let result = ElementExtractor::extract_with_depth("x = 1", "cobol");
        // Assert
        assert!(
            matches!(result, Err(ParserError::UnsupportedLanguage(ref lang)) if lang == "cobol"),
            "expected UnsupportedLanguage error, got {:?}",
            result
        );
    }

    #[test]
    fn test_semantic_extractor_unsupported_language() {
        // Arrange + Act
        let result = SemanticExtractor::extract("x = 1", "cobol", None, None);
        // Assert
        assert!(
            matches!(result, Err(ParserError::UnsupportedLanguage(ref lang)) if lang == "cobol"),
            "expected UnsupportedLanguage error, got {:?}",
            result
        );
    }
}
