// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Focused analysis: call-graph traversal, import lookup, and wildcard resolution.

use crate::analyze::{
    AnalyzeError, CallChainEntry, FocusedAnalysisConfig, FocusedAnalysisOutput, MAX_FILE_SIZE_BYTES,
};
use crate::formatter::{format_focused_internal, format_focused_summary_internal};
use crate::graph::{CallGraph, InternalCallChain};
use crate::lang::language_for_extension;
use crate::parser::SemanticExtractor;
use crate::test_detection::is_test_file;
use crate::traversal::{WalkEntry, walk_directory};
use crate::types::{ImplTraitInfo, ImportInfo, SemanticAnalysis, SymbolMatchMode};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

/// Internal parameters for focused analysis phases.
#[derive(Clone)]
pub(crate) struct InternalFocusedParams {
    pub(crate) focus: String,
    pub(crate) match_mode: SymbolMatchMode,
    pub(crate) follow_depth: u32,
    pub(crate) ast_recursion_limit: Option<usize>,
    pub(crate) use_summary: bool,
    pub(crate) impl_only: Option<bool>,
    pub(crate) def_use: bool,
    pub(crate) parse_timeout_micros: Option<u64>,
}

/// Type alias for analysis results: (`file_path`, `semantic_analysis`) pairs and impl-trait info.
type FileAnalysisBatch = (Vec<(PathBuf, SemanticAnalysis)>, Vec<ImplTraitInfo>);

/// Phase 1: Collect semantic analysis for all files in parallel.
fn collect_file_analysis(
    entries: &[WalkEntry],
    progress: &Arc<AtomicUsize>,
    ct: &CancellationToken,
    ast_recursion_limit: Option<usize>,
    parse_timeout_micros: Option<u64>,
) -> Result<FileAnalysisBatch, AnalyzeError> {
    // Check if already cancelled
    if ct.is_cancelled() {
        return Err(AnalyzeError::Cancelled);
    }

    // Use pre-walked entries (passed by caller)
    // Collect semantic analysis for all files in parallel
    let file_entries: Vec<&WalkEntry> = entries
        .iter()
        .filter(|e| !e.is_dir && !e.is_symlink)
        .collect();

    // Collect per-file timeout events so they can be surfaced as AnalyzeError::ParseTimeout.
    let timed_out: std::sync::Mutex<Vec<(PathBuf, u64)>> = std::sync::Mutex::new(Vec::new());

    let analysis_results: Vec<(PathBuf, SemanticAnalysis)> = file_entries
        .par_iter()
        .filter_map(|entry| {
            // Check cancellation per file
            if ct.is_cancelled() {
                return None;
            }

            let ext = entry.path.extension().and_then(|e| e.to_str());

            // Check file size before reading
            if entry.path.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_SIZE_BYTES {
                tracing::debug!("skipping large file: {}", entry.path.display());
                progress.fetch_add(1, Ordering::Relaxed);
                return None;
            }

            // Try to read file content
            let Ok(source) = std::fs::read_to_string(&entry.path) else {
                progress.fetch_add(1, Ordering::Relaxed);
                return None;
            };

            // Detect language and extract semantic information
            let language = if let Some(ext_str) = ext {
                language_for_extension(ext_str)
                    .map_or_else(|| "unknown".to_string(), std::string::ToString::to_string)
            } else {
                "unknown".to_string()
            };

            match SemanticExtractor::extract(
                &source,
                &language,
                ast_recursion_limit,
                parse_timeout_micros,
            ) {
                Ok(mut semantic) => {
                    // Populate file path on references
                    for r in &mut semantic.references {
                        r.location = entry.path.display().to_string();
                    }
                    // Populate file path on impl_traits (already extracted during SemanticExtractor::extract)
                    for trait_info in &mut semantic.impl_traits {
                        trait_info.path.clone_from(&entry.path);
                    }
                    progress.fetch_add(1, Ordering::Relaxed);
                    Some((entry.path.clone(), semantic))
                }
                Err(crate::parser::ParserError::Timeout(micros)) => {
                    tracing::warn!(
                        "parse timeout exceeded for {}: {} microseconds",
                        entry.path.display(),
                        micros
                    );
                    if let Ok(mut v) = timed_out.lock() {
                        v.push((entry.path.clone(), micros));
                    }
                    progress.fetch_add(1, Ordering::Relaxed);
                    None
                }
                Err(_) => {
                    progress.fetch_add(1, Ordering::Relaxed);
                    None
                }
            }
        })
        .collect();

    // Check if cancelled after parallel processing
    if ct.is_cancelled() {
        return Err(AnalyzeError::Cancelled);
    }

    // Surface the first timeout as AnalyzeError::ParseTimeout so callers can detect it.
    if let Ok(mut v) = timed_out.lock()
        && let Some((path, micros)) = v.drain(..).next()
    {
        return Err(AnalyzeError::ParseTimeout { path, micros });
    }

    // Collect all impl-trait info from analysis results
    let all_impl_traits: Vec<ImplTraitInfo> = analysis_results
        .iter()
        .flat_map(|(_, sem)| sem.impl_traits.iter().cloned())
        .collect();

    Ok((analysis_results, all_impl_traits))
}

