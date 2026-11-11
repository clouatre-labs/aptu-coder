// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Structural code search: metavariable pattern matching and symbol resolution.
//!
//! Two modes back the `search_code` MCP tool:
//!
//! - **Pattern mode** ([`pattern_search`]): an in-house metavariable matcher over the
//!   in-tree tree-sitter parsers. `$NAME` matches any single node; `$$$REST` matches
//!   zero or more sibling nodes. Matches are byte-exact anchored spans of the verbatim
//!   source, suitable for downstream `edit_replace` SEARCH blocks. Deliberately *not*
//!   compiled to tree-sitter `Query` objects: query syntax has no variadic repetition,
//!   so `$$$REST` is unexpressible there; the structural matcher below handles it.
//! - **Symbol mode** ([`symbol_search`]): builds a [`StructuralGraph`] from a directory
//!   analysis on demand (no persistence), then resolves definitions via
//!   `find_symbols_all`, callers via incoming `Calls` edges, and implementors via
//!   `impl Trait for Type` records.
//!
//! Pattern mode is scoped to rust, go, python, and typescript (grammar-sensitive;
//! per-language goldens live in `crates/aptu-coder/tests/search_code.rs`). Symbol mode
//! covers every language the existing graph already supports.

use crate::analyze::analyze_file;
use crate::graph::structural::{Edge, Node, StructuralGraph};
use crate::lang::language_for_extension;
use crate::languages::get_ts_language;
use crate::traversal::walk_directory;
use petgraph::visit::EdgeRef;
use std::path::Path;
use thiserror::Error;
use tree_sitter::{Node as TsNode, Parser, Tree};

/// Languages supported by pattern mode. Symbol mode is not limited to this list.
pub const PATTERN_LANGUAGES: &[&str] = &["rust", "go", "python", "typescript"];

/// Error type for structural code search.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SearchError {
    /// The pattern failed to parse in the target language, contained an invalid
    /// metavariable, or collided with an internal sentinel.
    #[error("Invalid pattern: {0}")]
    InvalidPattern(String),
    /// Pattern mode was requested for a language outside [`PATTERN_LANGUAGES`].
    #[error("Unsupported language for pattern mode: {0}")]
    UnsupportedLanguage(String),
    /// Filesystem access failed while building the symbol graph.
    #[error("Directory walk failed: {0}")]
    Walk(String),
}

/// What kind of location a [`SearchMatch`] anchors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    /// Structural pattern match (pattern mode).
    Pattern,
    /// Definition site of the queried symbol (symbol mode).
    Definition,
    /// A symbol with a resolved call edge to the queried symbol (symbol mode).
    Caller,
    /// An `impl Trait for Type` block implementing the queried trait (symbol mode,
    /// `impl_only`).
    Implementor,
}

impl MatchKind {
    /// Stable label used in text output and telemetry.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchKind::Pattern => "pattern",
            MatchKind::Definition => "definition",
            MatchKind::Caller => "caller",
            MatchKind::Implementor => "implementor",
        }
    }
}

/// A byte-exact anchored span: `file` with `start_byte..end_byte` delimiting the
/// verbatim `text` slice, plus 1-based `start_line`/`start_col` for display. Never
/// paraphrase, reformat, or elide inside `text`; downstream `edit_replace` blocks
/// depend on byte fidelity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// Path of the file containing the match (as passed to the search).
    pub file: String,
    /// Inclusive start byte offset of the verbatim span within the file.
    pub start_byte: usize,
    /// Exclusive end byte offset of the verbatim span within the file.
    pub end_byte: usize,
    /// 1-based line of `start_byte`.
    pub start_line: usize,
    /// 1-based column (byte offset within the line) of `start_byte`.
    pub start_col: usize,
    /// Verbatim source slice `[start_byte, end_byte)`.
    pub text: String,
    /// What this span anchors.
    pub kind: MatchKind,
}

/// A metavariable occurrence found while preprocessing a pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetaVar {
    /// `$NAME`: matches exactly one node.
    Single,
    /// `$$$NAME`: matches zero or more sibling nodes.
    Rest,
}

