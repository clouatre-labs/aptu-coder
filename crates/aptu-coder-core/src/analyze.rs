// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Main analysis engine for extracting code structure from files and directories.
//!
//! Implements the four MCP tools: `analyze_directory` (Overview), `analyze_file` (`FileDetails`),
//! and `analyze_module` (lightweight index). Handles parallel processing and cancellation.

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
    #[serde(default)]
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "File path of the analyzed file")
    )]
    pub path: String,
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
        path: String,
        formatted: String,
        semantic: SemanticAnalysis,
        line_count: usize,
        next_cursor: Option<String>,
    ) -> Self {
        Self {
            path,
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
    // Enforce the size limit from metadata BEFORE reading, so oversized files
    // are never fully read into memory only to be discarded.
    let size = std::fs::metadata(&entry.path)
        .map_err(|_| SkipReason::Unreadable)?
        .len();
    if size > MAX_FILE_SIZE_BYTES {
        tracing::debug!("skipping large file: {}", entry.path.display());
        return Err(SkipReason::Oversized);
    }
    let bytes = std::fs::read(&entry.path).map_err(|_| SkipReason::Unreadable)?;

    // Skip binary or unreadable files
    String::from_utf8(bytes).map_err(|_| SkipReason::Unreadable)
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

/// Check the file size limit and read the source, mapping errors to `AnalyzeError`.
pub(crate) fn read_source_checked(path: &str) -> Result<String, AnalyzeError> {
    if Path::new(path).metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_SIZE_BYTES {
        tracing::debug!("skipping large file: {}", path);
        return Err(AnalyzeError::Parser(
            crate::parser::ParserError::ParseError("file too large".to_string()),
        ));
    }

    std::fs::read_to_string(path)
        .map_err(|e| AnalyzeError::Parser(crate::parser::ParserError::ParseError(e.to_string())))
}

/// Analyze a single file and return semantic analysis with formatted output.
#[instrument(skip_all, fields(path))]
pub fn analyze_file(
    path: &str,
    ast_recursion_limit: Option<usize>,
) -> Result<FileAnalysisOutput, AnalyzeError> {
    // Enforce the size limit from metadata BEFORE reading, mirroring
    // read_source_checked, so oversized files are never fully read.
    if Path::new(path).metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_SIZE_BYTES {
        tracing::debug!("skipping large file: {}", path);
        return Err(AnalyzeError::Parser(
            crate::parser::ParserError::ParseError("file too large".to_string()),
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|e| AnalyzeError::Parser(crate::parser::ParserError::ParseError(e.to_string())))?;
    analyze_file_from_bytes(path, bytes, ast_recursion_limit)
}

/// Analyze already-read file bytes, avoiding a second read of the file.
///
/// Size checking and UTF-8 validation mirror `read_source_checked` so that
/// behavior is unchanged; the bytes are consumed without an extra copy.
#[instrument(skip(bytes), fields(path))]
pub fn analyze_file_from_bytes(
    path: &str,
    bytes: Vec<u8>,
    ast_recursion_limit: Option<usize>,
) -> Result<FileAnalysisOutput, AnalyzeError> {
    let start = Instant::now();

    if bytes.len() as u64 > MAX_FILE_SIZE_BYTES {
        tracing::debug!("skipping large file: {}", path);
        return Err(AnalyzeError::Parser(
            crate::parser::ParserError::ParseError("file too large".to_string()),
        ));
    }

    let source = String::from_utf8(bytes)
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
        path.to_string(),
        formatted,
        semantic,
        line_count,
        None,
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
        String::new(),
        formatted,
        semantic,
        line_count,
        None,
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
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub prod_chains: Vec<InternalCallChain>,
    /// Test caller chains. Not serialized; used for pagination summary in lib.rs.
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub test_chains: Vec<InternalCallChain>,
    /// Outgoing (callee) chains. Not serialized; used for pagination in lib.rs.
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub outgoing_chains: Vec<InternalCallChain>,
    /// Number of definitions for the symbol. Not serialized; used for pagination headers.
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub def_count: usize,
    /// Total unique callers before `impl_only` filter. Not serialized; used for FILTER header.
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub unfiltered_caller_count: usize,
    /// Unique callers after `impl_only` filter. Not serialized; used for FILTER header.
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
    /// blake3 hash of the stored (pre-cursor) formatted text, computed at cache
    /// put time and reused on L1 hits so hits need no re-hash. Not serialized;
    /// recomputed on L2 promotion.
    #[serde(skip)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub content_hash: Option<String>,
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

pub(crate) use crate::module_lookup::resolve_wildcard_imports;
pub use crate::module_lookup::{analyze_import_lookup, analyze_module_file};
/// Read a file and return its raw content with line numbers for a specified range.
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn focused_output_serialization_omits_content_hash() {
        // Arrange: a focused output with a populated internal-only hash field.
        let output = FocusedAnalysisOutput {
            formatted: "text".to_string(),
            next_cursor: None,
            prod_chains: Vec::new(),
            test_chains: Vec::new(),
            outgoing_chains: Vec::new(),
            def_count: 0,
            unfiltered_caller_count: 0,
            impl_trait_caller_count: 0,
            callers: None,
            test_callers: None,
            callees: None,
            def_use_sites: Vec::new(),
            content_hash: Some("abc".to_string()),
        };

        // Act
        let json = serde_json::to_string(&output).expect("serialize");

        // Assert: the internal-only hash must never leak into serialized output.
        assert!(
            !json.contains("content_hash"),
            "content_hash leaked: {json}"
        );
    }

    #[test]
    fn analyze_file_from_bytes_matches_analyze_file() {
        // Arrange: one temp file; bytes read once and passed to the bytes variant.
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("t.rs");
        fs::write(&path, "fn main() {}\n").expect("write fixture");
        let path_str = path.to_str().expect("utf8 path");

        // Act
        let from_path = analyze_file(path_str, None).expect("analyze_file");
        let bytes = fs::read(path_str).expect("read bytes");
        let from_bytes = analyze_file_from_bytes(path_str, bytes, None).expect("from bytes");

        // Assert: byte-identical formatted output from either entry point.
        assert_eq!(from_path.formatted, from_bytes.formatted);
        assert_eq!(from_path.line_count, from_bytes.line_count);
    }

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
}