/// Phase 2: Build call graph from analysis results.
fn build_call_graph(
    analysis_results: Vec<(PathBuf, SemanticAnalysis)>,
    all_impl_traits: &[ImplTraitInfo],
) -> Result<CallGraph, AnalyzeError> {
    // Build call graph. Always build without impl_only filter first so we can
    // record the unfiltered caller count before discarding those edges.
    CallGraph::build_from_results(
        analysis_results,
        all_impl_traits,
        false, // filter applied below after counting
    )
    .map_err(std::convert::Into::into)
}

/// Phase 3: Resolve symbol and apply `impl_only` filter.
/// Returns (`resolved_focus`, `unfiltered_caller_count`, `impl_trait_caller_count`).
/// CRITICAL: Must capture `unfiltered_caller_count` BEFORE `retain()`, then apply `retain()`,
/// then compute `impl_trait_caller_count`.
fn resolve_symbol(
    graph: &mut CallGraph,
    params: &InternalFocusedParams,
) -> Result<(String, usize, usize), AnalyzeError> {
    // Resolve symbol name using the requested match mode.
    let resolved_focus = if params.match_mode == SymbolMatchMode::Exact {
        let exists = graph.definitions.contains_key(&params.focus)
            || graph.callers.contains_key(&params.focus)
            || graph.callees.contains_key(&params.focus);
        if exists {
            params.focus.clone()
        } else {
            return Err(crate::graph::GraphError::SymbolNotFound {
                symbol: params.focus.clone(),
                hint: "Try match_mode=insensitive for a case-insensitive search, or match_mode=prefix to list symbols starting with this name.".to_string(),
            }
            .into());
        }
    } else {
        graph.resolve_symbol_indexed(&params.focus, &params.match_mode)?
    };

    // Count unique callers for the focus symbol before applying impl_only filter.
    let unfiltered_caller_count = graph.callers.get(&resolved_focus).map_or(0, |edges| {
        edges
            .iter()
            .map(|e| &e.neighbor_name)
            .collect::<std::collections::HashSet<_>>()
            .len()
    });

    // Apply impl_only filter now if requested, then count filtered callers.
    // Filter all caller adjacency lists so traversal and formatting are consistently
    // restricted to impl-trait edges regardless of follow_depth.
    let impl_trait_caller_count = if params.impl_only.unwrap_or(false) {
        for edges in graph.callers.values_mut() {
            edges.retain(|e| e.is_impl_trait);
        }
        graph.callers.get(&resolved_focus).map_or(0, |edges| {
            edges
                .iter()
                .map(|e| &e.neighbor_name)
                .collect::<std::collections::HashSet<_>>()
                .len()
        })
    } else {
        unfiltered_caller_count
    };

    Ok((
        resolved_focus,
        unfiltered_caller_count,
        impl_trait_caller_count,
    ))
}

/// Type alias for `compute_chains` return type: (`formatted_output`, `prod_chains`, `test_chains`, `outgoing_chains`, `def_count`).
type ChainComputeResult = (
    String,
    Vec<InternalCallChain>,
    Vec<InternalCallChain>,
    Vec<InternalCallChain>,
    usize,
);