/// Sentinel identifier substituted for metavariables before parsing the pattern.
/// Chosen to be a valid identifier in all four pattern languages and unlikely to
/// collide with real user identifiers; a collision is rejected as `InvalidPattern`.
fn sentinel(index: usize) -> String {
    format!("ZqMvar{index}Zq")
}

/// Parse a metavariable out of the pattern at `bytes[pos..]`. Returns
/// `Some((meta, name_len))` where `name_len` covers the full `$NAME`/`$$$NAME`
/// token, or `None` when the `$` is a literal (not followed by an identifier).
fn parse_meta_var(bytes: &[u8], pos: usize) -> Option<(MetaVar, usize)> {
    let mut i = pos;
    // Count consecutive dollars: one means $NAME (single), three means $$$NAME
    // (variadic). Two dollars is invalid (treated as a literal `$`).
    let mut dollars = 0usize;
    while dollars < 3 && bytes.get(i) == Some(&b'$') {
        dollars += 1;
        i += 1;
    }
    let meta = if dollars == 3 {
        MetaVar::Rest
    } else {
        MetaVar::Single
    };
    let name_start = i;
    let first = *bytes.get(name_start)?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    i += 1;
    while let Some(&b) = bytes.get(i) {
        if b.is_ascii_alphanumeric() || b == b'_' {
            i += 1;
        } else {
            break;
        }
    }
    if i == name_start {
        return None;
    }
    Some((meta, i - pos))
}

/// Replace metavariables with sentinels, returning the preprocessed pattern and
/// the sentinel table (`sentinel text` -> metavar kind).
fn preprocess(pattern: &str) -> Result<(String, Vec<(String, MetaVar)>), SearchError> {
    let bytes = pattern.as_bytes();
    let mut out = String::with_capacity(pattern.len());
    let mut table: Vec<(String, MetaVar)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$'
            && let Some((meta, len)) = parse_meta_var(bytes, i)
        {
            let index = table.len();
            let s = sentinel(index);
            if pattern.contains(&s) {
                return Err(SearchError::InvalidPattern(format!(
                    "pattern collides with internal sentinel `{s}`; rename the identifier"
                )));
            }
            table.push((s.clone(), meta));
            out.push_str(&s);
            i += len;
        } else {
            let ch_len = pattern[i..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&pattern[i..i + ch_len]);
            i += ch_len;
        }
    }
    Ok((out, table))
}

/// Pattern tree: a parsed AST node annotated with metavariable metadata.
#[derive(Debug)]
struct PatNode {
    /// tree-sitter kind id of the pattern node.
    kind_id: u16,
    /// Named children; a `$$$REST` metavariable child becomes `PatItem::Rest`.
    children: Vec<PatItem>,
    /// Exact text for leaf nodes (no named children); leaves match textually.
    leaf_text: Option<String>,
    /// Set when this node is itself a metavariable (matches any single node).
    meta: Option<MetaVar>,
}

#[derive(Debug)]
enum PatItem {
    Fixed(PatNode),
    Rest,
}

/// Build the pattern tree from a parsed pattern, marking sentinel identifier
/// nodes as metavariables.
fn build_pat_node(node: TsNode, src: &str, table: &[(String, MetaVar)]) -> PatNode {
    let text = &src[node.start_byte()..node.end_byte()];
    if let Some((_, meta)) = table.iter().find(|(s, _)| s == text) {
        return PatNode {
            kind_id: node.kind_id(),
            children: Vec::new(),
            leaf_text: None,
            meta: Some(*meta),
        };
    }
    let mut children = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() {
            children.push(PatItem::Fixed(build_pat_node(child, src, table)));
        }
    }
    let leaf_text = if children.is_empty() {
        Some(text.to_string())
    } else {
        None
    };
    PatNode {
        kind_id: node.kind_id(),
        children,
        leaf_text,
        meta: None,
    }
}

/// Post-process a built pattern tree: promote trailing/leading `Rest` markers.
/// `build_pat_node` already leaves sentinel nodes as `Fixed` leaves; this pass
/// converts sentinel leaves whose table entry is `Rest` into `PatItem::Rest`.
fn promote_rest(pat: &mut PatNode) {
    for child in &mut pat.children {
        if let PatItem::Fixed(node) = child {
            if node.meta == Some(MetaVar::Rest) {
                *child = PatItem::Rest;
            } else {
                promote_rest(node);
            }
        }
    }
}

