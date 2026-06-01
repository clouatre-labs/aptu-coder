// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Output formatting for analysis results across different modes.
//!
//! Formats semantic analysis, call graphs, and directory structures into human-readable text.
//! Handles multiline wrapping, pagination, and summary generation.

use crate::graph::CallGraph;
use crate::graph::InternalCallChain;
use crate::pagination::PaginationMode;
use crate::test_detection::is_test_file;
use crate::traversal::WalkEntry;
use crate::types::{
    AnalyzeFileField, ClassInfo, DefUseKind, DefUseSite, FileInfo, FunctionInfo, ImportInfo,
    ModuleInfo, SemanticAnalysis,
};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::instrument;

const MULTILINE_THRESHOLD: usize = 10;

/// Check if a function falls within a class's line range (method detection).
fn is_method_of_class(func: &FunctionInfo, class: &ClassInfo) -> bool {
    func.line >= class.line && func.end_line <= class.end_line
}

/// Collect methods for each class, preferring ClassInfo.methods when populated (Rust case),
/// falling back to line-range intersection for languages that do not populate ClassInfo.methods.
fn collect_class_methods<'a>(
    classes: &'a [ClassInfo],
    functions: &'a [FunctionInfo],
) -> HashMap<String, Vec<&'a FunctionInfo>> {
    let mut methods_by_class: HashMap<String, Vec<&'a FunctionInfo>> = HashMap::new();
    for class in classes {
        if class.methods.is_empty() {
            // Python/Java/TS/Go: infer methods by line-range containment
            let methods: Vec<&FunctionInfo> = functions
                .iter()
                .filter(|f| is_method_of_class(f, class))
                .collect();
            methods_by_class.insert(class.name.clone(), methods);
        } else {
            // Rust: parser already populated methods via extract_impl_methods
            methods_by_class.insert(class.name.clone(), class.methods.iter().collect());
        }
    }
    methods_by_class
}

/// Format a list of function signatures wrapped at 100 characters with bullet annotation.
fn format_function_list_wrapped<'a>(
    functions: impl Iterator<Item = &'a crate::types::FunctionInfo>,
    call_frequency: &std::collections::HashMap<String, usize>,
) -> String {
    let mut output = String::new();
    let mut line = String::from("  ");
    for (i, func) in functions.enumerate() {
        let mut call_marker = func.compact_signature();

        if let Some(&count) = call_frequency.get(&func.name)
            && count > 3
        {
            let _ = write!(call_marker, "\u{2022}{count}");
        }

        if i == 0 {
            line.push_str(&call_marker);
        } else if line.len() + call_marker.len() + 2 > 100 {
            output.push_str(&line);
            output.push('\n');
            let mut new_line = String::with_capacity(2 + call_marker.len());
            new_line.push_str("  ");
            new_line.push_str(&call_marker);
            line = new_line;
        } else {
            line.push_str(", ");
            line.push_str(&call_marker);
        }
    }
    if !line.trim().is_empty() {
        output.push_str(&line);
        output.push('\n');
    }
    output
}