/// Helper function to convert InternalCallChain data to CallChainEntry vec.
/// Takes the first (depth-1) element of each chain and converts it to a CallChainEntry.
/// Returns None if chains is empty, otherwise returns a vec of up to 10 entries.
pub(crate) fn chains_to_entries(
    chains: &[InternalCallChain],
    root: Option<&std::path::Path>,
) -> Option<Vec<CallChainEntry>> {
    if chains.is_empty() {
        return None;
    }
    let entries: Vec<CallChainEntry> = chains
        .iter()
        .take(10)
        .filter_map(|chain| {
            let (symbol, path, line) = chain.chain.first()?;
            let file = match root {
                Some(root) => path
                    .strip_prefix(root)
                    .unwrap_or(path.as_path())
                    .to_string_lossy()
                    .into_owned(),
                None => path.to_string_lossy().into_owned(),
            };
            Some(CallChainEntry {
                symbol: symbol.clone(),
                file,
                line: *line,
            })
        })
        .collect();
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

/// Phase 4: Compute chains and format output.
fn compute_chains(
    graph: &CallGraph,
    resolved_focus: &str,
    root: &Path,
    params: &InternalFocusedParams,
    unfiltered_caller_count: usize,
    impl_trait_caller_count: usize,
    def_use_sites: &[crate::types::DefUseSite],
) -> Result<ChainComputeResult, AnalyzeError> {
    // Compute chain data for pagination (always, regardless of summary mode)
    let def_count = graph.definitions.get(resolved_focus).map_or(0, Vec::len);
    let incoming_chains = graph.find_incoming_chains(resolved_focus, params.follow_depth)?;
    let outgoing_chains = graph.find_outgoing_chains(resolved_focus, params.follow_depth)?;

    let (prod_chains, test_chains): (Vec<_>, Vec<_>) =
        incoming_chains.iter().cloned().partition(|chain| {
            chain
                .chain
                .first()
                .is_none_or(|(name, path, _)| !is_test_file(path) && !name.starts_with("test_"))
        });

    // Format output with pre-computed chains
    let mut formatted = if params.use_summary {
        format_focused_summary_internal(
            graph,
            resolved_focus,
            params.follow_depth,
            Some(root),
            Some(&incoming_chains),
            Some(&outgoing_chains),
            def_use_sites,
        )?
    } else {
        format_focused_internal(
            graph,
            resolved_focus,
            params.follow_depth,
            Some(root),
            Some(&incoming_chains),
            Some(&outgoing_chains),
            def_use_sites,
        )?
    };

    // Add FILTER header if impl_only filter was applied
    if params.impl_only.unwrap_or(false) {
        let filter_header = format!(
            "FILTER: impl_only=true ({impl_trait_caller_count} of {unfiltered_caller_count} callers shown)\n",
        );
        formatted = format!("{filter_header}{formatted}");
    }

    Ok((
        formatted,
        prod_chains,
        test_chains,
        outgoing_chains,
        def_count,
    ))
}

/// Analyze a symbol's call graph across a directory with progress tracking.
// public API; callers expect owned semantics
#[allow(clippy::needless_pass_by_value)]
pub fn analyze_focused_with_progress(
    root: &Path,
    params: &FocusedAnalysisConfig,
    progress: Arc<AtomicUsize>,
    ct: CancellationToken,
) -> Result<FocusedAnalysisOutput, AnalyzeError> {
    let entries = walk_directory(root, params.max_depth)?;
    let internal_params = InternalFocusedParams {
        focus: params.focus.clone(),
        match_mode: params.match_mode.clone(),
        follow_depth: params.follow_depth,
        ast_recursion_limit: params.ast_recursion_limit,
        use_summary: params.use_summary,
        impl_only: params.impl_only,
        def_use: params.def_use,
        parse_timeout_micros: params.parse_timeout_micros,
    };
    analyze_focused_with_progress_with_entries_internal(
        root,
        params.max_depth,
        &progress,
        &ct,
        &internal_params,
        &entries,
    )
}

/// Internal implementation of focused analysis using pre-walked entries and params struct.
#[instrument(skip_all, fields(path = %root.display(), symbol = %params.focus))]
fn analyze_focused_with_progress_with_entries_internal(
    root: &Path,
    _max_depth: Option<u32>,
    progress: &Arc<AtomicUsize>,
    ct: &CancellationToken,
    params: &InternalFocusedParams,
    entries: &[WalkEntry],
) -> Result<FocusedAnalysisOutput, AnalyzeError> {
    // Check if already cancelled
    if ct.is_cancelled() {
        return Err(AnalyzeError::Cancelled);
    }

    // Check if path is a file (hint to use directory)
    if root.is_file() {
        let formatted =
            "Single-file focus not supported. Please provide a directory path for cross-file call graph analysis.\n"
                .to_string();
        return Ok(FocusedAnalysisOutput {
            formatted,
            next_cursor: None,
            prod_chains: vec![],
            test_chains: vec![],
            outgoing_chains: vec![],
            def_count: 0,
            unfiltered_caller_count: 0,
            impl_trait_caller_count: 0,
            callers: None,
            test_callers: None,
            callees: None,
            def_use_sites: vec![],
            cache_tier: None,
        });
    }

    // Phase 1: Collect file analysis
    let (analysis_results, all_impl_traits) = collect_file_analysis(
        entries,
        progress,
        ct,
        params.ast_recursion_limit,
        params.parse_timeout_micros,
    )?;

    // Check for cancellation before building the call graph (phase 2)
    if ct.is_cancelled() {
        return Err(AnalyzeError::Cancelled);
    }

    // Phase 2: Build call graph
    let mut graph = build_call_graph(analysis_results, &all_impl_traits)?;

    // Check for cancellation before resolving the symbol (phase 3)
    if ct.is_cancelled() {
        return Err(AnalyzeError::Cancelled);
    }

    // Phase 3: Resolve symbol and apply impl_only filter.
    // When def_use=true and the symbol is not in the call graph (e.g. a variable),
    // fall through to def-use extraction instead of returning SymbolNotFound.
    let resolve_result = resolve_symbol(&mut graph, params);
    if let Err(AnalyzeError::Graph(crate::graph::GraphError::SymbolNotFound { .. })) =
        &resolve_result
    {
        // Deliberately not collapsed: resolve_result must stay alive past this block
        // so that the `?` below can propagate non-SymbolNotFound errors.
        if params.def_use {
            let def_use_sites =
                collect_def_use_sites(entries, &params.focus, params.ast_recursion_limit, root, ct);
            if def_use_sites.is_empty() {
                // Symbol not found anywhere (neither in call graph nor as def/use site).
                // Propagate the original SymbolNotFound error instead of returning an
                // empty success response.
                if let Err(e) = resolve_result {
                    return Err(e);
                }
                unreachable!("resolve_result is Ok only when symbol was found");
            }
            use std::fmt::Write as _;
            let mut formatted = String::new();
            let _ = writeln!(
                formatted,
                "FOCUS: {} (0 defs, 0 callers, 0 callees)",
                params.focus
            );
            {
                let writes = def_use_sites
                    .iter()
                    .filter(|s| {
                        matches!(
                            s.kind,
                            crate::types::DefUseKind::Write | crate::types::DefUseKind::WriteRead
                        )
                    })
                    .count();
                let reads = def_use_sites
                    .iter()
                    .filter(|s| s.kind == crate::types::DefUseKind::Read)
                    .count();
                let _ = writeln!(
                    formatted,
                    "DEF-USE SITES  {}  ({} total: {} writes, {} reads)",
                    params.focus,
                    def_use_sites.len(),
                    writes,
                    reads
                );
            }
            return Ok(FocusedAnalysisOutput {
                formatted,
                next_cursor: None,
                callers: None,
                test_callers: None,
                callees: None,
                prod_chains: vec![],
                test_chains: vec![],
                outgoing_chains: vec![],
                def_count: 0,
                unfiltered_caller_count: 0,
                impl_trait_caller_count: 0,
                def_use_sites,
                cache_tier: None,
            });
        }
    }
    let (resolved_focus, unfiltered_caller_count, impl_trait_caller_count) = resolve_result?;

    // Check for cancellation before computing chains (phase 4)
    if ct.is_cancelled() {
        return Err(AnalyzeError::Cancelled);
    }

    // Phase 5 (optional, before formatting): Def-use site extraction.
    // Use params.focus (the raw user-supplied string) rather than resolved_focus
    // so that variable/field names that are not in the call graph still work.
    let def_use_sites = if params.def_use {
        collect_def_use_sites(entries, &params.focus, params.ast_recursion_limit, root, ct)
    } else {
        Vec::new()
    };

    // Phase 4: Compute chains and format output (includes def_use_sites in one pass)
    let (formatted, prod_chains, test_chains, outgoing_chains, def_count) = compute_chains(
        &graph,
        &resolved_focus,
        root,
        params,
        unfiltered_caller_count,
        impl_trait_caller_count,
        &def_use_sites,
    )?;

    // Compute depth-1 chains for structured output fields (always direct relationships only,
    // regardless of `follow_depth` used for the text-formatted output).
    let (depth1_callers, depth1_test_callers, depth1_callees) = if params.follow_depth <= 1 {
        // Chains already at depth 1; reuse the partitioned vecs.
        let callers = chains_to_entries(&prod_chains, Some(root));
        let test_callers = chains_to_entries(&test_chains, Some(root));
        let callees = chains_to_entries(&outgoing_chains, Some(root));
        (callers, test_callers, callees)
    } else {
        // follow_depth > 1: re-query at depth 1 to get only direct edges.
        let incoming1 = graph
            .find_incoming_chains(&resolved_focus, 1)
            .unwrap_or_default();
        let outgoing1 = graph
            .find_outgoing_chains(&resolved_focus, 1)
            .unwrap_or_default();
        let (prod1, test1): (Vec<_>, Vec<_>) = incoming1.into_iter().partition(|chain| {
            chain
                .chain
                .first()
                .is_none_or(|(name, path, _)| !is_test_file(path) && !name.starts_with("test_"))
        });
        let callers = chains_to_entries(&prod1, Some(root));
        let test_callers = chains_to_entries(&test1, Some(root));
        let callees = chains_to_entries(&outgoing1, Some(root));
        (callers, test_callers, callees)
    };

    Ok(FocusedAnalysisOutput {
        formatted,
        next_cursor: None,
        callers: depth1_callers,
        test_callers: depth1_test_callers,
        callees: depth1_callees,
        prod_chains,
        test_chains,
        outgoing_chains,
        def_count,
        unfiltered_caller_count,
        impl_trait_caller_count,
        def_use_sites,
        cache_tier: None,
    })
}

/// Phase 5: Extract def-use sites for `symbol` across all entries.
/// Writes go before reads; within each kind ordered by file, line, then column.
fn collect_def_use_sites(
    entries: &[WalkEntry],
    symbol: &str,
    ast_recursion_limit: Option<usize>,
    root: &std::path::Path,
    ct: &CancellationToken,
) -> Vec<crate::types::DefUseSite> {
    use crate::parser::SemanticExtractor;

    let file_entries: Vec<&WalkEntry> = entries
        .iter()
        .filter(|e| !e.is_dir && !e.is_symlink)
        .collect();

    let mut sites: Vec<crate::types::DefUseSite> = file_entries
        .par_iter()
        .filter_map(|entry| {
            if ct.is_cancelled() {
                return None;
            }

            // Check file size before reading
            if entry.path.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_SIZE_BYTES {
                tracing::debug!("skipping large file: {}", entry.path.display());
                return None;
            }

            let Ok(source) = std::fs::read_to_string(&entry.path) else {
                return None;
            };
            let ext = entry
                .path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let lang = crate::lang::language_for_extension(ext)?;
            let file_path = entry
                .path
                .strip_prefix(root)
                .unwrap_or(&entry.path)
                .display()
                .to_string();
            let sites = SemanticExtractor::extract_def_use_for_file(
                &source,
                lang,
                symbol,
                &file_path,
                ast_recursion_limit,
            );
            if sites.is_empty() { None } else { Some(sites) }
        })
        .flatten()
        .collect();

    // Writes before reads; within each kind: file, line, then column for deterministic order
    sites.sort_by(|a, b| {
        use crate::types::DefUseKind;
        let kind_ord = |k: &DefUseKind| match k {
            DefUseKind::Write | DefUseKind::WriteRead => 0,
            DefUseKind::Read => 1,
        };
        kind_ord(&a.kind)
            .cmp(&kind_ord(&b.kind))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.column.cmp(&b.column))
    });

    sites
}