/// Match a pattern item list against a source named-children slice.
fn match_items(pats: &[PatItem], srcs: &[TsNode], src: &str) -> bool {
    // At most one Rest item is supported per node.
    let rest_pos = pats
        .iter()
        .position(|p| matches!(p, PatItem::Rest))
        .map(|p| {
            if pats[p + 1..].iter().any(|q| matches!(q, PatItem::Rest)) {
                None
            } else {
                Some(p)
            }
        })
        .unwrap_or(Some(usize::MAX));
    let Some(rest_pos) = rest_pos else {
        return false; // multiple $$$REST at one level is invalid; treated as no-match
    };

    if rest_pos == usize::MAX {
        if pats.len() != srcs.len() {
            return false;
        }
        return pats
            .iter()
            .zip(srcs.iter())
            .all(|(p, s)| match_single(p.fixed(), *s, src));
    }

    let (before, after) = (&pats[..rest_pos], &pats[rest_pos + 1..]);
    if before.len() + after.len() > srcs.len() {
        return false;
    }
    for split in before.len()..=(srcs.len() - after.len()) {
        let (_mid, tail) = (
            &srcs[before.len()..split],
            &srcs[split..split + after.len()],
        );
        // `_mid` is the region covered by $$$REST; its contents need no verification.
        let before_ok = before
            .iter()
            .zip(srcs.iter().take(before.len()))
            .all(|(p, s)| match_single(p.fixed(), *s, src));
        let after_ok = after
            .iter()
            .zip(tail.iter())
            .all(|(p, s)| match_single(p.fixed(), *s, src));
        if before_ok && after_ok {
            // Rest matches `mid` (zero or more nodes); nothing further to verify.
            return true;
        }
    }
    false
}

impl PatItem {
    fn fixed(&self) -> &PatNode {
        match self {
            PatItem::Fixed(n) => n,
            PatItem::Rest => unreachable!("Rest items are filtered before match_single"),
        }
    }
}

/// Match one pattern node against one source node.
fn match_single(pat: &PatNode, src: TsNode, src_text: &str) -> bool {
    if pat.meta.is_some() {
        return true; // $NAME matches any single node
    }
    if src.kind_id() != pat.kind_id {
        return false;
    }
    let named: Vec<TsNode> = {
        let mut cursor = src.walk();
        src.children(&mut cursor).filter(|c| c.is_named()).collect()
    };
    if pat.children.is_empty() {
        // Leaf: kind equal plus exact text equality (sentinels never reach here).
        match &pat.leaf_text {
            Some(t) => &src_text[src.start_byte()..src.end_byte()] == t,
            None => true,
        }
    } else {
        match_items(&pat.children, &named, src_text)
    }
}

/// Walk the source tree collecting outermost matches (no descent into matched
/// subtrees), in document order.
fn collect_matches(
    node: TsNode,
    root: &PatNode,
    src: &str,
    file: &str,
    out: &mut Vec<SearchMatch>,
) {
    if match_single(root, node, src) {
        out.push(span_match(
            src,
            file,
            node.start_byte(),
            node.end_byte(),
            MatchKind::Pattern,
        ));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() {
            collect_matches(child, root, src, file, out);
        }
    }
}

