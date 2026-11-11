// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Paginated formatters for directory and file details output.

use crate::formatter::emit;
use crate::types::{AnalyzeFileField, FileInfo, FunctionInfo, SemanticAnalysis};
use std::fmt::Write;
use std::path::Path;
use tracing::instrument;

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
