// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Per-entity formatters for file details, module info, and summary file details.

use crate::types::{ModuleInfo, SemanticAnalysis};
use std::fmt::Write;
use std::path::Path;
use thiserror::Error;
use tracing::instrument;

use super::emit;

#[derive(Debug, Error)]
pub enum FormatterError {
    #[error("Graph error: {0}")]
    GraphError(#[from] crate::graph::GraphError),
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

    let display_path = emit::strip_base_path(Path::new(path), base_path);
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

    output.push_str(&emit::format_classes_section(
        &analysis.classes,
        &analysis.functions,
    ));

    let top_level_functions: Vec<&crate::types::FunctionInfo> = analysis
        .functions
        .iter()
        .filter(|func| {
            !analysis
                .classes
                .iter()
                .any(|class| emit::is_method_of_class(func, class))
        })
        .collect();

    if !top_level_functions.is_empty() {
        output.push_str("F:\n");
        output.push_str(&emit::format_function_list_wrapped(
            top_level_functions.iter().copied(),
            &analysis.call_frequency,
        ));
    }

    output.push_str(&emit::format_imports_section(&analysis.imports));

    output
}

/// Format a compact summary of file details for large `FileDetails` output.
#[instrument(skip_all)]
pub fn format_file_details_summary(
    semantic: &SemanticAnalysis,
    path: &str,
    line_count: usize,
) -> String {
    let mut output = String::new();

    output.push_str("FILE:\n");
    let _ = writeln!(output, "  path: {path}");
    let fn_count = semantic.functions.len();
    let class_count = semantic.classes.len();
    let _ = writeln!(output, "  {line_count}L, {fn_count}F, {class_count}C");
    output.push('\n');

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

    if !semantic.classes.is_empty() {
        output.push_str("CLASSES:\n");
        if semantic.classes.len() <= 10 {
            let class_strs: Vec<String> = semantic
                .classes
                .iter()
                .map(|c| {
                    if c.inherits.is_empty() {
                        format!("{}:{}-{}", c.name, c.line, c.end_line)
                    } else {
                        format!(
                            "{}:{}-{} ({})",
                            c.name,
                            c.line,
                            c.end_line,
                            c.inherits.join(", ")
                        )
                    }
                })
                .collect();
            let _ = writeln!(output, "  {}", class_strs.join("; "));
        } else {
            for c in &semantic.classes {
                let _ = writeln!(output, "  {}:{}-{}", c.name, c.line, c.end_line);
            }
        }
        output.push('\n');
    }

    if !semantic.imports.is_empty() {
        output.push_str("IMPORTS:\n");
        let import_strs: Vec<String> = semantic
            .imports
            .iter()
            .map(|i| {
                if i.items.is_empty() {
                    format!("  {}", i.module)
                } else {
                    format!("  {}: {}", i.module, i.items.join(", "))
                }
            })
            .collect();
        for s in import_strs {
            let _ = writeln!(output, "{s}");
        }
        output.push('\n');
    }

    output.push_str("SUGGESTION:\n");
    output.push_str("Use analyze_file for full function list, signatures, imports, and classes\n");

    output
}

/// Format a [`ModuleInfo`] into a compact single-block string.
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
