// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Main analysis engine for extracting code structure from files and directories.
//!
//! Implements the four MCP tools: `analyze_directory` (Overview), `analyze_file` (`FileDetails`),
//! `analyze_symbol` (call graph), and `analyze_module` (lightweight index). Handles parallel processing and cancellation.

use crate::formatter::{format_file_details, format_structure};
use crate::graph::InternalCallChain;
use crate::lang::{language_for_extension, supported_languages};
use crate::parser::{ElementExtractor, SemanticExtractor};
use crate::test_detection::is_test_file;
use crate::traversal::{WalkEntry, walk_directory};
use crate::types::{AnalysisMode, FileInfo, SemanticAnalysis, SymbolMatchMode};
use rayon::prelude::*;
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

pub const MAX_FILE_SIZE_BYTES: u64 = 10_000_000;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AnalyzeError {
    #[error("Traversal error: {0}")]
    Traversal(#[from] crate::traversal::TraversalError),
    #[error("Parser error: {0}")]
    Parser(#[from] crate::parser::ParserError),
    #[error("Graph error: {0}")]
    Graph(#[from] crate::graph::GraphError),
    #[error("Formatter error: {0}")]
    Formatter(#[from] crate::formatter::FormatterError),
    #[error("Analysis cancelled")]
    Cancelled,
    #[error("unsupported language: {0}")]
    UnsupportedLanguage(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid range: start ({start}) > end ({end}); file has {total} lines")]
    InvalidRange {
        start: usize,
        end: usize,
        total: usize,
    },
    #[error("path is a directory, not a file: {0}")]
    NotAFile(PathBuf),
    #[error(
        "file has {total_lines} lines; provide start_line and end_line, or call analyze_module first to locate the range"
    )]
    RangelessLargeFile { total_lines: usize },
    #[error("parse timeout exceeded for {path}: {micros} microseconds")]
    ParseTimeout { path: PathBuf, micros: u64 },
}

/// Result of directory analysis containing both formatted output and file data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[non_exhaustive]
pub struct AnalysisOutput {
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "Formatted text representation of the analysis")
    )]
    pub formatted: String,
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "List of files analyzed in the directory")
    )]
    pub files: Vec<FileInfo>,
    /// Walk entries used internally for summary generation; not serialized.
    #[serde(skip)]
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub entries: Vec<WalkEntry>,
    /// Subtree file counts computed from an unbounded walk; used by `format_summary`; not serialized.
    #[serde(skip)]
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub subtree_counts: Option<Vec<(std::path::PathBuf, usize)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "Opaque cursor token for the next page of results (absent when no more results)"
        )
    )]
    pub next_cursor: Option<String>,
}

/// Result of file-level semantic analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[non_exhaustive]
pub struct FileAnalysisOutput {
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "Formatted text representation of the analysis")
    )]
    pub formatted: String,
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "Semantic analysis data including functions, classes, and imports")
    )]
    pub semantic: SemanticAnalysis,
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "Total line count of the analyzed file")
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub line_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "Opaque cursor token for the next page of results (absent when no more results)"
        )
    )]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "True when the file extension is not supported; semantic fields are empty and formatted contains a raw preview"
        )
    )]
    pub unsupported: Option<bool>,
}

impl FileAnalysisOutput {
    /// Create a new `FileAnalysisOutput`.
    #[must_use]
    pub fn new(
        formatted: String,
        semantic: SemanticAnalysis,
        line_count: usize,
        next_cursor: Option<String>,
    ) -> Self {
        Self {
            formatted,
            semantic,
            line_count,
            next_cursor,
            unsupported: None,
        }
    }
}
/// Reason a file was skipped during eligibility check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipReason {
    Oversized,
    Unreadable,
}

/// Check if a file is eligible for analysis based on size and readability.
///
/// Returns `Ok(content)` when the file should be analyzed, `Err(reason)` to skip it.
fn check_file_eligibility(entry: &WalkEntry) -> Result<String, SkipReason> {
    // Check file size before reading
    if entry.path.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_SIZE_BYTES {
        tracing::debug!("skipping large file: {}", entry.path.display());
        return Err(SkipReason::Oversized);
    }

    // Try to read file content; skip binary or unreadable files
    std::fs::read_to_string(&entry.path).map_err(|_| SkipReason::Unreadable)
}