/// Analyze a symbol's call graph using pre-walked directory entries.
pub fn analyze_focused_with_progress_with_entries(
    root: &Path,
    params: &FocusedAnalysisConfig,
    progress: &Arc<AtomicUsize>,
    ct: &CancellationToken,
    entries: &[WalkEntry],
) -> Result<FocusedAnalysisOutput, AnalyzeError> {
    let internal_params = InternalFocusedParams {
        focus: params.focus.clone(),
        match_mode: params.match_mode.clone(),
        follow_depth: params.follow_depth,
        ast_recursion_limit: params.ast_recursion_limit,
        use_summary: params.use_summary,
        impl_only: params.impl_only,
        def_use: params.def_use,
        parse_timeout_micros: params.parse_timeout_micros,
    };
    analyze_focused_with_progress_with_entries_internal(
        root,
        params.max_depth,
        progress,
        ct,
        &internal_params,
        entries,
    )
}

#[instrument(skip_all, fields(path = %root.display(), symbol = %focus))]
pub fn analyze_focused(
    root: &Path,
    focus: &str,
    follow_depth: u32,
    max_depth: Option<u32>,
    ast_recursion_limit: Option<usize>,
) -> Result<FocusedAnalysisOutput, AnalyzeError> {
    let entries = walk_directory(root, max_depth)?;
    let counter = Arc::new(AtomicUsize::new(0));
    let ct = CancellationToken::new();
    let params = FocusedAnalysisConfig {
        focus: focus.to_string(),
        match_mode: SymbolMatchMode::Exact,
        follow_depth,
        max_depth,
        ast_recursion_limit,
        use_summary: false,
        impl_only: None,
        def_use: false,
        parse_timeout_micros: None,
    };
    analyze_focused_with_progress_with_entries(root, &params, &counter, &ct, &entries)
}

