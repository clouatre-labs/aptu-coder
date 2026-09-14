// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Module and wildcard-import lookup extracted from analyze_focused.

use crate::analyze::{AnalyzeError, FocusedAnalysisOutput, MAX_FILE_SIZE_BYTES};
use crate::lang::language_for_extension;
use crate::parser::SemanticExtractor;
use crate::traversal::WalkEntry;
use crate::types::ImportInfo;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use tracing::instrument;

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