/// Process a single file entry and extract its analysis data.
fn process_file_entry(entry: &WalkEntry, source: &str) -> FileInfo {
    let path_str = entry.path.display().to_string();
    let line_count = source.lines().count();

    // Detect language from extension
    let ext = entry.path.extension().and_then(|e| e.to_str());

    // Detect language and extract counts
    let (language, function_count, class_count) = if let Some(ext_str) = ext
        && let Some(lang) = language_for_extension(ext_str)
    {
        let lang_str = lang.to_string();
        match ElementExtractor::extract_with_depth(source, &lang_str) {
            Ok((func_count, class_count)) => (lang_str, func_count, class_count),
            Err(_) => (lang_str, 0, 0),
        }
    } else {
        (
            ext.map(|e| e.to_lowercase())
                .unwrap_or_else(|| "unknown".to_string()),
            0,
            0,
        )
    };

    let is_test = is_test_file(&entry.path);

    FileInfo {
        path: path_str,
        line_count,
        function_count,
        class_count,
        language,
        is_test,
    }
}

/// Analyze a single file entry in parallel context.
fn analyze_single_file(
    entry: &WalkEntry,
    progress: &Arc<AtomicUsize>,
    ct: &CancellationToken,
) -> Option<FileInfo> {
    // Check cancellation per file
    if ct.is_cancelled() {
        return None;
    }

    // Check file eligibility; progress accounting happens on all exit paths below
    let source = match check_file_eligibility(entry) {
        Ok(content) => content,
        Err(_) => {
            progress.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };

    let file_info = process_file_entry(entry, &source);
    progress.fetch_add(1, Ordering::Relaxed);

    Some(file_info)
}

/// Initialize analysis context and collect file entries.
fn init_analysis_context(entries: &[WalkEntry]) -> Vec<&WalkEntry> {
    entries
        .iter()
        .filter(|e| !e.is_dir && !e.is_symlink)
        .collect()
}

/// Build the final analysis output from results.
fn build_analysis_output(
    entries: Vec<WalkEntry>,
    analysis_results: Vec<FileInfo>,
) -> AnalysisOutput {
    let formatted = format_structure(&entries, &analysis_results, None);
    AnalysisOutput {
        formatted,
        files: analysis_results,
        entries,
        next_cursor: None,
        subtree_counts: None,
    }
}

/// Run parallel analysis on file entries and log completion.
fn run_parallel_analysis(
    file_entries: &[&WalkEntry],
    progress: &Arc<AtomicUsize>,
    ct: &CancellationToken,
) -> Result<Vec<FileInfo>, AnalyzeError> {
    let start = Instant::now();
    tracing::debug!(file_count = file_entries.len(), "analysis start");

    let _parse_span = tracing::info_span!("ast.parse_batch", count = file_entries.len()).entered();

    // Parallel analysis of files
    let analysis_results: Vec<FileInfo> = file_entries
        .par_iter()
        .filter_map(|entry| analyze_single_file(entry, progress, ct))
        .collect();

    // Check if cancelled after parallel processing
    if ct.is_cancelled() {
        return Err(AnalyzeError::Cancelled);
    }

    tracing::debug!(
        file_count = file_entries.len(),
        duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        "analysis complete"
    );

    Ok(analysis_results)
}

#[instrument(skip_all, fields(path = %root.display()))]
// public API; callers expect owned semantics
#[allow(clippy::needless_pass_by_value)]
pub fn analyze_directory_with_progress(
    root: &Path,
    entries: Vec<WalkEntry>,
    progress: Arc<AtomicUsize>,
    ct: CancellationToken,
) -> Result<AnalysisOutput, AnalyzeError> {
    // Check if already cancelled
    if ct.is_cancelled() {
        return Err(AnalyzeError::Cancelled);
    }

    tracing::debug!(root = %root.display(), "analysis start");

    let file_entries = init_analysis_context(&entries);
    let analysis_results = run_parallel_analysis(&file_entries, &progress, &ct)?;

    let _format_span = tracing::info_span!("output.format").entered();

    // Build and return output
    Ok(build_analysis_output(entries, analysis_results))
}

/// Analyze a directory structure and return formatted output and file data.
#[instrument(skip_all, fields(path = %root.display()))]
pub fn analyze_directory(
    root: &Path,
    max_depth: Option<u32>,
) -> Result<AnalysisOutput, AnalyzeError> {
    let entries = walk_directory(root, max_depth)?;
    let counter = Arc::new(AtomicUsize::new(0));
    let ct = CancellationToken::new();
    analyze_directory_with_progress(root, entries, counter, ct)
}

/// Determine analysis mode based on parameters and path.
#[must_use]
pub fn determine_mode(path: &str, focus: Option<&str>) -> AnalysisMode {
    if focus.is_some() {
        return AnalysisMode::SymbolFocus;
    }

    let path_obj = Path::new(path);
    if path_obj.is_dir() {
        AnalysisMode::Overview
    } else {
        AnalysisMode::FileDetails
    }
}

/// Analyze a single file and return semantic analysis with formatted output.
#[instrument(skip_all, fields(path))]
pub fn analyze_file(
    path: &str,
    ast_recursion_limit: Option<usize>,
) -> Result<FileAnalysisOutput, AnalyzeError> {
    let start = Instant::now();

    // Check file size before reading
    if Path::new(path).metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_SIZE_BYTES {
        tracing::debug!("skipping large file: {}", path);
        return Err(AnalyzeError::Parser(
            crate::parser::ParserError::ParseError("file too large".to_string()),
        ));
    }

    let source = std::fs::read_to_string(path)
        .map_err(|e| AnalyzeError::Parser(crate::parser::ParserError::ParseError(e.to_string())))?;

    let line_count = source.lines().count();

    // Detect language from extension
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(language_for_extension)
        .map_or_else(|| "unknown".to_string(), std::string::ToString::to_string);

    // Extract semantic information
    let mut semantic = SemanticExtractor::extract(&source, &ext, ast_recursion_limit, None)?;

    // Populate the file path on references now that the path is known
    for r in &mut semantic.references {
        r.location = path.to_string();
    }

    // Resolve Python wildcard imports
    if ext == "python" {
        resolve_wildcard_imports(Path::new(path), &mut semantic.imports);
    }

    // Detect if this is a test file
    let is_test = is_test_file(Path::new(path));

    // Extract parent directory for relative path display
    let parent_dir = Path::new(path).parent();

    // Format output
    let formatted = format_file_details(path, &semantic, line_count, is_test, parent_dir);

    tracing::debug!(path = %path, language = %ext, functions = semantic.functions.len(), classes = semantic.classes.len(), imports = semantic.imports.len(), duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX), "file analysis complete");

    Ok(FileAnalysisOutput::new(
        formatted, semantic, line_count, None,
    ))
}