/// Analyze a single file and return a minimal fixed schema (name, line count, language,
/// functions, imports) for lightweight code understanding.
#[instrument(skip_all, fields(path))]
pub fn analyze_module_file(path: &str) -> Result<crate::types::ModuleInfo, AnalyzeError> {
    // Check file size before reading
    if Path::new(path).metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_SIZE_BYTES {
        tracing::debug!("skipping large file: {}", path);
        return Err(AnalyzeError::Parser(
            crate::parser::ParserError::ParseError("file too large".to_string()),
        ));
    }

    let source = std::fs::read_to_string(path)
        .map_err(|e| AnalyzeError::Parser(crate::parser::ParserError::ParseError(e.to_string())))?;

    let file_path = Path::new(path);
    let name = file_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let line_count = source.lines().count();

    let language = file_path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(language_for_extension)
        .ok_or_else(|| {
            AnalyzeError::Parser(crate::parser::ParserError::UnsupportedLanguage(
                file_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("(no extension)")
                    .to_string(),
            ))
        })?;

    let mut module_info = SemanticExtractor::extract_module_info(&source, language, None)?;
    module_info.name = name;
    module_info.line_count = line_count;

    Ok(module_info)
}

/// Scan a directory for files that import a given module path.
///
/// For each non-directory walk entry, extracts imports via [`SemanticExtractor`] and
/// checks whether `module` matches `ImportInfo.module` or appears in `ImportInfo.items`.
/// Returns a [`FocusedAnalysisOutput`] whose `formatted` field lists matching files.
pub fn analyze_import_lookup(
    root: &Path,
    module: &str,
    entries: &[WalkEntry],
    ast_recursion_limit: Option<usize>,
) -> Result<FocusedAnalysisOutput, AnalyzeError> {
    let matches: Vec<(PathBuf, usize)> = entries
        .par_iter()
        .filter_map(|entry| {
            if entry.is_dir || entry.is_symlink {
                tracing::debug!("skipping symlink: {}", entry.path.display());
                return None;
            }
            let ext = entry
                .path
                .extension()
                .and_then(|e| e.to_str())
                .and_then(crate::lang::language_for_extension)?;
            let source = std::fs::read_to_string(&entry.path).ok()?;
            let semantic =
                SemanticExtractor::extract(&source, ext, ast_recursion_limit, None).ok()?;
            for import in &semantic.imports {
                if import.module == module || import.items.iter().any(|item| item == module) {
                    return Some((entry.path.clone(), import.line));
                }
            }
            None
        })
        .collect();

    let mut text = format!("IMPORT_LOOKUP: {module}\n");
    text.push_str(&format!("ROOT: {}\n", root.display()));
    text.push_str(&format!("MATCHES: {}\n", matches.len()));
    for (path, line) in &matches {
        let rel = path.strip_prefix(root).unwrap_or(path);
        text.push_str(&format!("  {}:{line}\n", rel.display()));
    }

    Ok(FocusedAnalysisOutput {
        formatted: text,
        next_cursor: None,
        prod_chains: vec![],
        test_chains: vec![],
        outgoing_chains: vec![],
        def_count: 0,
        unfiltered_caller_count: 0,
        impl_trait_caller_count: 0,
        callers: None,
        test_callers: None,
        callees: None,
        def_use_sites: vec![],
        cache_tier: None,
    })
}