/// Build a [`SearchMatch`] from byte offsets, computing 1-based line/column.
fn span_match(
    src: &str,
    file: &str,
    start_byte: usize,
    end_byte: usize,
    kind: MatchKind,
) -> SearchMatch {
    let before = &src[..start_byte];
    let start_line = before.bytes().filter(|b| *b == b'\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |p| p + 1);
    SearchMatch {
        file: file.to_string(),
        start_byte,
        end_byte,
        start_line,
        start_col: start_byte - line_start + 1,
        text: src[start_byte..end_byte].to_string(),
        kind,
    }
}

/// Run structural pattern matching over one source buffer.
///
/// # Errors
///
/// [`SearchError::UnsupportedLanguage`] when `language` is not in
/// [`PATTERN_LANGUAGES`]; [`SearchError::InvalidPattern`] when the pattern
/// preprocesses to an unparseable program.
pub fn pattern_search(
    pattern: &str,
    language: &str,
    source: &str,
    file: &str,
) -> Result<Vec<SearchMatch>, SearchError> {
    if !PATTERN_LANGUAGES.contains(&language) {
        return Err(SearchError::UnsupportedLanguage(language.to_string()));
    }
    let ts_language = get_ts_language(language)
        .ok_or_else(|| SearchError::UnsupportedLanguage(language.to_string()))?;
    let (preprocessed, _) = preprocess(pattern)?;

    // Parse the pattern inside a per-language function-body context. Top-level
    // standalone patterns do not parse in every grammar (e.g. Go requires a
    // package clause; bare expressions are invalid at Rust top level), and the
    // synthetic module wrapper would otherwise demand a one-child module match.
    // The context also fixes the root: the pattern statement(s) inside the
    // wrapper block, or the block itself for multi-statement patterns.
    let (wrapped, wrapper_kind, block_kind) = context_wrap(&preprocessed, language);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(|e| SearchError::InvalidPattern(e.to_string()))?;
    let tree: Tree = parser
        .parse(&wrapped, None)
        .ok_or_else(|| SearchError::InvalidPattern("pattern failed to parse".to_string()))?;
    if tree.root_node().has_error() {
        return Err(SearchError::InvalidPattern(format!(
            "pattern does not parse as {language}: {pattern}"
        )));
    }

    // Descend: root -> wrapper declaration -> block -> pattern statement(s).
    let mut node = tree.root_node();
    let mut wrapper: Option<TsNode> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() && child.kind() == wrapper_kind {
            wrapper = Some(child);
            break;
        }
    }
    node = wrapper.ok_or_else(|| {
        SearchError::InvalidPattern("pattern context failed to build".to_string())
    })?;
    let mut block: Option<TsNode> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() && child.kind() == block_kind {
            block = Some(child);
            break;
        }
    }
    node = block.ok_or_else(|| {
        SearchError::InvalidPattern("pattern context has no body block".to_string())
    })?;
    if node.named_child_count() == 1 {
        node = node
            .named_child(0)
            .ok_or_else(|| SearchError::InvalidPattern("empty pattern".to_string()))?;
    }
    let table: Vec<(String, MetaVar)> = preprocess(pattern)?.1;
    let mut root = build_pat_node(node, &wrapped, &table);
    promote_rest(&mut root);

    let mut parser = Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(|e| SearchError::InvalidPattern(e.to_string()))?;
    let source_tree: Tree = parser
        .parse(source, None)
        .ok_or_else(|| SearchError::InvalidPattern("source failed to parse".to_string()))?;

    let mut matches = Vec::new();
    collect_matches(source_tree.root_node(), &root, source, file, &mut matches);
    // Some grammars (e.g. go's expression_statement) include the trailing
    // newline in the node span; trim it so anchors end at real code.
    for m in &mut matches {
        while m.text.ends_with('\n') || m.text.ends_with('\r') {
            m.text.pop();
            m.end_byte -= 1;
        }
    }
    Ok(matches)
}

/// Language-specific parse context for pattern mode: a function-body wrapper
/// with the pattern inside. Returns the wrapped source plus the expected
/// (wrapper declaration kind, body block kind) used to locate the pattern.
fn context_wrap(preprocessed: &str, language: &str) -> (String, &'static str, &'static str) {
    match language {
        "go" => (
            format!("package aqwrap\n\nfunc aqwrap() {{\n\t{preprocessed}\n}}"),
            "function_declaration",
            "block",
        ),
        "python" => {
            let indented = preprocessed
                .lines()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\n");
            (
                format!("def aqwrap():\n{indented}\n"),
                "function_definition",
                "block",
            )
        }
        "typescript" => (
            format!("function aqwrap() {{\n    {preprocessed}\n}}"),
            "function_declaration",
            "statement_block",
        ),
        // rust and the default
        _ => (
            format!("fn aqwrap() {{\n    {preprocessed}\n}}"),
            "function_item",
            "block",
        ),
    }
}