/// Analyze source code from a string buffer without filesystem access.
///
/// This function analyzes in-memory source code by language identifier. The `language`
/// parameter can be either a language name (e.g., `"rust"`, `"python"`, `"go"`) or a file
/// extension (e.g., `"rs"`, `"py"`).
///
/// Accepted language identifiers depend on compiled features. Use [`supported_languages()`] to
/// discover the available language names at runtime, and [`language_for_extension()`] to resolve
/// a file extension to its supported language identifier.
///
/// # Arguments
///
/// * `source` - The source code to analyze
/// * `language` - The language identifier (language name or extension)
/// * `ast_recursion_limit` - Optional limit for AST traversal depth
///
/// # Returns
///
/// - `Ok(FileAnalysisOutput)` on success
/// - `Err(AnalyzeError::UnsupportedLanguage)` if the language is not recognized
/// - `Err(AnalyzeError::Parser)` if parsing fails
///
/// # Notes
///
/// - Python wildcard import resolution is skipped for in-memory analysis (no filesystem path available)
/// - The formatted output uses the standard file-details formatter, so it includes a `FILE:` header with an empty path
#[inline]
pub fn analyze_str(
    source: &str,
    language: &str,
    ast_recursion_limit: Option<usize>,
) -> Result<FileAnalysisOutput, AnalyzeError> {
    // Resolve language: first try as a file extension, then as a language name
    // (case-insensitive match against supported_languages()).
    let lang = language_for_extension(language).or_else(|| {
        let lower = language.to_ascii_lowercase();
        supported_languages()
            .iter()
            .find(|&&name| name == lower)
            .copied()
    });
    let lang = lang.ok_or_else(|| AnalyzeError::UnsupportedLanguage(language.to_string()))?;

    // Extract semantic information
    let mut semantic = SemanticExtractor::extract(source, lang, ast_recursion_limit, None)?;

    // Populate a stable in-memory sentinel on all reference locations
    for r in &mut semantic.references {
        r.location = "<memory>".to_string();
    }

    // Count lines in the source
    let line_count = source.lines().count();

    // Format output with empty path (no filesystem access)
    let formatted = format_file_details("", &semantic, line_count, false, None);

    Ok(FileAnalysisOutput::new(
        formatted, semantic, line_count, None,
    ))
}

