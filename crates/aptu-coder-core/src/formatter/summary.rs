// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Aggregate formatters for directory structure summaries.

use crate::formatter::emit;
use crate::traversal::WalkEntry;
use crate::types::FileInfo;
use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use tracing::instrument;

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