/// Extract the verbatim text of the 1-based line containing `byte_offset`.
fn line_span(src: &str, byte_offset: usize, file: &str, kind: MatchKind) -> Option<SearchMatch> {
    let clamped = byte_offset.min(src.len());
    let line_start = src[..clamped].rfind('\n').map_or(0, |p| p + 1);
    let line_end = src[line_start..]
        .find('\n')
        .map_or(src.len(), |p| line_start + p);
    let text = src[line_start..line_end].trim_end_matches('\r');
    if text.trim().is_empty() {
        return None;
    }
    Some(SearchMatch {
        file: file.to_string(),
        start_byte: line_start,
        end_byte: line_start + text.len(),
        start_line: src[..line_start].bytes().filter(|b| *b == b'\n').count() + 1,
        start_col: 1,
        text: text.to_string(),
        kind,
    })
}

/// Resolve a symbol across a directory tree on demand.
///
/// Walks `root` (respecting .gitignore via the shared walker), analyzes every
/// supported file, builds a [`StructuralGraph`] in memory (no persistence), and
/// returns definition sites plus resolved callers — or, with `impl_only`, the
/// `impl Trait for Type` blocks for `symbol`. A cold directory pays the full
/// build cost; there is no warm-up store (the #1819 premise was refuted).
///
/// # Errors
///
/// [`SearchError::Walk`] when the directory traversal fails.
pub fn symbol_search(
    root: &Path,
    symbol: &str,
    impl_only: bool,
) -> Result<Vec<SearchMatch>, SearchError> {
    let entries = walk_directory(root, None).map_err(|e| SearchError::Walk(e.to_string()))?;

    let mut analyses = Vec::new();
    let mut sources: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for entry in &entries {
        if entry.is_dir {
            continue;
        }
        let path_str = entry.path.to_string_lossy().to_string();
        let ext = entry.path.extension().and_then(|e| e.to_str());
        let Some(lang_name) = ext.and_then(language_for_extension) else {
            continue;
        };
        let _ = lang_name; // analysis dispatches on extension internally
        let Ok(output) = analyze_file(&path_str, None) else {
            continue;
        };
        if let Ok(src) = std::fs::read_to_string(&entry.path) {
            sources.insert(path_str.clone(), src);
        }
        analyses.push(output);
    }

    let mut matches = Vec::new();

    if impl_only {
        for output in &analyses {
            for impl_info in &output.semantic.impl_traits {
                if impl_info.trait_name != symbol {
                    continue;
                }
                let file = impl_info.path.to_string_lossy().to_string();
                let Some(src) = sources.get(&file) else {
                    continue;
                };
                let offsets = line_offsets(src);
                // impl_info.line is a 0-based row from tree-sitter.
                if let Some(&start) = offsets.get(impl_info.line.saturating_sub(1))
                    && let Some(m) = line_span(src, start, &file, MatchKind::Implementor)
                {
                    matches.push(m);
                }
            }
        }
        matches
            .sort_by(|a, b| (a.file.as_str(), a.start_byte).cmp(&(b.file.as_str(), b.start_byte)));
        matches.dedup_by(|a, b| a.file == b.file && a.start_byte == b.start_byte);
        return Ok(matches);
    }

    let graph = StructuralGraph::build_from_analysis(&analyses);
    let defs = graph.find_symbols_all(&[symbol]);
    for idx in &defs {
        let Node::Symbol {
            name,
            file_path,
            line,
            ..
        } = &graph.graph[*idx]
        else {
            continue;
        };
        let _ = name;
        let Some(src) = sources.get(file_path) else {
            continue;
        };
        let offsets = line_offsets(src);
        if let Some(&start) = offsets.get(line.saturating_sub(1))
            && let Some(m) = line_span(src, start, file_path, MatchKind::Definition)
        {
            matches.push(m);
        }
        // Callers: incoming resolved Calls edges into this definition.
        for edge in graph
            .graph
            .edges_directed(*idx, petgraph::Direction::Incoming)
        {
            if *edge.weight() != Edge::Calls {
                continue;
            }
            let caller_node = &graph.graph[edge.source()];
            let Node::Symbol {
                file_path: caller_file,
                line: caller_line,
                ..
            } = caller_node
            else {
                continue;
            };
            let Some(caller_src) = sources.get(caller_file) else {
                continue;
            };
            let caller_offsets = line_offsets(caller_src);
            if let Some(&start) = caller_offsets.get(caller_line.saturating_sub(1))
                && let Some(m) = line_span(caller_src, start, caller_file, MatchKind::Caller)
            {
                matches.push(m);
            }
        }
    }

    matches.sort_by(|a, b| (a.file.as_str(), a.start_byte).cmp(&(b.file.as_str(), b.start_byte)));
    matches.dedup_by(|a, b| a.file == b.file && a.start_byte == b.start_byte);
    Ok(matches)
}