/// Single entry in a call chain (depth-1 direct caller or callee).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct CallChainEntry {
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "Symbol name of the caller or callee")
    )]
    pub symbol: String,
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "File path relative to the repository root")
    )]
    pub file: String,
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "Line number of the definition or call site (1-indexed)",
            schema_with = "crate::schema_helpers::integer_schema"
        )
    )]
    pub line: usize,
}

/// Result of focused symbol analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[non_exhaustive]
pub struct FocusedAnalysisOutput {
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "Formatted text representation of the call graph analysis")
    )]
    pub formatted: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "Opaque cursor token for the next page of results (absent when no more results)"
        )
    )]
    pub next_cursor: Option<String>,
    /// Production caller chains (partitioned from incoming chains, excluding test callers).
    /// Not serialized; used for pagination in lib.rs.
    #[serde(skip)]
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub prod_chains: Vec<InternalCallChain>,
    /// Test caller chains. Not serialized; used for pagination summary in lib.rs.
    #[serde(skip)]
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub test_chains: Vec<InternalCallChain>,
    /// Outgoing (callee) chains. Not serialized; used for pagination in lib.rs.
    #[serde(skip)]
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub outgoing_chains: Vec<InternalCallChain>,
    /// Number of definitions for the symbol. Not serialized; used for pagination headers.
    #[serde(skip)]
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub def_count: usize,
    /// Total unique callers before `impl_only` filter. Not serialized; used for FILTER header.
    #[serde(skip)]
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub unfiltered_caller_count: usize,
    /// Unique callers after `impl_only` filter. Not serialized; used for FILTER header.
    #[serde(skip)]
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub impl_trait_caller_count: usize,
    /// Direct (depth-1) production callers. `follow_depth` does not affect this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callers: Option<Vec<CallChainEntry>>,
    /// Direct (depth-1) test callers. `follow_depth` does not affect this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_callers: Option<Vec<CallChainEntry>>,
    /// Direct (depth-1) callees. `follow_depth` does not affect this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callees: Option<Vec<CallChainEntry>>,
    /// Definition and use sites for the symbol.
    #[serde(default)]
    pub def_use_sites: Vec<crate::types::DefUseSite>,
    /// Cache tier for this result: `"l1_memory"`, `"l2_disk"`, or `"miss"`.
    /// Populated by the MCP handler after cache lookup.
    ///
    /// This field is `None` in the following cases:
    /// - `import_lookup=true` responses: the import-lookup path does not consult the call
    ///   graph cache, so no tier is recorded.
    /// - Non-symbol analysis modes (directory and file tools): `FocusedAnalysisOutput` is
    ///   not produced by those handlers, and the field is therefore absent.
    /// - Any `FocusedAnalysisOutput` constructed outside the `handle_focused_mode` return
    ///   path (e.g. legacy cached entries that pre-date this field).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "Cache tier for this result: l1_memory, l2_disk, or miss")
    )]
    pub cache_tier: Option<String>,
}

/// Parameters for focused symbol analysis. Groups high-arity parameters to keep
/// function signatures under clippy's default 7-argument threshold.
#[derive(Clone)]
pub struct FocusedAnalysisConfig {
    pub focus: String,
    pub match_mode: SymbolMatchMode,
    pub follow_depth: u32,
    pub max_depth: Option<u32>,
    pub ast_recursion_limit: Option<usize>,
    pub use_summary: bool,
    pub impl_only: Option<bool>,
    pub def_use: bool,
    pub parse_timeout_micros: Option<u64>,
}

