// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Low-level string formatting helpers used across formatter sub-modules.

use crate::types::{ClassInfo, DefUseKind, DefUseSite, FileInfo, FunctionInfo, ImportInfo};
use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;

const MULTILINE_THRESHOLD: usize = 10;
const SNIPPET_MAX_LEN: usize = 80;
const SNIPPET_TRUNCATION_POINT: usize = 77;

/// Check if a function falls within a class's line range (method detection).
pub(crate) fn is_method_of_class(func: &FunctionInfo, class: &ClassInfo) -> bool {
    func.line >= class.line && func.end_line <= class.end_line
}

/// Collect methods for each class, preferring ClassInfo.methods when populated (Rust case),
/// otherwise falling back to line-range detection.
pub(crate) fn collect_class_methods<'a>(
    classes: &'a [ClassInfo],
    functions: &'a [FunctionInfo],
) -> HashMap<String, Vec<&'a FunctionInfo>> {
    let mut methods_by_class: HashMap<String, Vec<&FunctionInfo>> = HashMap::new();
    for class in classes {
        if !class.methods.is_empty() {
            methods_by_class.insert(class.name.clone(), class.methods.iter().collect());
            continue;
        }
        let mut methods: Vec<&FunctionInfo> = functions
            .iter()
            .filter(|func| is_method_of_class(func, class))
            .collect();
        methods.sort_by_key(|f| f.line);
        methods_by_class.insert(class.name.clone(), methods);
    }
    methods_by_class
}

/// Format a list of function signatures wrapped at 100 characters with bullet annotation.
pub(crate) fn format_function_list_wrapped<'a>(
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

/// Build a bracket string for file info.
pub(crate) fn format_file_info_parts(
    line_count: usize,
    fn_count: usize,
    cls_count: usize,
) -> Option<String> {
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
pub(crate) fn def_use_write_read_counts(sites: &[DefUseSite]) -> (usize, usize) {
    let w = sites
        .iter()
        .filter(|s| matches!(s.kind, DefUseKind::Write | DefUseKind::WriteRead))
        .count();
    (w, sites.len() - w)
}

/// Render a WRITES or READS group of def-use sites.
pub(crate) fn render_def_use_group(
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

/// Format a single file entry line for paginated directory listing.
pub(crate) fn format_file_entry(file: &FileInfo, base_path: Option<&Path>) -> String {
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

/// Aggregate directory statistics from analyzed files.
pub(crate) fn aggregate_dir_stats(files_in_dir: &[&FileInfo]) -> (usize, usize, usize) {
    let dir_loc: usize = files_in_dir.iter().map(|f| f.line_count).sum();
    let dir_functions: usize = files_in_dir.iter().map(|f| f.function_count).sum();
    let dir_classes: usize = files_in_dir.iter().map(|f| f.class_count).sum();
    (dir_loc, dir_functions, dir_classes)
}

/// Render a top-files section string for directories with many files.
pub(crate) fn render_top_files_section(
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

/// Format the classes section (C:) for a file.
pub(crate) fn format_classes_section(classes: &[ClassInfo], functions: &[FunctionInfo]) -> String {
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

/// Format the imports section (I:) for a file.
pub(crate) fn format_imports_section(imports: &[ImportInfo]) -> String {
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