/// Resolve Python wildcard imports to actual symbol names.
///
/// For each import with items=`["*"]`, this function:
/// 1. Parses the relative dots (if any) and climbs the directory tree
/// 2. Finds the target .py file or __init__.py
/// 3. Extracts symbols (functions and classes) from the target
/// 4. Honors __all__ if defined, otherwise uses function+class names
///
/// All resolution failures are non-fatal: debug-logged and the wildcard is preserved.
pub(crate) fn resolve_wildcard_imports(file_path: &Path, imports: &mut [ImportInfo]) {
    use std::collections::HashMap;

    let mut resolved_cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let Ok(file_path_canonical) = file_path.canonicalize() else {
        tracing::debug!(file = ?file_path, "unable to canonicalize current file path");
        return;
    };

    for import in imports.iter_mut() {
        if import.items != ["*"] {
            continue;
        }
        resolve_single_wildcard(import, file_path, &file_path_canonical, &mut resolved_cache);
    }
}

/// Validate and canonicalize a wildcard target path, checking for self-references.
/// Returns the canonical path if valid, or None if validation fails.
fn validate_wildcard_target(
    target_to_read: &Path,
    file_path_canonical: &Path,
    module: &str,
) -> Option<PathBuf> {
    let Ok(canonical) = target_to_read.canonicalize() else {
        tracing::debug!(target = ?target_to_read, import = %module, "unable to canonicalize path");
        return None;
    };

    if canonical == file_path_canonical {
        tracing::debug!(target = ?canonical, import = %module, "cannot import from self");
        return None;
    }

    Some(canonical)
}

