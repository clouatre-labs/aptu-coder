// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Multi-language code structure analysis library using tree-sitter.
//!
//! This crate provides core analysis functionality for extracting code structure
//! from multiple programming languages. It is designed to be used as a library
//! by MCP servers and other tools.
//!
//! # Features
//!
//! - **Language support**: Astro, C/C++, C#, CSS, Fortran, Go, HTML, Java, JavaScript, JSON, Kotlin, Markdown, Python, Rust, TOML, TSX, TypeScript, YAML (feature-gated)
//! - **Schema generation**: Optional JSON schema support via the `schemars` feature
//! - **Async-friendly**: Uses tokio for concurrent analysis
//! - **Cancellation support**: Built-in cancellation token support
//!
//! # Examples
//!
//! ```no_run
//! use aptu_coder_core::analyze::analyze_directory;
//! use std::path::Path;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let output = analyze_directory(Path::new("src"), None)?;
//! println!("Files: {:?}", output.files.len());
//! # Ok(())
//! # }
//! ```

#![cfg_attr(test, allow(clippy::unwrap_used))]
#![cfg_attr(test, allow(clippy::expect_used))]

pub mod analyze;
pub mod cache;
pub mod cache_disk;
pub mod completion;
mod config;
pub mod edit;
pub mod formatter;
pub mod formatter_defuse;
pub mod graph;
pub mod lang;
pub mod languages;
pub mod pagination;
pub mod parser;
pub(crate) mod parser_elements;
pub mod test_detection;
pub mod traversal;
pub mod types;

#[cfg(feature = "schemars")]
pub mod schema_helpers;

pub const EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    ".git",
    "__pycache__",
    "target",
    "dist",
    "build",
    ".venv",
];

// Re-exports of key public APIs
pub use analyze::{
    AnalysisOutput, AnalyzeError, CallChainEntry, FileAnalysisOutput, FocusedAnalysisConfig,
    FocusedAnalysisOutput, analyze_directory, analyze_directory_with_progress, analyze_file,
    analyze_focused, analyze_focused_with_progress, analyze_focused_with_progress_with_entries,
    analyze_module_file, analyze_str,
};
pub use config::AnalysisConfig;
pub use edit::{EditError, edit_overwrite_content, edit_replace_block};
pub use graph::{GraphError, InternalCallChain};
pub use lang::{language_for_extension, supported_languages};
pub use pagination::{CursorData, PaginationError};
pub use parser::ParserError;
pub use traversal::{TraversalError, WalkEntry};
pub use types::{
    AnalysisMode, AnalyzeDirectoryParams, AnalyzeFileField, AnalyzeFileParams, AnalyzeModuleParams,
    AnalyzeSymbolParams, CallEdge, CallInfo, ClassInfo, DefUseKind, DefUseSite,
    EditOverwriteOutput, EditOverwriteParams, EditReplaceOutput, EditReplaceParams, FileInfo,
    FilterRule, FunctionInfo, ImplTraitInfo, ImportInfo, ModuleFunctionInfo, ModuleImportInfo,
    ModuleInfo, OutputControlParams, PaginationParams, ReferenceInfo, ReferenceType,
    SemanticAnalysis, SymbolMatchMode,
};

/// Captures from a custom tree-sitter query.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueryCapture {
    /// The capture name from the query (without leading `@`).
    pub capture_name: String,
    /// The matched source text.
    pub text: String,
    /// Start line (0-indexed).
    pub start_line: usize,
    /// End line (0-indexed, inclusive).
    pub end_line: usize,
    /// Start byte offset.
    pub start_byte: usize,
    /// End byte offset.
    pub end_byte: usize,
}

/// Execute a custom tree-sitter query against source code.
///
/// # Arguments
///
/// * `language` - Language name (e.g., "rust", "python"). Must be an enabled language feature.
/// * `source` - Source code to query.
/// * `query` - A tree-sitter query string (S-expression syntax).
///
/// # Returns
///
/// A vector of [`QueryCapture`] results, or a [`ParserError`] if the query is malformed
/// or the language is not supported.
///
/// # Security note
///
/// This function accepts user-controlled `query` strings. Pathological queries against
/// large `source` inputs may cause CPU exhaustion. Callers in untrusted environments
/// should bound the length of both `source` and `query` before calling this function.
/// `Query::new()` returns `Err` on malformed queries rather than panicking.
pub fn execute_query(
    language: &str,
    source: &str,
    query: &str,
) -> Result<Vec<QueryCapture>, parser::ParserError> {
    parser::execute_query_impl(language, source, query)
}
