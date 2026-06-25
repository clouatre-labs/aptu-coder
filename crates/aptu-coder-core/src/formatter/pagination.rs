// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Paginated formatters for directory, file details, and focused symbol output.

use crate::formatter::emit;
use crate::graph::InternalCallChain;
use crate::pagination::PaginationMode;
use crate::types::{AnalyzeFileField, FileInfo, FunctionInfo, SemanticAnalysis};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::path::Path;
use tracing::instrument;

/// Format chains as a tree-indented output, grouped by depth-1 symbol.
pub(crate) fn format_chains_as_tree(
    chains: &[(&str, &str)],
    arrow: &str,
    focus_symbol: &str,
) -> String {
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

/// Format a paginated list of files for directory analysis results.
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
            output.push_str(&emit::format_file_entry(file, base_path));
        }
    }

    if !test_files.is_empty() {
        if verbose {
            output.push_str("\nTEST FILES [LOC, FUNCTIONS, CLASSES]\n");
        } else if !prod_files.is_empty() {
            output.push('\n');
        }
        for file in &test_files {
            output.push_str(&emit::format_file_entry(file, base_path));
        }
    }

    output
}

/// Format a paginated subset of functions for `FileDetails` mode.
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

    let start = offset + 1;
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

    if show_classes && offset == 0 && !semantic.classes.is_empty() {
        output.push_str(&emit::format_classes_section(
            &semantic.classes,
            &semantic.functions,
        ));
    }

    if show_imports && offset == 0 && (verbose || !show_all) {
        output.push_str(&emit::format_imports_section(&semantic.imports));
    }

    let top_level_functions: Vec<&FunctionInfo> = functions_page
        .iter()
        .filter(|func| {
            !semantic
                .classes
                .iter()
                .any(|class| emit::is_method_of_class(func, class))
        })
        .collect();

    if show_functions && !top_level_functions.is_empty() {
        output.push_str("F:\n");
        output.push_str(&emit::format_function_list_wrapped(
            top_level_functions.iter().copied(),
            &semantic.call_frequency,
        ));
    }

    output
}

/// Format a paginated subset of callers or callees for `SymbolFocus` mode.
#[instrument(skip_all)]
#[allow(clippy::too_many_arguments)]
#[allow(clippy::similar_names)]
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
    let start = offset + 1;
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
                    .map(|f| emit::strip_base_path(Path::new(f), base_path))
                    .collect();

                let test_count = test_chains.len();
                let _ = writeln!(
                    output,
                    "CALLERS (test): {test_count} test functions (in {})",
                    display_files.join(", ")
                );
            }

            let callee_names: Vec<_> = outgoing_chains
                .iter()
                .filter_map(|chain| chain.chain.first().map(|(p, _, _)| p.clone()))
                .collect::<HashSet<_>>()
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
            let _ = writeln!(output, "CALLERS: {callers_count} production callers");

            if !test_chains.is_empty() {
                let _ = writeln!(
                    output,
                    "CALLERS (test): {} test functions",
                    test_chains.len()
                );
            }

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