/// Resolve one wildcard import in place. On any failure the import is left unchanged.
fn resolve_single_wildcard(
    import: &mut ImportInfo,
    file_path: &Path,
    file_path_canonical: &Path,
    resolved_cache: &mut std::collections::HashMap<PathBuf, Vec<String>>,
) {
    let module = import.module.clone();
    let dot_count = module.chars().take_while(|c| *c == '.').count();
    if dot_count == 0 {
        return;
    }
    let module_path = module.trim_start_matches('.');

    let Some(target_to_read) = locate_target_file(file_path, dot_count, module_path, &module)
    else {
        return;
    };

    let Some(canonical) = validate_wildcard_target(&target_to_read, file_path_canonical, &module)
    else {
        return;
    };

    if let Some(cached) = resolved_cache.get(&canonical) {
        tracing::debug!(import = %module, symbols_count = cached.len(), "using cached symbols");
        import.items.clone_from(cached);
        return;
    }

    if let Some(symbols) = parse_target_symbols(&target_to_read, &module) {
        tracing::debug!(import = %module, resolved_count = symbols.len(), "wildcard import resolved");
        import.items.clone_from(&symbols);
        resolved_cache.insert(canonical, symbols);
    }
}

/// Locate the .py file that a wildcard import refers to. Returns None if not found.
fn locate_target_file(
    file_path: &Path,
    dot_count: usize,
    module_path: &str,
    module: &str,
) -> Option<PathBuf> {
    let mut target_dir = file_path.parent()?.to_path_buf();

    for _ in 1..dot_count {
        if !target_dir.pop() {
            tracing::debug!(import = %module, "unable to climb {} levels", dot_count.saturating_sub(1));
            return None;
        }
    }

    let target_file = if module_path.is_empty() {
        target_dir.join("__init__.py")
    } else {
        let rel_path = module_path.replace('.', "/");
        target_dir.join(format!("{rel_path}.py"))
    };

    if target_file.exists() {
        Some(target_file)
    } else if target_file.with_extension("").is_dir() {
        let init = target_file.with_extension("").join("__init__.py");
        if init.exists() { Some(init) } else { None }
    } else {
        tracing::debug!(target = ?target_file, import = %module, "target file not found");
        None
    }
}