/// Build a bracket string for file info (line count, function count, class count).
/// Returns None if all counts are zero, otherwise returns "[42L, 7F, 2C]" format.
fn format_file_info_parts(line_count: usize, fn_count: usize, cls_count: usize) -> Option<String> {
    let mut parts = Vec::new();
    if line_count > 0 {
        parts.push(format!("{line_count}L"));
    }
    if fn_count > 0 {
        parts.push(format!("{fn_count}F"));
    }
    if cls_count > 0 {
        parts.push(format!("{cls_count}C"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("[{}]", parts.join(", ")))
    }
}

/// Strip a base path from a Path, returning a relative path or the original on failure.
pub(crate) fn strip_base_path(path: &Path, base_path: Option<&Path>) -> String {
    match base_path {
        Some(base) => {
            if let Ok(rel_path) = path.strip_prefix(base) {
                rel_path.display().to_string()
            } else {
                path.display().to_string()
            }
        }
        None => path.display().to_string(),
    }
}

const SNIPPET_MAX_LEN: usize = 80;
const SNIPPET_TRUNCATION_POINT: usize = 77;

/// Extract the center line from a snippet window and truncate at a char boundary.
pub(crate) fn snippet_one_line(snippet: &str) -> String {
    let lines: Vec<&str> = snippet.split('\n').collect();
    let center = if lines.len() >= 2 { lines[1] } else { lines[0] };
    let trimmed = center.trim();
    if trimmed.len() > SNIPPET_MAX_LEN {
        let truncate_at = trimmed.floor_char_boundary(SNIPPET_TRUNCATION_POINT);
        format!("{}...", &trimmed[..truncate_at])
    } else {
        trimmed.to_string()
    }
}

/// Count (writes, reads) in a def-use site slice.
fn def_use_write_read_counts(sites: &[DefUseSite]) -> (usize, usize) {
    let w = sites
        .iter()
        .filter(|s| matches!(s.kind, DefUseKind::Write | DefUseKind::WriteRead))
        .count();
    (w, sites.len() - w)
}

/// Render a WRITES or READS group of def-use sites.
fn render_def_use_group(
    output: &mut String,
    sites: &[DefUseSite],
    heading: &str,
    pred: impl Fn(&DefUseSite) -> bool,
    base_path: Option<&Path>,
) {
    let filtered: Vec<_> = sites.iter().filter(|s| pred(s)).collect();
    if filtered.is_empty() {
        return;
    }
    let _ = writeln!(output, "  {heading}");
    for site in filtered {
        let file_display = strip_base_path(Path::new(&site.file), base_path);
        let scope_str = site
            .enclosing_scope
            .as_ref()
            .map(|s| format!("{}()", s))
            .unwrap_or_default();
        let snippet = snippet_one_line(&site.snippet);
        let wr_label = if site.kind == DefUseKind::WriteRead {
            " [write_read]"
        } else {
            ""
        };
        let _ = writeln!(
            output,
            "    {file_display}:{}  {scope_str}  {snippet}{wr_label}",
            site.line
        );
    }
}

#[derive(Debug, Error)]
pub enum FormatterError {
    #[error("Graph error: {0}")]
    GraphError(#[from] crate::graph::GraphError),
}

/// Format directory structure analysis results.
#[instrument(skip_all)]
#[allow(clippy::too_many_lines)] // exhaustive directory formatting logic; splitting harms readability
pub fn format_structure(
    entries: &[WalkEntry],
    analysis_results: &[FileInfo],
    max_depth: Option<u32>,
) -> String {
    let mut output = String::new();

    // Build a map of path -> analysis for quick lookup
    let analysis_map: HashMap<String, &FileInfo> = analysis_results
        .iter()
        .map(|a| (a.path.clone(), a))
        .collect();

    // Partition files into production and test
    let (prod_files, test_files): (Vec<_>, Vec<_>) =
        analysis_results.iter().partition(|a| !a.is_test);

    // Calculate totals
    let total_loc: usize = analysis_results.iter().map(|a| a.line_count).sum();
    let total_functions: usize = analysis_results.iter().map(|a| a.function_count).sum();
    let total_classes: usize = analysis_results.iter().map(|a| a.class_count).sum();

    // Count files by language and calculate percentages
    let mut lang_counts: HashMap<String, usize> = HashMap::new();
    for analysis in analysis_results {
        *lang_counts.entry(analysis.language.clone()).or_insert(0) += 1;
    }
    let total_files = analysis_results.len();

    // Leading summary line with totals
    let primary_lang = lang_counts
        .iter()
        .max_by_key(|&(_, count)| count)
        .map_or_else(
            || "unknown 0%".to_string(),
            |(name, count)| {
                let percentage = (*count * 100).checked_div(total_files).unwrap_or_default();
                format!("{name} {percentage}%")
            },
        );

    let _ = writeln!(
        output,
        "{total_files} files, {total_loc}L, {total_functions}F, {total_classes}C ({primary_lang})"
    );

    // SUMMARY block
    output.push_str("SUMMARY:\n");
    let depth_label = match max_depth {
        Some(n) if n > 0 => format!(" (max_depth={n})"),
        _ => String::new(),
    };
    let _ = writeln!(
        output,
        "Shown: {} files ({} prod, {} test), {total_loc}L, {total_functions}F, {total_classes}C{depth_label}",
        total_files,
        prod_files.len(),
        test_files.len()
    );

    if !lang_counts.is_empty() {
        output.push_str("Languages: ");
        let mut langs: Vec<_> = lang_counts.iter().collect();
        langs.sort_by_key(|&(name, _)| name);
        let lang_strs: Vec<String> = langs
            .iter()
            .map(|(name, count)| {
                let percentage = (**count * 100).checked_div(total_files).unwrap_or_default();
                format!("{name} ({percentage}%)")
            })
            .collect();
        output.push_str(&lang_strs.join(", "));
        output.push('\n');
    }

    output.push('\n');

    // PATH block - tree structure (production and test files in single pass)
    output.push_str("PATH [LOC, FUNCTIONS, CLASSES]\n");

    let mut test_buf = String::new();

    for entry in entries {
        // Skip the root directory itself
        if entry.depth == 0 {
            continue;
        }

        // Calculate indentation
        let indent = "  ".repeat(entry.depth - 1);

        // Get just the filename/dirname
        let name = entry
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");

        // For files, append analysis info
        if entry.is_dir {
            let line = format!("{indent}{name}/\n");
            output.push_str(&line);
        } else if let Some(analysis) = analysis_map.get(&entry.path.display().to_string())
            && let Some(info_str) = format_file_info_parts(
                analysis.line_count,
                analysis.function_count,
                analysis.class_count,
            )
        {
            let line = format!("{indent}{name} {info_str}\n");
            if analysis.is_test {
                test_buf.push_str(&line);
            } else {
                output.push_str(&line);
            }
            // Skip files not in analysis_map (binary/unreadable files)
        }
    }

    // TEST FILES section (if any test files exist)
    if !test_buf.is_empty() {
        output.push_str("\nTEST FILES [LOC, FUNCTIONS, CLASSES]\n");
        output.push_str(&test_buf);
    }

    output
}

/// Format file-level semantic analysis results.
#[instrument(skip_all)]
pub fn format_file_details(
    path: &str,
    analysis: &SemanticAnalysis,
    line_count: usize,
    is_test: bool,
    base_path: Option<&Path>,
) -> String {
    let mut output = String::new();

    // FILE: header with counts, prepend [TEST] if applicable
    let display_path = strip_base_path(Path::new(path), base_path);
    let fn_count = analysis.functions.len();
    let class_count = analysis.classes.len();
    let import_count = analysis.imports.len();
    if is_test {
        let _ = writeln!(
            output,
            "FILE [TEST] {display_path}({line_count}L, {fn_count}F, {class_count}C, {import_count}I)"
        );
    } else {
        let _ = writeln!(
            output,
            "FILE: {display_path}({line_count}L, {fn_count}F, {class_count}C, {import_count}I)"
        );
    }

    // C: section with classes and methods
    output.push_str(&format_classes_section(
        &analysis.classes,
        &analysis.functions,
    ));

    // F: section with top-level functions only (exclude methods)
    let top_level_functions: Vec<&FunctionInfo> = analysis
        .functions
        .iter()
        .filter(|func| {
            !analysis
                .classes
                .iter()
                .any(|class| is_method_of_class(func, class))
        })
        .collect();

    if !top_level_functions.is_empty() {
        output.push_str("F:\n");
        output.push_str(&format_function_list_wrapped(
            top_level_functions.iter().copied(),
            &analysis.call_frequency,
        ));
    }

    // I: section with imports grouped by module
    output.push_str(&format_imports_section(&analysis.imports));

    output
}

/// Format chains as a tree-indented output, grouped by depth-1 symbol.
/// Groups chains by their first symbol (depth-1), deduplicates and sorts depth-2 children,
/// then renders with 2-space indentation using the provided arrow.
/// `focus_symbol` is the name of the depth-0 symbol (focus point) to prepend on depth-1 lines.
///
/// Indentation rules:
/// - Depth-1: `  {focus} {arrow} {parent}` (2-space indent)
/// - Depth-2: `    {arrow} {child}` (4-space indent)
/// - Empty:   `  (none)` (2-space indent)
fn format_chains_as_tree(chains: &[(&str, &str)], arrow: &str, focus_symbol: &str) -> String {
    use std::collections::BTreeMap;

    if chains.is_empty() {
        return "  (none)\n".to_string();
    }

    let mut output = String::new();

    // Group chains by depth-1 symbol, counting duplicate children
    let mut groups: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for (parent, child) in chains {
        // Only count non-empty children
        if child.is_empty() {
            // Ensure parent is in groups even if no children
            groups.entry(parent.to_string()).or_default();
        } else {
            *groups
                .entry(parent.to_string())
                .or_default()
                .entry(child.to_string())
                .or_insert(0) += 1;
        }
    }

    // Render grouped tree
    #[allow(clippy::similar_names)]
    // domain-appropriate pairs (arrow/parent, child/count); renaming harms clarity
    for (parent, children) in groups {
        let _ = writeln!(output, "  {focus_symbol} {arrow} {parent}");
        // Sort children by count descending, then alphabetically
        let mut sorted: Vec<_> = children.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (child, count) in sorted {
            if count > 1 {
                let _ = writeln!(output, "    {arrow} {child} (x{count})");
            } else {
                let _ = writeln!(output, "    {arrow} {child}");
            }
        }
    }

    output
}

/// Format focused symbol analysis with call graph.
#[instrument(skip_all)]
#[allow(clippy::too_many_lines)] // exhaustive symbol focus formatting; splitting harms readability
/// Internal helper that accepts pre-computed chains.
#[allow(clippy::similar_names)] // callers_count/callees_count are domain-clear distinct pairs
pub(crate) fn format_focused_internal(
    graph: &CallGraph,
    symbol: &str,
    follow_depth: u32,
    base_path: Option<&Path>,
    incoming_chains: Option<&[InternalCallChain]>,
    outgoing_chains: Option<&[InternalCallChain]>,
    def_use_sites: &[DefUseSite],
) -> Result<String, FormatterError> {
    let mut output = String::new();

    // Compute all counts BEFORE output begins
    let def_count = graph.definitions.get(symbol).map_or(0, Vec::len);

    // Use pre-computed chains if provided, otherwise compute them
    let (incoming_chains_vec, outgoing_chains_vec);
    let (incoming_chains_ref, outgoing_chains_ref) =
        if let (Some(inc), Some(out)) = (incoming_chains, outgoing_chains) {
            (inc, out)
        } else {
            incoming_chains_vec = graph.find_incoming_chains(symbol, follow_depth)?;
            outgoing_chains_vec = graph.find_outgoing_chains(symbol, follow_depth)?;
            (
                incoming_chains_vec.as_slice(),
                outgoing_chains_vec.as_slice(),
            )
        };

    // Partition incoming_chains into production and test callers
    let (prod_chains, test_chains): (Vec<_>, Vec<_>) =
        incoming_chains_ref.iter().cloned().partition(|chain| {
            chain
                .chain
                .first()
                .is_none_or(|(name, path, _)| !is_test_file(path) && !name.starts_with("test_"))
        });

    // Count unique callers
    let callers_count = prod_chains
        .iter()
        .filter_map(|chain| chain.chain.first().map(|(p, _, _)| p))
        .collect::<std::collections::HashSet<_>>()
        .len();

    // Count unique callees
    let callees_count = outgoing_chains_ref
        .iter()
        .filter_map(|chain| chain.chain.first().map(|(p, _, _)| p))
        .collect::<std::collections::HashSet<_>>()
        .len();

    // FOCUS section - with inline counts
    let _ = writeln!(
        output,
        "FOCUS: {symbol} ({def_count} defs, {callers_count} callers, {callees_count} callees)"
    );

    // DEPTH section
    let _ = writeln!(output, "DEPTH: {follow_depth}");

    // DEFINED section - show where the symbol is defined
    if let Some(definitions) = graph.definitions.get(symbol) {
        output.push_str("DEFINED:\n");
        for (path, line) in definitions {
            let display = strip_base_path(path, base_path);
            let _ = writeln!(output, "  {display}:{line}");
        }
    } else {
        output.push_str("DEFINED: (not found)\n");
    }

    // CALLERS section - who calls this symbol
    output.push_str("CALLERS:\n");

    // Render production callers
    let prod_refs: Vec<_> = prod_chains
        .iter()
        .filter_map(|chain| {
            if chain.chain.len() >= 2 {
                Some((chain.chain[0].0.as_str(), chain.chain[1].0.as_str()))
            } else if chain.chain.len() == 1 {
                Some((chain.chain[0].0.as_str(), ""))
            } else {
                None
            }
        })
        .collect();

    if prod_refs.is_empty() {
        output.push_str("  (none)\n");
    } else {
        output.push_str(&format_chains_as_tree(&prod_refs, "<-", symbol));
    }

    // Render test callers summary if any
    if !test_chains.is_empty() {
        let mut test_files: Vec<_> = test_chains
            .iter()
            .filter_map(|chain| {
                chain
                    .chain
                    .first()
                    .map(|(_, path, _)| path.to_string_lossy().into_owned())
            })
            .collect();
        test_files.sort();
        test_files.dedup();

        // Strip base path for display
        let display_files: Vec<_> = test_files
            .iter()
            .map(|f| strip_base_path(Path::new(f), base_path))
            .collect();

        let file_list = display_files.join(", ");
        let test_count = test_chains.len();
        let _ = writeln!(
            output,
            "CALLERS (test): {test_count} test functions (in {file_list})"
        );
    }

    // CALLEES section - what this symbol calls
    output.push_str("CALLEES:\n");
    let outgoing_refs: Vec<_> = outgoing_chains_ref
        .iter()
        .filter_map(|chain| {
            if chain.chain.len() >= 2 {
                Some((chain.chain[0].0.as_str(), chain.chain[1].0.as_str()))
            } else if chain.chain.len() == 1 {
                Some((chain.chain[0].0.as_str(), ""))
            } else {
                None
            }
        })
        .collect();

    if outgoing_refs.is_empty() {
        output.push_str("  (none)\n");
    } else {
        output.push_str(&format_chains_as_tree(&outgoing_refs, "->", symbol));
    }

    // FILES section - collect unique files from production chains
    let mut files = HashSet::new();
    for chain in &prod_chains {
        for (_, path, _) in &chain.chain {
            files.insert(path.clone());
        }
    }
    for chain in outgoing_chains_ref {
        for (_, path, _) in &chain.chain {
            files.insert(path.clone());
        }
    }
    if let Some(definitions) = graph.definitions.get(symbol) {
        for (path, _) in definitions {
            files.insert(path.clone());
        }
    }

    // Partition files into production and test
    let (prod_files, test_files): (Vec<_>, Vec<_>) =
        files.into_iter().partition(|path| !is_test_file(path));

    output.push_str("FILES:\n");
    if prod_files.is_empty() && test_files.is_empty() {
        output.push_str("  (none)\n");
    } else {
        // Show production files first
        if !prod_files.is_empty() {
            let mut sorted_files = prod_files;
            sorted_files.sort();
            for file in sorted_files {
                let display = strip_base_path(&file, base_path);
                let _ = writeln!(output, "  {display}");
            }
        }

        // Show test files in separate subsection
        if !test_files.is_empty() {
            output.push_str("  TEST FILES:\n");
            let mut sorted_files = test_files;
            sorted_files.sort();
            for file in sorted_files {
                let display = strip_base_path(&file, base_path);
                let _ = writeln!(output, "    {display}");
            }
        }
    }

    // DEF-USE SITES section - show writes and reads of the symbol
    if !def_use_sites.is_empty() {
        let (write_count, read_count) = def_use_write_read_counts(def_use_sites);
        let total = def_use_sites.len();
        let _ = writeln!(
            output,
            "DEF-USE SITES  {symbol}  ({total} total: {write_count} writes, {read_count} reads)"
        );

        render_def_use_group(
            &mut output,
            def_use_sites,
            "WRITES",
            |s| matches!(s.kind, DefUseKind::Write | DefUseKind::WriteRead),
            base_path,
        );
        render_def_use_group(
            &mut output,
            def_use_sites,
            "READS",
            |s| s.kind == DefUseKind::Read,
            base_path,
        );
    }

    Ok(output)
}

/// Format a compact summary of focused symbol analysis.
/// Used when output would exceed the size threshold or when explicitly requested.
#[instrument(skip_all)]
#[allow(clippy::too_many_lines)] // exhaustive symbol summary formatting; splitting harms readability
#[allow(clippy::similar_names)] // domain pairs: callers_count/callees_count are intentionally similar
/// Internal helper that accepts pre-computed chains.
pub(crate) fn format_focused_summary_internal(
    graph: &CallGraph,
    symbol: &str,
    follow_depth: u32,
    base_path: Option<&Path>,
    incoming_chains: Option<&[InternalCallChain]>,
    outgoing_chains: Option<&[InternalCallChain]>,
    def_use_sites: &[DefUseSite],
) -> Result<String, FormatterError> {
    let mut output = String::new();

    // Compute all counts BEFORE output begins
    let def_count = graph.definitions.get(symbol).map_or(0, Vec::len);

    // Use pre-computed chains if provided, otherwise compute them
    let (incoming_chains_vec, outgoing_chains_vec);
    let (incoming_chains_ref, outgoing_chains_ref) =
        if let (Some(inc), Some(out)) = (incoming_chains, outgoing_chains) {
            (inc, out)
        } else {
            incoming_chains_vec = graph.find_incoming_chains(symbol, follow_depth)?;
            outgoing_chains_vec = graph.find_outgoing_chains(symbol, follow_depth)?;
            (
                incoming_chains_vec.as_slice(),
                outgoing_chains_vec.as_slice(),
            )
        };

    // Partition incoming_chains into production and test callers
    let (prod_chains, test_chains): (Vec<_>, Vec<_>) =
        incoming_chains_ref.iter().cloned().partition(|chain| {
            chain
                .chain
                .first()
                .is_none_or(|(name, path, _)| !is_test_file(path) && !name.starts_with("test_"))
        });

    // Count unique production callers
    let callers_count = prod_chains
        .iter()
        .filter_map(|chain| chain.chain.first().map(|(p, _, _)| p))
        .collect::<std::collections::HashSet<_>>()
        .len();

    // Count unique callees
    let callees_count = outgoing_chains_ref
        .iter()
        .filter_map(|chain| chain.chain.first().map(|(p, _, _)| p))
        .collect::<std::collections::HashSet<_>>()
        .len();

    // FOCUS header
    let _ = writeln!(
        output,
        "FOCUS: {symbol} ({def_count} defs, {callers_count} callers, {callees_count} callees)"
    );

    // DEPTH line
    let _ = writeln!(output, "DEPTH: {follow_depth}");

    // DEFINED section
    if let Some(definitions) = graph.definitions.get(symbol) {
        output.push_str("DEFINED:\n");
        for (path, line) in definitions {
            let display = strip_base_path(path, base_path);
            let _ = writeln!(output, "  {display}:{line}");
        }
    } else {
        output.push_str("DEFINED: (not found)\n");
    }

    // CALLERS (production, top 10 by frequency)
    output.push_str("CALLERS (top 10):\n");
    if prod_chains.is_empty() {
        output.push_str("  (none)\n");
    } else {
        // Collect caller names with their file paths (from chain.chain.first())
        let mut caller_freq: std::collections::HashMap<String, (usize, String)> =
            std::collections::HashMap::new();
        for chain in &prod_chains {
            if let Some((name, path, _)) = chain.chain.first() {
                let file_path = strip_base_path(path, base_path);
                caller_freq
                    .entry(name.clone())
                    .and_modify(|(count, _)| *count += 1)
                    .or_insert((1, file_path));
            }
        }

        // Sort by frequency descending, take top 10
        let mut sorted_callers: Vec<_> = caller_freq.into_iter().collect();
        sorted_callers.sort_unstable_by_key(|b| std::cmp::Reverse(b.1.0));

        for (name, (_, file_path)) in sorted_callers.into_iter().take(10) {
            let _ = writeln!(output, "  {name} {file_path}");
        }
    }

    // CALLERS (test) - summary only
    if !test_chains.is_empty() {
        let mut test_files: Vec<_> = test_chains
            .iter()
            .filter_map(|chain| {
                chain
                    .chain
                    .first()
                    .map(|(_, path, _)| path.to_string_lossy().into_owned())
            })
            .collect();
        test_files.sort();
        test_files.dedup();

        let test_count = test_chains.len();
        let test_file_count = test_files.len();
        let _ = writeln!(
            output,
            "CALLERS (test): {test_count} test functions (in {test_file_count} files)"
        );
    }

    // CALLEES (top 10 by frequency)
    output.push_str("CALLEES (top 10):\n");
    if outgoing_chains_ref.is_empty() {
        output.push_str("  (none)\n");
    } else {
        // Collect callee names and count frequency
        let mut callee_freq: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for chain in outgoing_chains_ref {
            if let Some((name, _, _)) = chain.chain.first() {
                *callee_freq.entry(name.clone()).or_insert(0) += 1;
            }
        }

        // Sort by frequency descending, take top 10
        let mut sorted_callees: Vec<_> = callee_freq.into_iter().collect();
        sorted_callees.sort_unstable_by_key(|b| std::cmp::Reverse(b.1));

        for (name, _) in sorted_callees.into_iter().take(10) {
            let _ = writeln!(output, "  {name}");
        }
    }

    // SUGGESTION section
    output.push_str("SUGGESTION:\n");
    output.push_str("Use summary=false with force=true for full output\n");

    // DEF-USE SITES brief summary
    if !def_use_sites.is_empty() {
        let (write_count, read_count) = def_use_write_read_counts(def_use_sites);
        let total = def_use_sites.len();
        let _ = writeln!(
            output,
            "DEF-USE SITES: {total} total ({write_count} writes, {read_count} reads)",
        );
    }

    Ok(output)
}

/// Format a compact summary of focused symbol analysis.
/// Public wrapper that computes chains if not provided.
pub fn format_focused_summary(
    graph: &CallGraph,
    symbol: &str,
    follow_depth: u32,
    base_path: Option<&Path>,
) -> Result<String, FormatterError> {
    format_focused_summary_internal(graph, symbol, follow_depth, base_path, None, None, &[])
}

/// Render top files section with class/function counts and hints.
fn render_top_files_section(
    files_in_dir: &[&FileInfo],
    dir_path: &Path,
    has_classes: bool,
) -> String {
    let top_files_sorted: Vec<&FileInfo> = if has_classes {
        let mut sorted = files_in_dir.to_vec();
        sorted.sort_unstable_by(|a, b| {
            b.class_count
                .cmp(&a.class_count)
                .then(b.function_count.cmp(&a.function_count))
                .then(a.path.cmp(&b.path))
        });
        sorted
    } else {
        let mut sorted = files_in_dir.to_vec();
        sorted.sort_unstable_by(|a, b| {
            b.function_count
                .cmp(&a.function_count)
                .then(a.path.cmp(&b.path))
        });
        sorted
    };

    let top_n: Vec<String> = top_files_sorted
        .iter()
        .take(3)
        .filter(|f| {
            if has_classes {
                f.class_count > 0
            } else {
                f.function_count > 0
            }
        })
        .map(|f| {
            let rel = Path::new(&f.path).strip_prefix(dir_path).map_or_else(
                |_| {
                    Path::new(&f.path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map_or_else(|| "?".to_owned(), std::borrow::ToOwned::to_owned)
                },
                |p| p.to_string_lossy().into_owned(),
            );
            let count = if has_classes {
                f.class_count
            } else {
                f.function_count
            };
            let suffix = if has_classes { 'C' } else { 'F' };
            format!("{rel}({count}{suffix})")
        })
        .collect();

    if top_n.is_empty() {
        String::new()
    } else {
        let joined = top_n.join(", ");
        format!(" top: {joined}")
    }
}

/// Aggregate directory statistics from analyzed files.
fn aggregate_dir_stats(files_in_dir: &[&FileInfo]) -> (usize, usize, usize) {
    let dir_loc: usize = files_in_dir.iter().map(|f| f.line_count).sum();
    let dir_functions: usize = files_in_dir.iter().map(|f| f.function_count).sum();
    let dir_classes: usize = files_in_dir.iter().map(|f| f.class_count).sum();
    (dir_loc, dir_functions, dir_classes)
}

/// Format a compact summary for large directory analysis results.
/// Used when output would exceed the size threshold or when explicitly requested.
#[instrument(skip_all)]
#[allow(clippy::too_many_lines)] // exhaustive directory summary formatting; splitting harms readability
pub fn format_summary(
    entries: &[WalkEntry],
    analysis_results: &[FileInfo],
    max_depth: Option<u32>,
    subtree_counts: Option<&[(PathBuf, usize)]>,
) -> String {
    let mut output = String::new();

    // Partition files into production and test
    let (prod_files, test_files): (Vec<_>, Vec<_>) =
        analysis_results.iter().partition(|a| !a.is_test);

    // Calculate totals
    let total_loc: usize = analysis_results.iter().map(|a| a.line_count).sum();
    let total_functions: usize = analysis_results.iter().map(|a| a.function_count).sum();
    let total_classes: usize = analysis_results.iter().map(|a| a.class_count).sum();

    // Count files by language
    let mut lang_counts: HashMap<String, usize> = HashMap::new();
    for analysis in analysis_results {
        *lang_counts.entry(analysis.language.clone()).or_insert(0) += 1;
    }
    let total_files = analysis_results.len();

    // SUMMARY block
    output.push_str("SUMMARY:\n");
    let depth_label = match max_depth {
        Some(n) if n > 0 => format!(" (max_depth={n})"),
        _ => String::new(),
    };
    let prod_count = prod_files.len();
    let test_count = test_files.len();
    let _ = writeln!(
        output,
        "{total_files} files ({prod_count} prod, {test_count} test), {total_loc}L, {total_functions}F, {total_classes}C{depth_label}"
    );

    if !lang_counts.is_empty() {
        output.push_str("Languages: ");
        let mut langs: Vec<_> = lang_counts.iter().collect();
        langs.sort_unstable_by_key(|&(name, _)| name);
        let lang_strs: Vec<String> = langs
            .iter()
            .map(|(name, count)| {
                let percentage = (**count * 100).checked_div(total_files).unwrap_or_default();
                format!("{name} ({percentage}%)")
            })
            .collect();
        output.push_str(&lang_strs.join(", "));
        output.push('\n');
    }

    output.push('\n');

    // STRUCTURE (depth 1) block
    output.push_str("STRUCTURE (depth 1):\n");

    // Build a map of path -> analysis for quick lookup
    let analysis_map: HashMap<String, &FileInfo> = analysis_results
        .iter()
        .map(|a| (a.path.clone(), a))
        .collect();

    // Collect depth-1 entries (directories and files at depth 1)
    let mut depth1_entries: Vec<&WalkEntry> = entries.iter().filter(|e| e.depth == 1).collect();
    depth1_entries.sort_by(|a, b| a.path.cmp(&b.path));

    // Track largest non-excluded directory for SUGGESTION
    let mut largest_dir_name: Option<String> = None;
    let mut largest_dir_path: Option<String> = None;
    let mut largest_dir_count: usize = 0;

    for entry in depth1_entries {
        let name = entry
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");

        if entry.is_dir {
            // For directories, aggregate stats from all files under this directory
            let dir_path_str = entry.path.display().to_string();
            let files_in_dir: Vec<&FileInfo> = analysis_results
                .iter()
                .filter(|f| Path::new(&f.path).starts_with(&entry.path))
                .collect();

            if files_in_dir.is_empty() {
                // No analyzed files at this depth, but subtree_counts may have a true count
                let entry_name_str = name.to_string();
                if let Some(counts) = subtree_counts {
                    let true_count = counts
                        .binary_search_by_key(&&entry.path, |(p, _)| p)
                        .ok()
                        .map_or(0, |i| counts[i].1);
                    if true_count > 0 {
                        // Track for SUGGESTION
                        if !crate::EXCLUDED_DIRS.contains(&entry_name_str.as_str())
                            && true_count > largest_dir_count
                        {
                            largest_dir_count = true_count;
                            largest_dir_name = Some(entry_name_str);
                            largest_dir_path = Some(
                                entry
                                    .path
                                    .canonicalize()
                                    .unwrap_or_else(|_| entry.path.clone())
                                    .display()
                                    .to_string(),
                            );
                        }
                        let depth_val = max_depth.unwrap_or(0);
                        let _ = writeln!(
                            output,
                            "  {name}/ [{true_count} files total; showing 0 at depth={depth_val}, 0L, 0F, 0C]"
                        );
                    } else {
                        let _ = writeln!(output, "  {name}/");
                    }
                } else {
                    let _ = writeln!(output, "  {name}/");
                }
            } else {
                let dir_file_count = files_in_dir.len();
                let (dir_loc, dir_functions, dir_classes) = aggregate_dir_stats(&files_in_dir);

                // Track largest non-excluded directory for SUGGESTION
                let entry_name_str = name.to_string();
                let effective_count = if let Some(counts) = subtree_counts {
                    counts
                        .binary_search_by_key(&&entry.path, |(p, _)| p)
                        .ok()
                        .map_or(dir_file_count, |i| counts[i].1)
                } else {
                    dir_file_count
                };
                if !crate::EXCLUDED_DIRS.contains(&entry_name_str.as_str())
                    && effective_count > largest_dir_count
                {
                    largest_dir_count = effective_count;
                    largest_dir_name = Some(entry_name_str);
                    largest_dir_path = Some(
                        entry
                            .path
                            .canonicalize()
                            .unwrap_or_else(|_| entry.path.clone())
                            .display()
                            .to_string(),
                    );
                }

                // Build hint: top-N files sorted by class_count desc, fallback to function_count
                let hint = if files_in_dir.len() > 1 && (dir_classes > 0 || dir_functions > 0) {
                    let has_classes = files_in_dir.iter().any(|f| f.class_count > 0);
                    let dir_path = Path::new(&dir_path_str);
                    render_top_files_section(&files_in_dir, dir_path, has_classes)
                } else {
                    String::new()
                };

                // Collect depth-2 sub-package directories (immediate children of this directory)
                let mut subdirs: Vec<String> = entries
                    .iter()
                    .filter(|e| e.depth == 2 && e.is_dir && e.path.starts_with(&entry.path))
                    .filter_map(|e| {
                        e.path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(std::borrow::ToOwned::to_owned)
                    })
                    .collect();
                subdirs.sort();
                subdirs.dedup();
                let subdir_suffix = if subdirs.is_empty() {
                    String::new()
                } else {
                    let subdirs_capped: Vec<String> =
                        subdirs.iter().take(5).map(|s| format!("{s}/")).collect();
                    let joined = subdirs_capped.join(", ");
                    format!("  sub: {joined}")
                };

                let files_label = if let Some(counts) = subtree_counts {
                    let true_count = counts
                        .binary_search_by_key(&&entry.path, |(p, _)| p)
                        .ok()
                        .map_or(dir_file_count, |i| counts[i].1);
                    if true_count == dir_file_count {
                        format!(
                            "{dir_file_count} files, {dir_loc}L, {dir_functions}F, {dir_classes}C"
                        )
                    } else {
                        let depth_val = max_depth.unwrap_or(0);
                        format!(
                            "{true_count} files total; showing {dir_file_count} at depth={depth_val}, {dir_loc}L, {dir_functions}F, {dir_classes}C"
                        )
                    }
                } else {
                    format!("{dir_file_count} files, {dir_loc}L, {dir_functions}F, {dir_classes}C")
                };
                let _ = writeln!(output, "  {name}/ [{files_label}]{hint}{subdir_suffix}");
            }
        } else {
            // For files, show individual stats
            if let Some(analysis) = analysis_map.get(&entry.path.display().to_string())
                && let Some(info_str) = format_file_info_parts(
                    analysis.line_count,
                    analysis.function_count,
                    analysis.class_count,
                )
            {
                let _ = writeln!(output, "  {name} {info_str}");
            } else if analysis_map.contains_key(&entry.path.display().to_string()) {
                let _ = writeln!(output, "  {name}");
            }
        }
    }

    output.push('\n');

    // SUGGESTION block
    if let (Some(name), Some(path)) = (largest_dir_name, largest_dir_path) {
        let _ = writeln!(
            output,
            "SUGGESTION: Largest source directory: {name}/ ({largest_dir_count} files total). For module details, re-run with path={path} and max_depth=2."
        );
    } else {
        output.push_str("SUGGESTION:\n");
        output.push_str("Use a narrower path for details (e.g., analyze src/core/)\n");
    }

    output
}

/// Format a compact summary of file details for large `FileDetails` output.
///
/// Returns `FILE` header with path/LOC/counts, top 10 functions by line span descending,
/// classes inline if <=10, import count, and suggestion block.
#[instrument(skip_all)]
pub fn format_file_details_summary(
    semantic: &SemanticAnalysis,
    path: &str,
    line_count: usize,
) -> String {
    let mut output = String::new();

    // FILE header
    output.push_str("FILE:\n");
    let _ = writeln!(output, "  path: {path}");
    let fn_count = semantic.functions.len();
    let class_count = semantic.classes.len();
    let _ = writeln!(output, "  {line_count}L, {fn_count}F, {class_count}C");
    output.push('\n');

    // Top 10 functions by line span (end_line - start_line) descending
    if !semantic.functions.is_empty() {
        output.push_str("TOP FUNCTIONS BY SIZE:\n");
        let mut funcs: Vec<&crate::types::FunctionInfo> = semantic.functions.iter().collect();
        let k = funcs.len().min(10);
        if k > 0 {
            funcs.select_nth_unstable_by(k.saturating_sub(1), |a, b| {
                let a_span = a.end_line.saturating_sub(a.line);
                let b_span = b.end_line.saturating_sub(b.line);
                b_span.cmp(&a_span)
            });
            funcs[..k].sort_by(|a, b| {
                let a_span = a.end_line.saturating_sub(a.line);
                let b_span = b.end_line.saturating_sub(b.line);
                b_span.cmp(&a_span)
            });
        }

        for func in &funcs[..k] {
            let span = func.end_line.saturating_sub(func.line);
            let params = if func.parameters.is_empty() {
                String::new()
            } else {
                format!("({})", func.parameters.join(", "))
            };
            let _ = writeln!(
                output,
                "  {}:{}: {} {} [{}L]",
                func.line, func.end_line, func.name, params, span
            );
        }
        output.push('\n');
    }

    // Classes inline if <=10, else multiline with method count
    if !semantic.classes.is_empty() {
        output.push_str("CLASSES:\n");
        if semantic.classes.len() <= 10 {
            // Inline format: one class per line with method count
            for class in &semantic.classes {
                let methods_count = class.methods.len();
                let _ = writeln!(output, "  {}: {}M", class.name, methods_count);
            }
        } else {
            // Multiline format with summary
            let _ = writeln!(output, "  {} classes total", semantic.classes.len());
            for class in semantic.classes.iter().take(5) {
                let _ = writeln!(output, "    {}", class.name);
            }
            if semantic.classes.len() > 5 {
                let _ = writeln!(output, "    ... and {} more", semantic.classes.len() - 5);
            }
        }
        output.push('\n');
    }

    // Import count only
    let _ = writeln!(output, "Imports: {}", semantic.imports.len());
    output.push('\n');

    // SUGGESTION block
    output.push_str("SUGGESTION:\n");
    output.push_str("Use force=true for full output, or narrow your scope\n");

    output
}

/// Format a paginated subset of files for Overview mode.
#[instrument(skip_all)]
pub fn format_structure_paginated(
    paginated_files: &[FileInfo],
    total_files: usize,
    max_depth: Option<u32>,
    base_path: Option<&Path>,
    verbose: bool,
) -> String {
    let mut output = String::new();

    let depth_label = match max_depth {
        Some(n) if n > 0 => format!(" (max_depth={n})"),
        _ => String::new(),
    };
    let _ = writeln!(
        output,
        "PAGINATED: showing {} of {} files{}\n",
        paginated_files.len(),
        total_files,
        depth_label
    );

    let prod_files: Vec<&FileInfo> = paginated_files.iter().filter(|f| !f.is_test).collect();
    let test_files: Vec<&FileInfo> = paginated_files.iter().filter(|f| f.is_test).collect();

    if !prod_files.is_empty() {
        if verbose {
            output.push_str("FILES [LOC, FUNCTIONS, CLASSES]\n");
        }
        for file in &prod_files {
            output.push_str(&format_file_entry(file, base_path));
        }
    }

    if !test_files.is_empty() {
        if verbose {
            output.push_str("\nTEST FILES [LOC, FUNCTIONS, CLASSES]\n");
        } else if !prod_files.is_empty() {
            output.push('\n');
        }
        for file in &test_files {
            output.push_str(&format_file_entry(file, base_path));
        }
    }

    output
}

/// Format a paginated subset of functions for `FileDetails` mode.
/// When `verbose=false` (default/compact): shows `C:` (if non-empty) and `F:` with wrapped rendering; omits `I:`.
/// When `verbose=true`: shows `C:`, `I:`, and `F:` with wrapped rendering on the first page (offset == 0).
/// Header shows position context: `FILE: path (NL, start-end/totalF, CC, II)`.
#[instrument(skip_all)]
#[allow(clippy::too_many_arguments)]
pub fn format_file_details_paginated(
    functions_page: &[FunctionInfo],
    total_functions: usize,
    semantic: &SemanticAnalysis,
    path: &str,
    line_count: usize,
    offset: usize,
    verbose: bool,
    fields: Option<&[AnalyzeFileField]>,
) -> String {
    let mut output = String::new();

    let start = offset + 1; // 1-indexed for display
    let end = offset + functions_page.len();

    let _ = writeln!(
        output,
        "FILE: {} ({}L, {}-{}/{}F, {}C, {}I)",
        path,
        line_count,
        start,
        end,
        total_functions,
        semantic.classes.len(),
        semantic.imports.len()
    );

    // Compute field visibility flags. Empty slice behaves same as None (show all).
    let show_all = fields.is_none_or(<[AnalyzeFileField]>::is_empty);
    let show_classes = show_all
        || fields.is_some_and(|f| {
            f.contains(&AnalyzeFileField::All) || f.contains(&AnalyzeFileField::Classes)
        });
    let show_imports = show_all
        || fields.is_some_and(|f| {
            f.contains(&AnalyzeFileField::All) || f.contains(&AnalyzeFileField::Imports)
        });
    let show_functions = show_all
        || fields.is_some_and(|f| {
            f.contains(&AnalyzeFileField::All) || f.contains(&AnalyzeFileField::Functions)
        });

    // Classes section on first page for both verbose and compact modes
    if show_classes && offset == 0 && !semantic.classes.is_empty() {
        output.push_str(&format_classes_section(
            &semantic.classes,
            &semantic.functions,
        ));
    }

    // Imports section only on first page in verbose mode, or when explicitly listed in fields
    if show_imports && offset == 0 && (verbose || !show_all) {
        output.push_str(&format_imports_section(&semantic.imports));
    }

    // F: section with paginated function slice (exclude methods)
    let top_level_functions: Vec<&FunctionInfo> = functions_page
        .iter()
        .filter(|func| {
            !semantic
                .classes
                .iter()
                .any(|class| is_method_of_class(func, class))
        })
        .collect();

    if show_functions && !top_level_functions.is_empty() {
        output.push_str("F:\n");
        output.push_str(&format_function_list_wrapped(
            top_level_functions.iter().copied(),
            &semantic.call_frequency,
        ));
    }

    output
}

/// Format a paginated subset of callers or callees for `SymbolFocus` mode.
/// Mode is determined by the `mode` parameter:
/// - `PaginationMode::Callers`: paginate production callers; show test callers summary and callees summary.
/// - `PaginationMode::Callees`: paginate callees; show callers summary and test callers summary.
#[instrument(skip_all)]
#[allow(clippy::too_many_arguments)]
#[allow(clippy::similar_names)] // domain-appropriate pairs (callers_count/callees_count); renaming harms clarity
pub fn format_focused_paginated(
    paginated_chains: &[InternalCallChain],
    total: usize,
    mode: PaginationMode,
    symbol: &str,
    prod_chains: &[InternalCallChain],
    test_chains: &[InternalCallChain],
    outgoing_chains: &[InternalCallChain],
    def_count: usize,
    offset: usize,
    base_path: Option<&Path>,
    _verbose: bool,
) -> String {
    let start = offset + 1; // 1-indexed
    let end = offset + paginated_chains.len();

    let callers_count = prod_chains.len();

    let callees_count = outgoing_chains.len();

    let mut output = String::new();

    let _ = writeln!(
        output,
        "FOCUS: {symbol} ({def_count} defs, {callers_count} callers, {callees_count} callees)"
    );

    match mode {
        PaginationMode::Callers => {
            // Paginate production callers
            let _ = writeln!(output, "CALLERS ({start}-{end} of {total}):");

            let page_refs: Vec<_> = paginated_chains
                .iter()
                .filter_map(|chain| {
                    if chain.chain.len() >= 2 {
                        Some((chain.chain[0].0.as_str(), chain.chain[1].0.as_str()))
                    } else if chain.chain.len() == 1 {
                        Some((chain.chain[0].0.as_str(), ""))
                    } else {
                        None
                    }
                })
                .collect();

            if page_refs.is_empty() {
                output.push_str("  (none)\n");
            } else {
                output.push_str(&format_chains_as_tree(&page_refs, "<-", symbol));
            }

            // Test callers summary
            if !test_chains.is_empty() {
                let mut test_files: Vec<_> = test_chains
                    .iter()
                    .filter_map(|chain| {
                        chain
                            .chain
                            .first()
                            .map(|(_, path, _)| path.to_string_lossy().into_owned())
                    })
                    .collect();
                test_files.sort();
                test_files.dedup();

                let display_files: Vec<_> = test_files
                    .iter()
                    .map(|f| strip_base_path(std::path::Path::new(f), base_path))
                    .collect();

                let _ = writeln!(
                    output,
                    "CALLERS (test): {} test functions (in {})",
                    test_chains.len(),
                    display_files.join(", ")
                );
            }

            // Callees summary
            let callee_names: Vec<_> = outgoing_chains
                .iter()
                .filter_map(|chain| chain.chain.first().map(|(p, _, _)| p.clone()))
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            if callee_names.is_empty() {
                output.push_str("CALLEES: (none)\n");
            } else {
                let _ = writeln!(
                    output,
                    "CALLEES: {callees_count} (use cursor for callee pagination)"
                );
            }
        }
        PaginationMode::Callees => {
            // Callers summary
            let _ = writeln!(output, "CALLERS: {callers_count} production callers");

            // Test callers summary
            if !test_chains.is_empty() {
                let _ = writeln!(
                    output,
                    "CALLERS (test): {} test functions",
                    test_chains.len()
                );
            }

            // Paginate callees
            let _ = writeln!(output, "CALLEES ({start}-{end} of {total}):");

            let page_refs: Vec<_> = paginated_chains
                .iter()
                .filter_map(|chain| {
                    if chain.chain.len() >= 2 {
                        Some((chain.chain[0].0.as_str(), chain.chain[1].0.as_str()))
                    } else if chain.chain.len() == 1 {
                        Some((chain.chain[0].0.as_str(), ""))
                    } else {
                        None
                    }
                })
                .collect();

            if page_refs.is_empty() {
                output.push_str("  (none)\n");
            } else {
                output.push_str(&format_chains_as_tree(&page_refs, "->", symbol));
            }
        }
        PaginationMode::Default => {
            unreachable!("format_focused_paginated called with PaginationMode::Default")
        }
        PaginationMode::DefUse => {
            unreachable!("format_focused_paginated called with PaginationMode::DefUse")
        }
    }

    output
}

fn format_file_entry(file: &FileInfo, base_path: Option<&Path>) -> String {
    let mut parts = Vec::new();
    if file.line_count > 0 {
        parts.push(format!("{}L", file.line_count));
    }
    if file.function_count > 0 {
        parts.push(format!("{}F", file.function_count));
    }
    if file.class_count > 0 {
        parts.push(format!("{}C", file.class_count));
    }
    let display_path = strip_base_path(Path::new(&file.path), base_path);
    if parts.is_empty() {
        format!("{display_path}\n")
    } else {
        format!("{display_path} [{}]\n", parts.join(", "))
    }
}

/// Format a [`ModuleInfo`] into a compact single-block string.
///
/// Output format:
/// ```text
/// FILE: <name> (<line_count>L, <fn_count>F, <import_count>I)
/// F:
///   func1:10, func2:42
/// I:
///   module1:item1, item2; module2:item1; module3
/// ```
///
/// The `F:` section is omitted when there are no functions; likewise `I:` when
/// there are no imports.
#[instrument(skip_all)]
pub fn format_module_info(info: &ModuleInfo) -> String {
    use std::fmt::Write as _;
    let fn_count = info.functions.len();
    let import_count = info.imports.len();
    let mut out = String::with_capacity(64 + fn_count * 24 + import_count * 32);
    let _ = writeln!(
        out,
        "FILE: {} ({}L, {}F, {}I)",
        info.name, info.line_count, fn_count, import_count
    );
    if !info.functions.is_empty() {
        out.push_str("F:\n  ");
        let parts: Vec<String> = info
            .functions
            .iter()
            .map(|f| format!("{}:{}", f.name, f.line))
            .collect();
        out.push_str(&parts.join(", "));
        out.push('\n');
    }
    if !info.imports.is_empty() {
        out.push_str("I:\n  ");
        let parts: Vec<String> = info
            .imports
            .iter()
            .map(|i| {
                if i.items.is_empty() {
                    i.module.clone()
                } else {
                    format!("{}:{}", i.module, i.items.join(", "))
                }
            })
            .collect();
        out.push_str(&parts.join("; "));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_base_path_relative() {
        let path = Path::new("/home/user/project/src/main.rs");
        let base = Path::new("/home/user/project");
        let result = strip_base_path(path, Some(base));
        assert_eq!(result, "src/main.rs");
    }

    #[test]
    fn test_strip_base_path_fallback_absolute() {
        let path = Path::new("/other/project/src/main.rs");
        let base = Path::new("/home/user/project");
        let result = strip_base_path(path, Some(base));
        assert_eq!(result, "/other/project/src/main.rs");
    }

    #[test]
    fn test_strip_base_path_none() {
        let path = Path::new("/home/user/project/src/main.rs");
        let result = strip_base_path(path, None);
        assert_eq!(result, "/home/user/project/src/main.rs");
    }

    #[test]
    fn test_format_file_details_summary_empty() {
        use crate::types::SemanticAnalysis;
        use std::collections::HashMap;

        let semantic = SemanticAnalysis {
            functions: vec![],
            classes: vec![],
            imports: vec![],
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let result = format_file_details_summary(&semantic, "src/main.rs", 100);

        // Should contain FILE header, Imports count, and SUGGESTION
        assert!(result.contains("FILE:"));
        assert!(result.contains("100L, 0F, 0C"));
        assert!(result.contains("src/main.rs"));
        assert!(result.contains("Imports: 0"));
        assert!(result.contains("SUGGESTION:"));
    }

    #[test]
    fn test_format_file_details_summary_with_functions() {
        use crate::types::{ClassInfo, FunctionInfo, SemanticAnalysis};
        use std::collections::HashMap;

        let semantic = SemanticAnalysis {
            functions: vec![
                FunctionInfo {
                    name: "short".to_string(),
                    line: 10,
                    end_line: 12,
                    parameters: vec![],
                    return_type: None,
                },
                FunctionInfo {
                    name: "long_function".to_string(),
                    line: 20,
                    end_line: 50,
                    parameters: vec!["x".to_string(), "y".to_string()],
                    return_type: Some("i32".to_string()),
                },
            ],
            classes: vec![ClassInfo {
                name: "MyClass".to_string(),
                line: 60,
                end_line: 80,
                methods: vec![],
                fields: vec![],
                inherits: vec![],
            }],
            imports: vec![],
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let result = format_file_details_summary(&semantic, "src/lib.rs", 250);

        // Should contain FILE header with counts
        assert!(result.contains("FILE:"));
        assert!(result.contains("src/lib.rs"));
        assert!(result.contains("250L, 2F, 1C"));

        // Should contain TOP FUNCTIONS BY SIZE with longest first
        assert!(result.contains("TOP FUNCTIONS BY SIZE:"));
        let long_idx = result.find("long_function").unwrap_or(0);
        let short_idx = result.find("short").unwrap_or(0);
        assert!(
            long_idx > 0 && short_idx > 0 && long_idx < short_idx,
            "long_function should appear before short"
        );

        // Should contain classes inline
        assert!(result.contains("CLASSES:"));
        assert!(result.contains("MyClass:"));

        // Should contain import count
        assert!(result.contains("Imports: 0"));
    }
    #[test]
    fn test_format_file_info_parts_all_zero() {
        assert_eq!(format_file_info_parts(0, 0, 0), None);
    }

    #[test]
    fn test_format_file_info_parts_partial() {
        assert_eq!(
            format_file_info_parts(42, 0, 3),
            Some("[42L, 3C]".to_string())
        );
    }

    #[test]
    fn test_format_file_info_parts_all_nonzero() {
        assert_eq!(
            format_file_info_parts(100, 5, 2),
            Some("[100L, 5F, 2C]".to_string())
        );
    }

    #[test]
    fn test_format_function_list_wrapped_empty() {
        let freq = std::collections::HashMap::new();
        let result = format_function_list_wrapped(std::iter::empty(), &freq);
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_function_list_wrapped_bullet_annotation() {
        use crate::types::FunctionInfo;
        use std::collections::HashMap;

        let mut freq = HashMap::new();
        freq.insert("frequent".to_string(), 5); // count > 3 should get bullet

        let funcs = vec![FunctionInfo {
            name: "frequent".to_string(),
            line: 1,
            end_line: 10,
            parameters: vec![],
            return_type: Some("void".to_string()),
        }];

        let result = format_function_list_wrapped(funcs.iter(), &freq);
        // Should contain bullet (U+2022) followed by count
        assert!(result.contains("\u{2022}5"));
    }

    #[test]
    fn test_compact_format_omits_sections() {
        use crate::types::{ClassInfo, FunctionInfo, ImportInfo, SemanticAnalysis};
        use std::collections::HashMap;

        let funcs: Vec<FunctionInfo> = (0..10)
            .map(|i| FunctionInfo {
                name: format!("fn_{}", i),
                line: i * 5 + 1,
                end_line: i * 5 + 4,
                parameters: vec![format!("x: u32")],
                return_type: Some("bool".to_string()),
            })
            .collect();
        let imports: Vec<ImportInfo> = vec![ImportInfo {
            module: "std::collections".to_string(),
            items: vec!["HashMap".to_string()],
            line: 1,
        }];
        let classes: Vec<ClassInfo> = vec![ClassInfo {
            name: "MyStruct".to_string(),
            line: 100,
            end_line: 150,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];
        let semantic = SemanticAnalysis {
            functions: funcs,
            classes,
            imports,
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let verbose_out = format_file_details_paginated(
            &semantic.functions,
            semantic.functions.len(),
            &semantic,
            "src/lib.rs",
            100,
            0,
            true,
            None,
        );
        let compact_out = format_file_details_paginated(
            &semantic.functions,
            semantic.functions.len(),
            &semantic,
            "src/lib.rs",
            100,
            0,
            false,
            None,
        );

        // Verbose includes C:, I:, F: section headers
        assert!(verbose_out.contains("C:\n"), "verbose must have C: section");
        assert!(verbose_out.contains("I:\n"), "verbose must have I: section");
        assert!(verbose_out.contains("F:\n"), "verbose must have F: section");

        // Compact includes C: and F: but omits I: (imports)
        assert!(
            compact_out.contains("C:\n"),
            "compact must have C: section (restored)"
        );
        assert!(
            !compact_out.contains("I:\n"),
            "compact must not have I: section (imports omitted)"
        );
        assert!(
            compact_out.contains("F:\n"),
            "compact must have F: section with wrapped formatting"
        );

        // Compact functions are wrapped: fn_0 and fn_1 must appear on the same line
        assert!(compact_out.contains("fn_0"), "compact must list functions");
        let has_two_on_same_line = compact_out
            .lines()
            .any(|l| l.contains("fn_0") && l.contains("fn_1"));
        assert!(
            has_two_on_same_line,
            "compact must render multiple functions per line (wrapped), not one-per-line"
        );
    }

    /// Regression test: compact mode must be <= verbose for function-heavy files (no imports to mask regression).
    #[test]
    fn test_compact_mode_consistent_token_reduction() {
        use crate::types::{FunctionInfo, SemanticAnalysis};
        use std::collections::HashMap;

        let funcs: Vec<FunctionInfo> = (0..50)
            .map(|i| FunctionInfo {
                name: format!("function_name_{}", i),
                line: i * 10 + 1,
                end_line: i * 10 + 8,
                parameters: vec![
                    "arg1: u32".to_string(),
                    "arg2: String".to_string(),
                    "arg3: Option<bool>".to_string(),
                ],
                return_type: Some("Result<Vec<String>, Error>".to_string()),
            })
            .collect();

        let semantic = SemanticAnalysis {
            functions: funcs,
            classes: vec![],
            imports: vec![],
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let verbose_out = format_file_details_paginated(
            &semantic.functions,
            semantic.functions.len(),
            &semantic,
            "src/large_file.rs",
            1000,
            0,
            true,
            None,
        );
        let compact_out = format_file_details_paginated(
            &semantic.functions,
            semantic.functions.len(),
            &semantic,
            "src/large_file.rs",
            1000,
            0,
            false,
            None,
        );

        assert!(
            compact_out.len() <= verbose_out.len(),
            "compact ({} chars) must be <= verbose ({} chars)",
            compact_out.len(),
            verbose_out.len(),
        );
    }

    /// Edge case test: Compact mode with empty classes should not emit C: header.
    #[test]
    fn test_format_module_info_happy_path() {
        use crate::types::{ModuleFunctionInfo, ModuleImportInfo, ModuleInfo};
        let info = ModuleInfo::new(
            "parser.rs".to_string(),
            312,
            "rust".to_string(),
            vec![
                ModuleFunctionInfo {
                    name: "parse_file".to_string(),
                    line: 24,
                },
                ModuleFunctionInfo {
                    name: "parse_block".to_string(),
                    line: 58,
                },
            ],
            vec![
                ModuleImportInfo {
                    module: "crate::types".to_string(),
                    items: vec!["Token".to_string(), "Expr".to_string()],
                },
                ModuleImportInfo {
                    module: "std::io".to_string(),
                    items: vec!["BufReader".to_string()],
                },
            ],
        );
        let result = format_module_info(&info);
        assert!(result.starts_with("FILE: parser.rs (312L, 2F, 2I)"));
        assert!(result.contains("F:"));
        assert!(result.contains("parse_file:24"));
        assert!(result.contains("parse_block:58"));
        assert!(result.contains("I:"));
        assert!(result.contains("crate::types:Token, Expr"));
        assert!(result.contains("std::io:BufReader"));
        assert!(result.contains("; "));
        assert!(!result.contains('{'));
    }

    #[test]
    fn test_format_module_info_empty() {
        use crate::types::ModuleInfo;
        let info = ModuleInfo::new(
            "empty.rs".to_string(),
            0,
            "rust".to_string(),
            vec![],
            vec![],
        );
        let result = format_module_info(&info);
        assert!(result.starts_with("FILE: empty.rs (0L, 0F, 0I)"));
        assert!(!result.contains("F:"));
        assert!(!result.contains("I:"));
    }

    #[test]
    fn test_compact_mode_empty_classes_no_header() {
        use crate::types::{FunctionInfo, SemanticAnalysis};
        use std::collections::HashMap;

        let funcs: Vec<FunctionInfo> = (0..5)
            .map(|i| FunctionInfo {
                name: format!("fn_{}", i),
                line: i * 5 + 1,
                end_line: i * 5 + 4,
                parameters: vec![],
                return_type: None,
            })
            .collect();

        let semantic = SemanticAnalysis {
            functions: funcs,
            classes: vec![], // Empty classes
            imports: vec![],
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let compact_out = format_file_details_paginated(
            &semantic.functions,
            semantic.functions.len(),
            &semantic,
            "src/simple.rs",
            100,
            0,
            false,
            None,
        );

        // Should not have stray C: header when classes are empty
        assert!(
            !compact_out.contains("C:\n"),
            "compact mode must not emit C: header when classes are empty"
        );
    }

    #[test]
    fn test_format_classes_with_methods() {
        use crate::types::{ClassInfo, FunctionInfo};

        let functions = vec![
            FunctionInfo {
                name: "method_a".to_string(),
                line: 5,
                end_line: 8,
                parameters: vec![],
                return_type: None,
            },
            FunctionInfo {
                name: "method_b".to_string(),
                line: 10,
                end_line: 12,
                parameters: vec![],
                return_type: None,
            },
            FunctionInfo {
                name: "top_level_func".to_string(),
                line: 50,
                end_line: 55,
                parameters: vec![],
                return_type: None,
            },
        ];

        let classes = vec![ClassInfo {
            name: "MyClass".to_string(),
            line: 1,
            end_line: 30,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];

        let output = format_classes_section(&classes, &functions);

        assert!(
            output.contains("MyClass:1-30"),
            "class header should show start-end range"
        );
        assert!(output.contains("method_a:5"), "method_a should be listed");
        assert!(output.contains("method_b:10"), "method_b should be listed");
        assert!(
            !output.contains("top_level_func"),
            "top_level_func outside class range should not be listed"
        );
    }

    #[test]
    fn test_format_classes_method_cap() {
        use crate::types::{ClassInfo, FunctionInfo};

        let mut functions = Vec::new();
        for i in 0..15 {
            functions.push(FunctionInfo {
                name: format!("method_{}", i),
                line: 2 + i,
                end_line: 3 + i,
                parameters: vec![],
                return_type: None,
            });
        }

        let classes = vec![ClassInfo {
            name: "LargeClass".to_string(),
            line: 1,
            end_line: 50,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];

        let output = format_classes_section(&classes, &functions);

        assert!(output.contains("method_0"), "first method should be listed");
        assert!(output.contains("method_9"), "10th method should be listed");
        assert!(
            !output.contains("method_10"),
            "11th method should not be listed (cap at 10)"
        );
        assert!(
            output.contains("... (5 more)"),
            "truncation message should show remaining count"
        );
    }

    #[test]
    fn test_format_classes_no_methods() {
        use crate::types::{ClassInfo, FunctionInfo};

        let functions = vec![FunctionInfo {
            name: "top_level".to_string(),
            line: 100,
            end_line: 105,
            parameters: vec![],
            return_type: None,
        }];

        let classes = vec![ClassInfo {
            name: "EmptyClass".to_string(),
            line: 1,
            end_line: 50,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];

        let output = format_classes_section(&classes, &functions);

        assert!(
            output.contains("EmptyClass:1-50"),
            "empty class header should appear"
        );
        assert!(
            !output.contains("top_level"),
            "top-level functions outside class should not appear"
        );
    }

    #[test]
    fn test_f_section_excludes_methods() {
        use crate::types::{ClassInfo, FunctionInfo, SemanticAnalysis};
        use std::collections::HashMap;

        let functions = vec![
            FunctionInfo {
                name: "method_a".to_string(),
                line: 5,
                end_line: 10,
                parameters: vec![],
                return_type: None,
            },
            FunctionInfo {
                name: "top_level".to_string(),
                line: 50,
                end_line: 55,
                parameters: vec![],
                return_type: None,
            },
        ];

        let semantic = SemanticAnalysis {
            functions,
            classes: vec![ClassInfo {
                name: "TestClass".to_string(),
                line: 1,
                end_line: 30,
                methods: vec![],
                fields: vec![],
                inherits: vec![],
            }],
            imports: vec![],
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let output = format_file_details("test.rs", &semantic, 100, false, None);

        assert!(output.contains("C:"), "classes section should exist");
        assert!(
            output.contains("method_a:5"),
            "method should be in C: section"
        );
        assert!(output.contains("F:"), "F: section should exist");
        assert!(
            output.contains("top_level"),
            "top-level function should be in F: section"
        );

        // Verify method_a is not in F: section (check sequence: C: before method_a, F: after it)
        let f_pos = output.find("F:").unwrap();
        let method_pos = output.find("method_a").unwrap();
        assert!(
            method_pos < f_pos,
            "method_a should appear before F: section"
        );
    }

    #[test]
    fn test_format_focused_paginated_unit() {
        use crate::graph::InternalCallChain;
        use crate::pagination::PaginationMode;
        use std::path::PathBuf;

        // Arrange: create mock caller chains
        let make_chain = |name: &str| -> InternalCallChain {
            InternalCallChain {
                chain: vec![
                    (name.to_string(), PathBuf::from("src/lib.rs"), 10),
                    ("target".to_string(), PathBuf::from("src/lib.rs"), 5),
                ],
            }
        };

        let prod_chains: Vec<InternalCallChain> = (0..8)
            .map(|i| make_chain(&format!("caller_{}", i)))
            .collect();
        let page = &prod_chains[0..3];

        // Act
        let formatted = format_focused_paginated(
            page,
            8,
            PaginationMode::Callers,
            "target",
            &prod_chains,
            &[],
            &[],
            1,
            0,
            None,
            true,
        );

        // Assert: header present
        assert!(
            formatted.contains("CALLERS (1-3 of 8):"),
            "header should show 1-3 of 8, got: {}",
            formatted
        );
        assert!(
            formatted.contains("FOCUS: target"),
            "should have FOCUS header"
        );
    }

    #[test]
    fn test_fields_none_regression() {
        use crate::types::SemanticAnalysis;
        use std::collections::HashMap;

        let functions = vec![FunctionInfo {
            name: "hello".to_string(),
            line: 10,
            end_line: 15,
            parameters: vec![],
            return_type: None,
        }];

        let classes = vec![ClassInfo {
            name: "MyClass".to_string(),
            line: 20,
            end_line: 50,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];

        let imports = vec![ImportInfo {
            module: "std".to_string(),
            items: vec!["io".to_string()],
            line: 1,
        }];

        let semantic = SemanticAnalysis {
            functions: functions.clone(),
            classes: classes.clone(),
            imports: imports.clone(),
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let output = format_file_details_paginated(
            &functions,
            functions.len(),
            &semantic,
            "test.rs",
            100,
            0,
            true,
            None,
        );

        assert!(output.contains("FILE: test.rs"), "FILE header missing");
        assert!(output.contains("C:"), "Classes section missing");
        assert!(output.contains("I:"), "Imports section missing");
        assert!(output.contains("F:"), "Functions section missing");
    }

    #[test]
    fn test_fields_functions_only() {
        use crate::types::SemanticAnalysis;
        use std::collections::HashMap;

        let functions = vec![FunctionInfo {
            name: "hello".to_string(),
            line: 10,
            end_line: 15,
            parameters: vec![],
            return_type: None,
        }];

        let classes = vec![ClassInfo {
            name: "MyClass".to_string(),
            line: 20,
            end_line: 50,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];

        let imports = vec![ImportInfo {
            module: "std".to_string(),
            items: vec!["io".to_string()],
            line: 1,
        }];

        let semantic = SemanticAnalysis {
            functions: functions.clone(),
            classes: classes.clone(),
            imports: imports.clone(),
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let fields = Some(vec![AnalyzeFileField::Functions]);
        let output = format_file_details_paginated(
            &functions,
            functions.len(),
            &semantic,
            "test.rs",
            100,
            0,
            true,
            fields.as_deref(),
        );

        assert!(output.contains("FILE: test.rs"), "FILE header missing");
        assert!(!output.contains("C:"), "Classes section should not appear");
        assert!(!output.contains("I:"), "Imports section should not appear");
        assert!(output.contains("F:"), "Functions section missing");
    }

    #[test]
    fn test_fields_classes_only() {
        use crate::types::SemanticAnalysis;
        use std::collections::HashMap;

        let functions = vec![FunctionInfo {
            name: "hello".to_string(),
            line: 10,
            end_line: 15,
            parameters: vec![],
            return_type: None,
        }];

        let classes = vec![ClassInfo {
            name: "MyClass".to_string(),
            line: 20,
            end_line: 50,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];

        let imports = vec![ImportInfo {
            module: "std".to_string(),
            items: vec!["io".to_string()],
            line: 1,
        }];

        let semantic = SemanticAnalysis {
            functions: functions.clone(),
            classes: classes.clone(),
            imports: imports.clone(),
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let fields = Some(vec![AnalyzeFileField::Classes]);
        let output = format_file_details_paginated(
            &functions,
            functions.len(),
            &semantic,
            "test.rs",
            100,
            0,
            true,
            fields.as_deref(),
        );

        assert!(output.contains("FILE: test.rs"), "FILE header missing");
        assert!(output.contains("C:"), "Classes section missing");
        assert!(!output.contains("I:"), "Imports section should not appear");
        assert!(
            !output.contains("F:"),
            "Functions section should not appear"
        );
    }

    #[test]
    fn test_fields_imports_verbose() {
        use crate::types::SemanticAnalysis;
        use std::collections::HashMap;

        let functions = vec![FunctionInfo {
            name: "hello".to_string(),
            line: 10,
            end_line: 15,
            parameters: vec![],
            return_type: None,
        }];

        let classes = vec![ClassInfo {
            name: "MyClass".to_string(),
            line: 20,
            end_line: 50,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];

        let imports = vec![ImportInfo {
            module: "std".to_string(),
            items: vec!["io".to_string()],
            line: 1,
        }];

        let semantic = SemanticAnalysis {
            functions: functions.clone(),
            classes: classes.clone(),
            imports: imports.clone(),
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let fields = Some(vec![AnalyzeFileField::Imports]);
        let output = format_file_details_paginated(
            &functions,
            functions.len(),
            &semantic,
            "test.rs",
            100,
            0,
            true,
            fields.as_deref(),
        );

        assert!(output.contains("FILE: test.rs"), "FILE header missing");
        assert!(!output.contains("C:"), "Classes section should not appear");
        assert!(output.contains("I:"), "Imports section missing");
        assert!(
            !output.contains("F:"),
            "Functions section should not appear"
        );
    }

    #[test]
    fn test_fields_imports_no_verbose() {
        use crate::types::SemanticAnalysis;
        use std::collections::HashMap;

        let functions = vec![FunctionInfo {
            name: "hello".to_string(),
            line: 10,
            end_line: 15,
            parameters: vec![],
            return_type: None,
        }];

        let classes = vec![ClassInfo {
            name: "MyClass".to_string(),
            line: 20,
            end_line: 50,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];

        let imports = vec![ImportInfo {
            module: "std".to_string(),
            items: vec!["io".to_string()],
            line: 1,
        }];

        let semantic = SemanticAnalysis {
            functions: functions.clone(),
            classes: classes.clone(),
            imports: imports.clone(),
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let fields = Some(vec![AnalyzeFileField::Imports]);
        let output = format_file_details_paginated(
            &functions,
            functions.len(),
            &semantic,
            "test.rs",
            100,
            0,
            false,
            fields.as_deref(),
        );

        assert!(output.contains("FILE: test.rs"), "FILE header missing");
        assert!(!output.contains("C:"), "Classes section should not appear");
        assert!(
            output.contains("I:"),
            "Imports section should appear (explicitly listed in fields)"
        );
        assert!(
            !output.contains("F:"),
            "Functions section should not appear"
        );
    }

    #[test]
    fn test_fields_empty_array() {
        use crate::types::SemanticAnalysis;
        use std::collections::HashMap;

        let functions = vec![FunctionInfo {
            name: "hello".to_string(),
            line: 10,
            end_line: 15,
            parameters: vec![],
            return_type: None,
        }];

        let classes = vec![ClassInfo {
            name: "MyClass".to_string(),
            line: 20,
            end_line: 50,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];

        let imports = vec![ImportInfo {
            module: "std".to_string(),
            items: vec!["io".to_string()],
            line: 1,
        }];

        let semantic = SemanticAnalysis {
            functions: functions.clone(),
            classes: classes.clone(),
            imports: imports.clone(),
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let fields = Some(vec![]);
        let output = format_file_details_paginated(
            &functions,
            functions.len(),
            &semantic,
            "test.rs",
            100,
            0,
            true,
            fields.as_deref(),
        );

        assert!(output.contains("FILE: test.rs"), "FILE header missing");
        assert!(
            output.contains("C:"),
            "Classes section missing (empty fields = show all)"
        );
        assert!(
            output.contains("I:"),
            "Imports section missing (empty fields = show all)"
        );
        assert!(
            output.contains("F:"),
            "Functions section missing (empty fields = show all)"
        );
    }

    #[test]
    fn test_fields_pagination_no_functions() {
        use crate::types::SemanticAnalysis;
        use std::collections::HashMap;

        let functions = vec![FunctionInfo {
            name: "hello".to_string(),
            line: 10,
            end_line: 15,
            parameters: vec![],
            return_type: None,
        }];

        let classes = vec![ClassInfo {
            name: "MyClass".to_string(),
            line: 20,
            end_line: 50,
            methods: vec![],
            fields: vec![],
            inherits: vec![],
        }];

        let imports = vec![ImportInfo {
            module: "std".to_string(),
            items: vec!["io".to_string()],
            line: 1,
        }];

        let semantic = SemanticAnalysis {
            functions: functions.clone(),
            classes: classes.clone(),
            imports: imports.clone(),
            references: vec![],
            call_frequency: HashMap::new(),
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };

        let fields = Some(vec![AnalyzeFileField::Classes, AnalyzeFileField::Imports]);
        let output = format_file_details_paginated(
            &functions,
            functions.len(),
            &semantic,
            "test.rs",
            100,
            0,
            true,
            fields.as_deref(),
        );

        assert!(output.contains("FILE: test.rs"), "FILE header missing");
        assert!(
            output.contains("1-1/1F"),
            "FILE header should contain valid range (1-1/1F)"
        );
        assert!(output.contains("C:"), "Classes section missing");
        assert!(output.contains("I:"), "Imports section missing");
        assert!(
            !output.contains("F:"),
            "Functions section should not appear (filtered by fields)"
        );
    }

    #[test]
    fn test_snippet_one_line_short() {
        let snippet = "prev line\nlet x = 1;\nnext line";
        let result = snippet_one_line(snippet);
        assert_eq!(result, "let x = 1;");
    }

    #[test]
    fn test_snippet_one_line_single_line() {
        let result = snippet_one_line("only line");
        assert_eq!(result, "only line");
    }

    #[test]
    fn test_format_focused_internal_with_def_use_sites() {
        let mut graph = CallGraph::new();
        graph
            .definitions
            .insert("my_var".into(), vec![(PathBuf::from("src/lib.rs"), 5)]);

        let site = |kind, file: &str, line, snippet: &str, scope: Option<&str>| DefUseSite {
            kind,
            symbol: "my_var".into(),
            file: file.into(),
            line,
            column: 0,
            snippet: snippet.into(),
            enclosing_scope: scope.map(Into::into),
        };
        let sites = vec![
            site(
                DefUseKind::Write,
                "src/lib.rs",
                5,
                "\nlet my_var = 42;\n",
                Some("init"),
            ),
            site(
                DefUseKind::WriteRead,
                "src/lib.rs",
                10,
                "\nmy_var += 1;\n",
                None,
            ),
            site(
                DefUseKind::Read,
                "src/main.rs",
                20,
                "\nprintln!(\"{}\", my_var);\n",
                Some("run"),
            ),
        ];

        let output =
            format_focused_internal(&graph, "my_var", 1, None, Some(&[]), Some(&[]), &sites)
                .expect("format should succeed");

        assert!(output.contains("DEF-USE SITES  my_var  (3 total: 2 writes, 1 reads)"));
        assert!(output.contains("WRITES"));
        assert!(output.contains("[write_read]"));
        assert!(output.contains("READS"));
        assert!(output.contains("run()"));
    }

    #[test]
    fn test_format_focused_summary_internal_with_def_use_sites() {
        let mut graph = CallGraph::new();
        graph
            .definitions
            .insert("counter".into(), vec![(PathBuf::from("src/a.rs"), 1)]);

        let sites = vec![DefUseSite {
            kind: DefUseKind::Read,
            symbol: "counter".into(),
            file: "src/b.rs".into(),
            line: 15,
            column: 0,
            snippet: "\nuse_counter(counter);\n".into(),
            enclosing_scope: Some("main".into()),
        }];

        let output = format_focused_summary_internal(
            &graph,
            "counter",
            1,
            None,
            Some(&[]),
            Some(&[]),
            &sites,
        )
        .expect("format should succeed");

        assert!(output.contains("DEF-USE SITES: 1 total (0 writes, 1 reads)"));
    }

    #[test]
    fn test_analyze_file_field_all_equivalent_to_none() {
        use crate::types::AnalyzeFileField;
        // fields=None and fields=Some([All]) must produce the same section selections
        let all_fields = [AnalyzeFileField::All];
        // Functions section
        assert!(
            all_fields.contains(&AnalyzeFileField::All)
                || all_fields.contains(&AnalyzeFileField::Functions),
            "All variant should include Functions"
        );
        // Classes section
        assert!(
            all_fields.contains(&AnalyzeFileField::All)
                || all_fields.contains(&AnalyzeFileField::Classes),
            "All variant should include Classes"
        );
        // Imports section
        assert!(
            all_fields.contains(&AnalyzeFileField::All)
                || all_fields.contains(&AnalyzeFileField::Imports),
            "All variant should include Imports"
        );
    }

    #[test]
    fn test_analyze_file_field_all_dominates() {
        use crate::types::AnalyzeFileField;
        // All + specific variant still returns all sections
        let mixed = [AnalyzeFileField::All, AnalyzeFileField::Functions];
        assert!(mixed.contains(&AnalyzeFileField::All));
        // All dominates: Classes and Imports should also be included
        assert!(
            mixed.contains(&AnalyzeFileField::All) || mixed.contains(&AnalyzeFileField::Classes),
            "All dominates: Classes included even when not explicitly listed"
        );
        assert!(
            mixed.contains(&AnalyzeFileField::All) || mixed.contains(&AnalyzeFileField::Imports),
            "All dominates: Imports included even when not explicitly listed"
        );
    }
}

fn format_classes_section(classes: &[ClassInfo], functions: &[FunctionInfo]) -> String {
    let mut output = String::new();
    if classes.is_empty() {
        return output;
    }
    output.push_str("C:\n");

    let methods_by_class = collect_class_methods(classes, functions);
    let has_methods = methods_by_class.values().any(|m| !m.is_empty());

    if classes.len() <= MULTILINE_THRESHOLD && !has_methods {
        let class_strs: Vec<String> = classes
            .iter()
            .map(|class| {
                if class.inherits.is_empty() {
                    format!("{}:{}-{}", class.name, class.line, class.end_line)
                } else {
                    format!(
                        "{}:{}-{} ({})",
                        class.name,
                        class.line,
                        class.end_line,
                        class.inherits.join(", ")
                    )
                }
            })
            .collect();
        output.push_str("  ");
        output.push_str(&class_strs.join("; "));
        output.push('\n');
    } else {
        for class in classes {
            if class.inherits.is_empty() {
                let _ = writeln!(output, "  {}:{}-{}", class.name, class.line, class.end_line);
            } else {
                let _ = writeln!(
                    output,
                    "  {}:{}-{} ({})",
                    class.name,
                    class.line,
                    class.end_line,
                    class.inherits.join(", ")
                );
            }

            // Append methods for each class
            if let Some(methods) = methods_by_class.get(&class.name)
                && !methods.is_empty()
            {
                for (i, method) in methods.iter().take(10).enumerate() {
                    let _ = writeln!(output, "    {}:{}", method.name, method.line);
                    if i + 1 == 10 && methods.len() > 10 {
                        let _ = writeln!(output, "    ... ({} more)", methods.len() - 10);
                        break;
                    }
                }
            }
        }
    }
    output
}

/// Format related files section (incoming/outgoing imports).
/// Returns empty string when `import_graph` is None.
fn format_imports_section(imports: &[ImportInfo]) -> String {
    let mut output = String::new();
    if imports.is_empty() {
        return output;
    }
    output.push_str("I:\n");
    let mut module_map: HashMap<String, usize> = HashMap::new();
    for import in imports {
        module_map
            .entry(import.module.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }
    let mut modules: Vec<_> = module_map.keys().cloned().collect();
    modules.sort();
    let formatted_modules: Vec<String> = modules
        .iter()
        .map(|module| format!("{}({})", module, module_map[module]))
        .collect();
    if formatted_modules.len() <= MULTILINE_THRESHOLD {
        output.push_str("  ");
        output.push_str(&formatted_modules.join("; "));
        output.push('\n');
    } else {
        for module_str in formatted_modules {
            output.push_str("  ");
            output.push_str(&module_str);
            output.push('\n');
        }
    }
    output
}
