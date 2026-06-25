// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Aggregate formatters for directory structure summaries and focused symbol summaries.

use crate::formatter::emit;
use crate::graph::{CallGraph, InternalCallChain};
use crate::test_detection::is_test_file;
use crate::traversal::WalkEntry;
use crate::types::{DefUseKind, DefUseSite, FileInfo};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use tracing::instrument;

use super::render::FormatterError;

/// Format directory structure analysis results.
#[instrument(skip_all)]
#[allow(clippy::too_many_lines)]
pub fn format_structure(
    entries: &[WalkEntry],
    analysis_results: &[FileInfo],
    max_depth: Option<u32>,
) -> String {
    let mut output = String::new();

    let analysis_map: HashMap<String, &FileInfo> = analysis_results
        .iter()
        .map(|a| (a.path.clone(), a))
        .collect();

    let (prod_files, test_files): (Vec<_>, Vec<_>) =
        analysis_results.iter().partition(|a| !a.is_test);

    let total_loc: usize = analysis_results.iter().map(|a| a.line_count).sum();
    let total_functions: usize = analysis_results.iter().map(|a| a.function_count).sum();
    let total_classes: usize = analysis_results.iter().map(|a| a.class_count).sum();

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

    // PATH block - tree structure
    output.push_str("PATH [LOC, FUNCTIONS, CLASSES]\n");

    let mut test_buf = String::new();

    for entry in entries {
        if entry.depth == 0 {
            continue;
        }

        let indent = "  ".repeat(entry.depth - 1);

        let name = entry
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");

        if entry.is_dir {
            let line = format!("{indent}{name}/\n");
            output.push_str(&line);
        } else if let Some(analysis) = analysis_map.get(&entry.path.display().to_string())
            && let Some(info_str) = emit::format_file_info_parts(
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
        }
    }

    if !test_buf.is_empty() {
        output.push_str("\nTEST FILES [LOC, FUNCTIONS, CLASSES]\n");
        output.push_str(&test_buf);
    }

    output
}

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
                let (dir_loc, dir_functions, dir_classes) =
                    emit::aggregate_dir_stats(&files_in_dir);

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
                    emit::render_top_files_section(&files_in_dir, dir_path, has_classes)
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
                && let Some(info_str) = emit::format_file_info_parts(
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

/// Full-format focused symbol output (callers/callees with chain trees).
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
            let display = emit::strip_base_path(path, base_path);
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
        output.push_str(&crate::formatter::pagination::format_chains_as_tree(
            &prod_refs, "<-", symbol,
        ));
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
            .map(|f| emit::strip_base_path(Path::new(f), base_path))
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
        output.push_str(&crate::formatter::pagination::format_chains_as_tree(
            &outgoing_refs,
            "->",
            symbol,
        ));
    }

    // FILES section - collect unique files from production chains
    let mut files: HashSet<PathBuf> = HashSet::new();
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
                let display = emit::strip_base_path(&file, base_path);
                let _ = writeln!(output, "  {display}");
            }
        }

        // Show test files in separate subsection
        if !test_files.is_empty() {
            output.push_str("  TEST FILES:\n");
            let mut sorted_files = test_files;
            sorted_files.sort();
            for file in sorted_files {
                let display = emit::strip_base_path(&file, base_path);
                let _ = writeln!(output, "    {display}");
            }
        }
    }

    // DEF-USE SITES section - show writes and reads of the symbol
    if !def_use_sites.is_empty() {
        let (write_count, read_count) = emit::def_use_write_read_counts(def_use_sites);
        let total = def_use_sites.len();
        let _ = writeln!(
            output,
            "DEF-USE SITES  {symbol}  ({total} total: {write_count} writes, {read_count} reads)"
        );

        emit::render_def_use_group(
            &mut output,
            def_use_sites,
            "WRITES",
            |s| matches!(s.kind, DefUseKind::Write | DefUseKind::WriteRead),
            base_path,
        );
        emit::render_def_use_group(
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
/// Internal helper that accepts pre-computed chains.
#[instrument(skip_all)]
#[allow(clippy::too_many_lines)] // exhaustive symbol summary formatting; splitting harms readability
#[allow(clippy::similar_names)] // domain pairs: callers_count/callees_count are intentionally similar
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
            let display = emit::strip_base_path(path, base_path);
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
                let file_path = emit::strip_base_path(path, base_path);
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
        let (write_count, read_count) = emit::def_use_write_read_counts(def_use_sites);
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