/// Build a tree-sitter parser for Python and parse the source code.
fn build_parser_for_file(source: &str) -> Option<tree_sitter::Tree> {
    use tree_sitter::Parser;

    let lang_info = crate::languages::get_language_info("python")?;
    let mut parser = Parser::new();
    if parser.set_language(&lang_info.language).is_err() {
        return None;
    }
    parser.parse(source, None)
}

/// Extract all public symbols from a parsed tree (functions and classes).
fn extract_all_symbols(tree: &tree_sitter::Tree, source: &str) -> Vec<String> {
    let mut symbols = Vec::new();
    let root = tree.root_node();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if matches!(child.kind(), "function_definition" | "class_definition")
            && let Some(name_node) = child.child_by_field_name("name")
        {
            let name = source[name_node.start_byte()..name_node.end_byte()].to_string();
            if !name.starts_with('_') {
                symbols.push(name);
            }
        }
    }
    symbols
}

/// Try to resolve symbols from __all__ or fallback to function/class extraction.
fn resolve_symbols_from_tree(tree: &tree_sitter::Tree, source: &str, module: &str) -> Vec<String> {
    let mut symbols = Vec::new();
    extract_all_from_tree(tree, source, &mut symbols);
    if !symbols.is_empty() {
        tracing::debug!(import = %module, symbols = ?symbols, "using __all__ symbols");
        return symbols;
    }

    // Fallback: extract functions/classes from the tree
    let symbols = extract_all_symbols(tree, source);
    tracing::debug!(import = %module, fallback_symbols = ?symbols, "using fallback function/class names");
    symbols
}

/// Read and parse a target .py file, returning its exported symbols.
fn parse_target_symbols(target_path: &Path, module: &str) -> Option<Vec<String>> {
    // Check file size before reading
    if target_path.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_SIZE_BYTES {
        tracing::debug!("skipping large file: {}", target_path.display());
        return None;
    }

    let source = match std::fs::read_to_string(target_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(target = ?target_path, import = %module, error = %e, "unable to read target file");
            return None;
        }
    };

    // Parse once with tree-sitter
    let tree = build_parser_for_file(&source)?;

    // Try to extract __all__ or fallback to function/class extraction
    let symbols = resolve_symbols_from_tree(&tree, &source, module);
    Some(symbols)
}

/// Extract __all__ from a tree-sitter tree.
fn extract_all_from_tree(tree: &tree_sitter::Tree, source: &str, result: &mut Vec<String>) {
    let root = tree.root_node();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "simple_statement" {
            // simple_statement contains assignment and other statement types
            let mut simple_cursor = child.walk();
            for simple_child in child.children(&mut simple_cursor) {
                if simple_child.kind() == "assignment"
                    && let Some(left) = simple_child.child_by_field_name("left")
                {
                    let target_text = source[left.start_byte()..left.end_byte()].trim();
                    if target_text == "__all__"
                        && let Some(right) = simple_child.child_by_field_name("right")
                    {
                        extract_string_list_from_list_node(&right, source, result);
                    }
                }
            }
        } else if child.kind() == "expression_statement" {
            // Fallback for older Python AST structures
            let mut stmt_cursor = child.walk();
            for stmt_child in child.children(&mut stmt_cursor) {
                if stmt_child.kind() == "assignment"
                    && let Some(left) = stmt_child.child_by_field_name("left")
                {
                    let target_text = source[left.start_byte()..left.end_byte()].trim();
                    if target_text == "__all__"
                        && let Some(right) = stmt_child.child_by_field_name("right")
                    {
                        extract_string_list_from_list_node(&right, source, result);
                    }
                }
            }
        }
    }
}

/// Extract string literals from a Python list node.
fn extract_string_list_from_list_node(
    list_node: &tree_sitter::Node,
    source: &str,
    result: &mut Vec<String>,
) {
    let mut cursor = list_node.walk();
    for child in list_node.named_children(&mut cursor) {
        if child.kind() == "string" {
            let raw = source[child.start_byte()..child.end_byte()].trim();
            // Strip quotes: "name" -> name
            let unquoted = raw.trim_matches('"').trim_matches('\'').to_string();
            if !unquoted.is_empty() {
                result.push(unquoted);
            }
        }
    }
}