/// Byte offset of the start of each 0-indexed line.
fn line_offsets(src: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}
#[cfg(test)]
mod tests {
    use super::*;

    const RUST_SRC: &str = "fn alpha() {\n    beta();\n}\n\nfn gamma(x: u32) {\n    x + 1\n}\n";

    #[test]
    fn pattern_rust_fn_item_matches_both_functions() {
        let matches =
            pattern_search("fn $NAME($$$PARAMS) { $$$BODY }", "rust", RUST_SRC, "a.rs").unwrap();
        assert_eq!(matches.len(), 2, "both top-level fns should match");
        assert_eq!(matches[0].start_line, 1);
        assert!(matches[0].text.starts_with("fn alpha()"));
        assert_eq!(matches[1].start_line, 5);
        assert!(matches[1].text.starts_with("fn gamma"));
        // Byte spans are exact: slicing the source reproduces the text.
        assert_eq!(
            &RUST_SRC[matches[0].start_byte..matches[0].end_byte],
            matches[0].text
        );
    }

    #[test]
    fn pattern_leaf_text_is_matched_exactly() {
        // The leaf identifier in a call pattern is matched textually: `helper`
        // must not match `helper2`.
        let src = "def go():\n    helper(1, 2)\n    helper2(3)\n";
        let matches = pattern_search("helper($$$ARGS)", "python", src, "a.py").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text, "helper(1, 2)");
    }

    #[test]
    fn pattern_rest_matches_zero_nodes() {
        let matches = pattern_search("fn $NAME() { $$$BODY }", "rust", RUST_SRC, "a.rs").unwrap();
        assert_eq!(matches.len(), 1, "only the zero-param fn should match");
        assert_eq!(matches[0].start_line, 1);
    }

    #[test]
    fn pattern_unsupported_language_is_rejected() {
        let err = pattern_search("fn $N() {}", "java", RUST_SRC, "a.java").unwrap_err();
        assert!(matches!(err, SearchError::UnsupportedLanguage(l) if l == "java"));
    }

    #[test]
    fn pattern_literal_without_metavariables_matches_verbatim() {
        // A metavariable-free pattern is permitted and behaves as a structural
        // literal search.
        let matches = pattern_search("fn alpha() { beta(); }", "rust", RUST_SRC, "a.rs").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start_line, 1);
    }

    #[test]
    fn pattern_that_does_not_parse_is_rejected() {
        let err = pattern_search("fn $$$X $$$Y", "rust", RUST_SRC, "a.rs").unwrap_err();
        assert!(matches!(err, SearchError::InvalidPattern(_)));
    }

    #[test]
    fn pattern_go_call_shape_matches() {
        let src = "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(1)\n}\n";
        let matches = pattern_search("$PKG.$FN($$$ARGS)", "go", src, "m.go").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text, "fmt.Println(1)");
    }

    #[test]
    fn pattern_python_call_shape_matches() {
        let src = "def go():\n    helper(1, 2)\n";
        let matches = pattern_search("$FN($$$ARGS)", "python", src, "m.py").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text, "helper(1, 2)");
    }

    #[test]
    fn pattern_typescript_call_shape_matches() {
        let src = "function go(): void {\n  helper(true);\n}\n";
        let matches = pattern_search("$FN($$$ARGS);", "typescript", src, "m.ts").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text, "helper(true);");
    }
}