#[cfg(test)]
pub(crate) use crate::analyze_focused::chains_to_entries;
pub(crate) use crate::analyze_focused::resolve_wildcard_imports;
pub use crate::analyze_focused::{
    analyze_focused, analyze_focused_with_progress, analyze_focused_with_progress_with_entries,
    analyze_import_lookup, analyze_module_file,
};
/// Read a file and return its raw content with line numbers for a specified range.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::formatter::format_focused_paginated;
    use crate::graph::InternalCallChain;
    use crate::pagination::{PaginationMode, decode_cursor, paginate_slice};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn analyze_str_rust_happy_path() {
        let source = "fn hello() -> i32 { 42 }";
        let result = analyze_str(source, "rs", None);
        assert!(result.is_ok());
    }

    #[test]
    fn analyze_str_python_happy_path() {
        let source = "def greet(name):\n    return f'Hello {name}'";
        let result = analyze_str(source, "py", None);
        assert!(result.is_ok());
    }

    #[test]
    fn analyze_str_rust_by_language_name() {
        let source = "fn hello() -> i32 { 42 }";
        let result = analyze_str(source, "rust", None);
        assert!(result.is_ok());
    }

    #[test]
    fn analyze_str_python_by_language_name() {
        let source = "def greet(name):\n    return f'Hello {name}'";
        let result = analyze_str(source, "python", None);
        assert!(result.is_ok());
    }

    #[test]
    fn analyze_str_rust_mixed_case() {
        let source = "fn hello() -> i32 { 42 }";
        let result = analyze_str(source, "RuSt", None);
        assert!(result.is_ok());
    }

    #[test]
    fn analyze_str_python_mixed_case() {
        let source = "def greet(name):\n    return f'Hello {name}'";
        let result = analyze_str(source, "PyThOn", None);
        assert!(result.is_ok());
    }

    #[test]
    fn analyze_str_unsupported_language() {
        let result = analyze_str("code", "brainfuck", None);
        assert!(
            matches!(result, Err(AnalyzeError::UnsupportedLanguage(lang)) if lang == "brainfuck")
        );
    }

    #[test]
    fn test_symbol_focus_callers_pagination_first_page() {
        let temp_dir = TempDir::new().unwrap();

        // Create a file with many callers of `target`
        let mut code = String::from("fn target() {}\n");
        for i in 0..15 {
            code.push_str(&format!("fn caller_{:02}() {{ target(); }}\n", i));
        }
        fs::write(temp_dir.path().join("lib.rs"), &code).unwrap();

        // Act
        let output = analyze_focused(temp_dir.path(), "target", 1, None, None).unwrap();

        // Paginate prod callers with page_size=5
        let paginated = paginate_slice(&output.prod_chains, 0, 5, PaginationMode::Callers)
            .expect("paginate failed");
        assert!(
            paginated.total >= 5,
            "should have enough callers to paginate"
        );
        assert!(
            paginated.next_cursor.is_some(),
            "should have next_cursor for page 1"
        );

        // Verify cursor encodes callers mode
        assert_eq!(paginated.items.len(), 5);
    }

    #[test]
    fn test_symbol_focus_callers_pagination_second_page() {
        let temp_dir = TempDir::new().unwrap();

        let mut code = String::from("fn target() {}\n");
        for i in 0..12 {
            code.push_str(&format!("fn caller_{:02}() {{ target(); }}\n", i));
        }
        fs::write(temp_dir.path().join("lib.rs"), &code).unwrap();

        let output = analyze_focused(temp_dir.path(), "target", 1, None, None).unwrap();
        let total_prod = output.prod_chains.len();

        if total_prod > 5 {
            // Get page 1 cursor
            let p1 = paginate_slice(&output.prod_chains, 0, 5, PaginationMode::Callers)
                .expect("paginate failed");
            assert!(p1.next_cursor.is_some());

            let cursor_str = p1.next_cursor.unwrap();
            let cursor_data = decode_cursor(&cursor_str).expect("decode failed");

            // Get page 2
            let p2 = paginate_slice(
                &output.prod_chains,
                cursor_data.offset,
                5,
                PaginationMode::Callers,
            )
            .expect("paginate failed");

            // Format paginated output
            let formatted = format_focused_paginated(
                &p2.items,
                total_prod,
                PaginationMode::Callers,
                "target",
                &output.prod_chains,
                &output.test_chains,
                &output.outgoing_chains,
                output.def_count,
                cursor_data.offset,
                Some(temp_dir.path()),
                true,
            );

            // Assert: header shows correct range for page 2
            let expected_start = cursor_data.offset + 1;
            assert!(
                formatted.contains(&format!("CALLERS ({}", expected_start)),
                "header should show page 2 range, got: {}",
                formatted
            );
        }
    }

    #[test]
    fn test_chains_to_entries_empty_returns_none() {
        // Arrange
        let chains: Vec<InternalCallChain> = vec![];

        // Act
        let result = chains_to_entries(&chains, None);

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn test_chains_to_entries_with_data_returns_entries() {
        // Arrange
        let chains = vec![
            InternalCallChain {
                chain: vec![("caller1".to_string(), PathBuf::from("/root/lib.rs"), 10)],
            },
            InternalCallChain {
                chain: vec![("caller2".to_string(), PathBuf::from("/root/other.rs"), 20)],
            },
        ];
        let root = PathBuf::from("/root");

        // Act
        let result = chains_to_entries(&chains, Some(root.as_path()));

        // Assert
        assert!(result.is_some());
        let entries = result.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].symbol, "caller1");
        assert_eq!(entries[0].file, "lib.rs");
        assert_eq!(entries[0].line, 10);
        assert_eq!(entries[1].symbol, "caller2");
        assert_eq!(entries[1].file, "other.rs");
        assert_eq!(entries[1].line, 20);
    }

    #[test]
    fn test_symbol_focus_callees_pagination() {
        let temp_dir = TempDir::new().unwrap();

        // target calls many functions
        let mut code = String::from("fn target() {\n");
        for i in 0..10 {
            code.push_str(&format!("    callee_{:02}();\n", i));
        }
        code.push_str("}\n");
        for i in 0..10 {
            code.push_str(&format!("fn callee_{:02}() {{}}\n", i));
        }
        fs::write(temp_dir.path().join("lib.rs"), &code).unwrap();

        let output = analyze_focused(temp_dir.path(), "target", 1, None, None).unwrap();
        let total_callees = output.outgoing_chains.len();

        if total_callees > 3 {
            let paginated = paginate_slice(&output.outgoing_chains, 0, 3, PaginationMode::Callees)
                .expect("paginate failed");

            let formatted = format_focused_paginated(
                &paginated.items,
                total_callees,
                PaginationMode::Callees,
                "target",
                &output.prod_chains,
                &output.test_chains,
                &output.outgoing_chains,
                output.def_count,
                0,
                Some(temp_dir.path()),
                true,
            );

            assert!(
                formatted.contains(&format!(
                    "CALLEES (1-{} of {})",
                    paginated.items.len(),
                    total_callees
                )),
                "header should show callees range, got: {}",
                formatted
            );
        }
    }

    #[test]
    fn test_symbol_focus_empty_prod_callers() {
        let temp_dir = TempDir::new().unwrap();

        // target is only called from test functions
        let code = r#"
fn target() {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_something() { target(); }
}
"#;
        fs::write(temp_dir.path().join("lib.rs"), code).unwrap();

        let output = analyze_focused(temp_dir.path(), "target", 1, None, None).unwrap();

        // prod_chains may be empty; pagination should handle it gracefully
        let paginated = paginate_slice(&output.prod_chains, 0, 100, PaginationMode::Callers)
            .expect("paginate failed");
        assert_eq!(paginated.items.len(), output.prod_chains.len());
        assert!(
            paginated.next_cursor.is_none(),
            "no next_cursor for empty or single-page prod_chains"
        );
    }

    #[test]
    fn test_impl_only_filter_header_correct_counts() {
        let temp_dir = TempDir::new().unwrap();

        // Create a Rust fixture with:
        // - A trait definition
        // - An impl Trait for SomeType block that calls the focus symbol
        // - A regular (non-trait-impl) function that also calls the focus symbol
        let code = r#"
trait MyTrait {
    fn focus_symbol();
}

struct SomeType;

impl MyTrait for SomeType {
    fn focus_symbol() {}
}

fn impl_caller() {
    SomeType::focus_symbol();
}

fn regular_caller() {
    SomeType::focus_symbol();
}
"#;
        fs::write(temp_dir.path().join("lib.rs"), code).unwrap();

        // Call analyze_focused with impl_only=Some(true)
        let params = FocusedAnalysisConfig {
            focus: "focus_symbol".to_string(),
            match_mode: SymbolMatchMode::Insensitive,
            follow_depth: 1,
            max_depth: None,
            ast_recursion_limit: None,
            use_summary: false,
            impl_only: Some(true),
            def_use: false,
            parse_timeout_micros: None,
        };
        let output = analyze_focused_with_progress(
            temp_dir.path(),
            &params,
            Arc::new(AtomicUsize::new(0)),
            CancellationToken::new(),
        )
        .unwrap();

        // Assert the result contains "FILTER: impl_only=true"
        assert!(
            output.formatted.contains("FILTER: impl_only=true"),
            "formatted output should contain FILTER header for impl_only=true, got: {}",
            output.formatted
        );

        // Assert the retained count N < total count M
        assert!(
            output.impl_trait_caller_count < output.unfiltered_caller_count,
            "impl_trait_caller_count ({}) should be less than unfiltered_caller_count ({})",
            output.impl_trait_caller_count,
            output.unfiltered_caller_count
        );

        // Assert format is "FILTER: impl_only=true (N of M callers shown)"
        let filter_line = output
            .formatted
            .lines()
            .find(|line| line.contains("FILTER: impl_only=true"))
            .expect("should find FILTER line");
        assert!(
            filter_line.contains(&format!(
                "({} of {} callers shown)",
                output.impl_trait_caller_count, output.unfiltered_caller_count
            )),
            "FILTER line should show correct N of M counts, got: {}",
            filter_line
        );
    }

    #[test]
    fn test_callers_count_matches_formatted_output() {
        let temp_dir = TempDir::new().unwrap();

        // Create a file with multiple callers of `target`
        let code = r#"
fn target() {}
fn caller_a() { target(); }
fn caller_b() { target(); }
fn caller_c() { target(); }
"#;
        fs::write(temp_dir.path().join("lib.rs"), code).unwrap();

        // Analyze the symbol
        let output = analyze_focused(temp_dir.path(), "target", 1, None, None).unwrap();

        // Extract CALLERS count from formatted output
        let formatted = &output.formatted;
        let callers_count_from_output = formatted
            .lines()
            .find(|line| line.contains("FOCUS:"))
            .and_then(|line| {
                line.split(',')
                    .find(|part| part.contains("callers"))
                    .and_then(|part| {
                        part.trim()
                            .split_whitespace()
                            .next()
                            .and_then(|s| s.parse::<usize>().ok())
                    })
            })
            .expect("should find CALLERS count in formatted output");

        // Compute expected count from prod_chains (unique first-caller names)
        let expected_callers_count = output
            .prod_chains
            .iter()
            .filter_map(|chain| chain.chain.first().map(|(name, _, _)| name))
            .collect::<std::collections::HashSet<_>>()
            .len();

        assert_eq!(
            callers_count_from_output, expected_callers_count,
            "CALLERS count in formatted output should match unique-first-caller count in prod_chains"
        );
    }

    #[test]
    fn test_def_use_focused_analysis() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("lib.rs"),
            "fn example() {\n    let x = 10;\n    x += 1;\n    println!(\"{}\", x);\n    let y = x + 1;\n}\n",
        )
        .unwrap();

        let entries = walk_directory(temp_dir.path(), None).unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let ct = CancellationToken::new();
        let params = FocusedAnalysisConfig {
            focus: "x".to_string(),
            match_mode: SymbolMatchMode::Exact,
            follow_depth: 1,
            max_depth: None,
            ast_recursion_limit: None,
            use_summary: false,
            impl_only: None,
            def_use: true,
            parse_timeout_micros: None,
        };

        let output = analyze_focused_with_progress_with_entries(
            temp_dir.path(),
            &params,
            &counter,
            &ct,
            &entries,
        )
        .expect("def_use analysis should succeed");

        assert!(
            !output.def_use_sites.is_empty(),
            "should find def-use sites for x"
        );
        assert!(
            output
                .def_use_sites
                .iter()
                .any(|s| s.kind == crate::types::DefUseKind::Write),
            "should have at least one Write site",
        );
        // No location appears as both write and read
        let write_locs: std::collections::HashSet<_> = output
            .def_use_sites
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    crate::types::DefUseKind::Write | crate::types::DefUseKind::WriteRead
                )
            })
            .map(|s| (&s.file, s.line, s.column))
            .collect();
        assert!(
            output
                .def_use_sites
                .iter()
                .filter(|s| s.kind == crate::types::DefUseKind::Read)
                .all(|s| !write_locs.contains(&(&s.file, s.line, s.column))),
            "no location should appear as both write and read",
        );
        assert!(
            output.formatted.contains("DEF-USE SITES"),
            "formatted output should contain DEF-USE SITES"
        );
    }

    fn make_temp_file(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        use std::io::Write;
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }
}
