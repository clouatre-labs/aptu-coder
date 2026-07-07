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

use crate::languages::get_language_info;
use crate::types::{ImplTraitInfo, SemanticAnalysis};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;
use thiserror::Error;
use tracing::instrument;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

// Import extracted element handlers from parser_elements module
use crate::parser_elements::{
    extract_calls, extract_def_use, extract_elements, extract_impl_methods,
    extract_impl_traits_from_tree, extract_imports, extract_references,
};

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
pub(crate) struct TimeoutConfig {
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
    pub(crate) fn is_exceeded(self) -> bool {
        self.deadline
            .is_some_and(|d| std::time::Instant::now() >= d)
    }
}

/// Compiled tree-sitter queries for a language.
/// Stores all query types: mandatory (element, call) and optional (import, impl, reference).
pub(crate) struct CompiledQueries {
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
/// Compiles all tree-sitter queries for a language, including mandatory queries
/// (element, call) and optional queries (import, impl, reference, impl_trait, defuse).
/// Returns an error if any query fails to compile.
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

    let reference = if let Some(reference_query_str) = lang_info.reference_query {
        Some(
            Query::new(&lang_info.language, reference_query_str).map_err(|e| {
                ParserError::QueryError(format!(
                    "Failed to compile reference query for {}: {}",
                    lang_info.name, e
                ))
            })?,
        )
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
    pub(crate) static PARSER: RefCell<Parser> = RefCell::new(Parser::new());
    pub(crate) static QUERY_CURSOR: RefCell<QueryCursor> = RefCell::new(QueryCursor::new());
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

/// Canonical API for detailed semantic analysis of source code.
pub struct SemanticExtractor;

impl SemanticExtractor {
    /// Extract detailed semantic information from source code.
    ///
    /// This is the main entry point for comprehensive semantic analysis. It extracts:
    /// - Function and class definitions
    /// - Function calls and call frequency
    /// - Import statements
    /// - Type references
    /// - Impl trait blocks (Rust only)
    ///
    /// # Arguments
    ///
    /// * `source` - The source code as a string
    /// * `language` - The programming language (e.g., "rust", "python")
    /// * `ast_recursion_limit` - Optional AST recursion depth limit (0 = unlimited)
    /// * `timeout_micros` - Optional timeout in microseconds
    ///
    /// # Returns
    ///
    /// A `SemanticAnalysis` containing all extracted semantic information.
    ///
    /// # Errors
    ///
    /// Returns a `ParserError` if:
    /// * `ParserError::Timeout` - The operation exceeds the specified timeout
    /// * `ParserError::UnsupportedLanguage` - The language is not supported
    /// * `ParserError::ParseError` - Tree-sitter parsing fails
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

        // Convert ast_recursion_limit: 0 means unlimited (None); positive values become Some(u32).
        let max_depth: Option<u32> = ast_recursion_limit
            .filter(|&limit| limit > 0)
            .and_then(|limit| u32::try_from(limit).ok());

        let mut functions = Vec::new();
        let mut classes = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let mut calls = Vec::new();
        let mut call_frequency = HashMap::new();

        // Extract functions and classes
        extract_elements(
            source,
            compiled,
            root,
            max_depth,
            &mut functions,
            &mut classes,
            tc,
            &lang_info,
        )?;

        // Check deadline after extract_elements
        if tc.is_exceeded() {
            return Err(ParserError::Timeout(tc.micros));
        }

        extract_calls(
            source,
            compiled,
            root,
            max_depth,
            &mut calls,
            &mut call_frequency,
            tc,
        )?;
        extract_imports(source, compiled, root, max_depth, &mut imports, tc)?;
        extract_impl_methods(source, compiled, root, max_depth, &mut classes, tc)?;
        extract_references(source, compiled, root, max_depth, &mut references, tc)?;

        // Extract impl-trait blocks for Rust files (empty for other languages)
        let impl_traits = if language == "rust" {
            extract_impl_traits_from_tree(source, compiled, root, tc)?
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
        extract_elements(
            source,
            compiled,
            root,
            None,
            &mut functions,
            &mut classes,
            tc,
            &lang_info,
        )?;

        // Check deadline after extract_elements
        if tc.is_exceeded() {
            return Err(ParserError::Timeout(tc.micros));
        }

        // Extract imports
        extract_imports(source, compiled, root, None, &mut imports, tc)?;

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

    /// Parse `source` in `language`, run the defuse query for `symbol`, and return all sites.
    /// Returns an empty vec if the language has no defuse query or parsing fails.
    pub(crate) fn extract_def_use_for_file(
        source: &str,
        language: &str,
        symbol: &str,
        file_path: &str,
        ast_recursion_limit: Option<usize>,
    ) -> Vec<crate::types::DefUseSite> {
        let Some(lang_info) = get_language_info(language) else {
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

        extract_def_use(source, compiled, root, symbol, file_path, max_depth)
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

#[cfg(test)]
// Tests for Rust language parsing
mod tests_rust {
    use super::*;
    use crate::types::CallInfo;

    #[test]
    fn test_ast_recursion_limit_zero_is_unlimited() {
        // Arrange: simple Rust source
        let source = r#"fn hello() -> u32 { 42 }"#;
        // Act: extract with ast_recursion_limit=0 (unlimited)
        let result = SemanticExtractor::extract(source, "rust", Some(0), None);
        // Assert: should succeed and find the function
        assert!(result.is_ok(), "extract with limit=0 should succeed");
        let analysis = result.unwrap();
        assert_eq!(
            analysis.functions.len(),
            1,
            "should find exactly one function"
        );
    }

    #[test]
    fn test_rust_use_as_imports() {
        // Arrange: Rust use-as import
        let source = "use std::io as stdio;\n";
        // Act
        let result = SemanticExtractor::extract(source, "rust", None, None).unwrap();
        // Assert: should capture the alias "stdio"
        let stdio_import = result
            .imports
            .iter()
            .find(|imp| imp.items.iter().any(|i| i == "stdio"));
        assert!(
            stdio_import.is_some(),
            "expected import with alias 'stdio' in {:?}",
            result.imports
        );
    }

    #[test]
    fn test_rust_use_as_clause_plain_identifier() {
        // Arrange: plain identifier with alias
        let source = "use io as stdio;\n";
        // Act
        let result = SemanticExtractor::extract(source, "rust", None, None).unwrap();
        // Assert: should capture the alias
        let alias_import = result
            .imports
            .iter()
            .find(|imp| imp.items.iter().any(|i| i == "stdio"));
        assert!(
            alias_import.is_some(),
            "expected import with alias 'stdio' in {:?}",
            result.imports
        );
    }

    #[test]
    fn test_rust_scoped_use_with_prefix() {
        // Arrange: scoped use with prefix
        let source = "use std::{io, fs};\n";
        // Act
        let result = SemanticExtractor::extract(source, "rust", None, None).unwrap();
        // Assert: should capture both io and fs
        let has_io = result
            .imports
            .iter()
            .any(|imp| imp.items.iter().any(|i| i == "io"));
        let has_fs = result
            .imports
            .iter()
            .any(|imp| imp.items.iter().any(|i| i == "fs"));
        assert!(has_io, "expected import 'io' in {:?}", result.imports);
        assert!(has_fs, "expected import 'fs' in {:?}", result.imports);
    }

    #[test]
    fn test_rust_scoped_use_imports() {
        // Arrange: scoped use imports
        let source = "use std::{io, fs};\n";
        // Act
        let result = SemanticExtractor::extract(source, "rust", None, None).unwrap();
        // Assert: should capture both imports
        assert!(
            !result.imports.is_empty(),
            "expected imports in {:?}",
            result.imports
        );
    }

    #[test]
    fn test_rust_wildcard_imports() {
        // Arrange: wildcard import
        let source = "use std::*;\n";
        // Act
        let result = SemanticExtractor::extract(source, "rust", None, None).unwrap();
        // Assert: should capture wildcard
        let wildcard = result
            .imports
            .iter()
            .find(|imp| imp.items.iter().any(|i| i == "*"));
        assert!(
            wildcard.is_some(),
            "expected wildcard import in {:?}",
            result.imports
        );
    }

    #[test]
    fn test_extract_impl_traits_standalone() {
        // Arrange: Rust impl trait block
        let source = r#"
            trait MyTrait {
                fn method(&self);
            }
            impl MyTrait for MyType {
                fn method(&self) {}
            }
        "#;
        // Act
        let result = extract_impl_traits(source, Path::new("test.rs"));
        // Assert: should find the impl trait
        assert!(
            !result.is_empty(),
            "expected impl trait in result, got {:?}",
            result
        );
    }

    #[test]
    fn test_ast_recursion_limit_overflow() {
        // Arrange: simple Rust source with very large recursion limit
        let source = r#"fn hello() -> u32 { 42 }"#;
        // Act: extract with ast_recursion_limit=usize::MAX (will overflow to None)
        let result = SemanticExtractor::extract(source, "rust", Some(usize::MAX), None);
        // Assert: should still succeed (overflow is handled gracefully)
        assert!(
            result.is_ok(),
            "extract with limit=usize::MAX should succeed"
        );
    }

    #[test]
    fn test_ast_recursion_limit_some() {
        // Arrange: simple Rust source
        let source = r#"fn hello() -> u32 { 42 }"#;
        // Act: extract with ast_recursion_limit=10
        let result = SemanticExtractor::extract(source, "rust", Some(10), None);
        // Assert: should succeed and find the function
        assert!(result.is_ok(), "extract with limit=10 should succeed");
        let analysis = result.unwrap();
        assert_eq!(
            analysis.functions.len(),
            1,
            "should find exactly one function"
        );
    }

    #[test]
    fn test_extract_def_use_for_file_finds_write_and_read() {
        // Arrange: Rust source with variable write and read
        let source = r#"
            fn test() {
                let mut x = 5;
                x = 10;
                let y = x;
            }
        "#;
        // Act
        let result =
            SemanticExtractor::extract_def_use_for_file(source, "rust", "x", "test.rs", None);
        // Assert: should find both write and read sites
        let has_write = result
            .iter()
            .any(|s| s.kind == crate::types::DefUseKind::Write);
        let has_read = result
            .iter()
            .any(|s| s.kind == crate::types::DefUseKind::Read);
        assert!(has_write, "expected write site for 'x'");
        assert!(has_read, "expected read site for 'x'");
    }

    #[test]
    fn test_extract_def_use_for_file_no_match_returns_empty() {
        // Arrange: Rust source without the target symbol
        let source = r#"
            fn test() {
                let x = 5;
            }
        "#;
        // Act
        let result = SemanticExtractor::extract_def_use_for_file(
            source,
            "rust",
            "nonexistent",
            "test.rs",
            None,
        );
        // Assert: should return empty vec
        assert!(
            result.is_empty(),
            "expected empty result for nonexistent symbol"
        );
    }

    #[test]
    fn extract_calls_does_not_panic_on_function_calls() {
        // Arrange: Rust source with function calls
        let src = r#"
            fn foo() {}
            fn bar() {
                foo();
            }
        "#;
        // Act
        let result = SemanticExtractor::extract(src, "rust", None, None);
        // Assert: should succeed and extract calls
        assert!(
            result.is_ok(),
            "extract must succeed on source with function calls"
        );
        let output = result.unwrap();
        assert!(
            !output.calls.is_empty(),
            "extract must return call entries for source with function calls"
        );
    }

    #[test]
    fn extract_calls_caps_arg_count_at_sixteen_hops() {
        // Regression test for #1251: verify deeply nested parenthesized arguments
        // (20 levels) do not cause a panic. The inner call `g()` is always at 1 hop
        // from its enclosing call_expression (function identifier is a direct child),
        // so arg_count is Some(0) -- the cap only fires when the captured node is
        // deeper in the AST than the direct function child of a call_expression.
        let src = r#"fn main() { f((((((((((((((((((((g())))))))))))))))))))); }"#;
        let result = SemanticExtractor::extract(src, "rust", None, None);
        assert!(
            result.is_ok(),
            "extract must succeed even with deeply nested parenthesized arguments"
        );
        let output = result.unwrap();
        let g_calls: Vec<&CallInfo> = output.calls.iter().filter(|c| c.callee == "g").collect();
        assert_eq!(
            g_calls.len(),
            1,
            "expected exactly one CallInfo with callee 'g', got {}",
            g_calls.len()
        );
        // arg_count is Some(0) because g() has 0 arguments; the >16 hop cap is
        // a parent-traversal guard on the captured node, not on the call nesting.
        assert_eq!(
            g_calls[0].arg_count,
            Some(0),
            "g() has 0 arguments, expected Some(0)"
        );
    }
}

#[cfg(test)]
// Tests for Python language parsing
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
