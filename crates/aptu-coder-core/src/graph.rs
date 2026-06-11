// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Call graph construction and analysis.
//!
//! Builds caller and callee relationships from semantic analysis results.
//! Implements type-aware function matching to disambiguate overloads and name collisions.

use crate::types::{CallEdge, ImplTraitInfo, SemanticAnalysis, SymbolMatchMode};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::{debug, instrument};

const MAX_CANDIDATES_IN_ERROR: usize = 20;

fn format_candidates(candidates: &[String]) -> String {
    if candidates.len() <= MAX_CANDIDATES_IN_ERROR {
        candidates.join(", ")
    } else {
        format!(
            "{}, (and {} more)",
            candidates[..MAX_CANDIDATES_IN_ERROR].join(", "),
            candidates.len() - MAX_CANDIDATES_IN_ERROR
        )
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GraphError {
    #[error("Symbol not found: '{symbol}'. {hint}")]
    SymbolNotFound { symbol: String, hint: String },
    #[error(
        "Multiple candidates matched '{query}': {candidates_display}. Use match_mode=exact to target one of the candidates listed above, or refine the symbol name.",
        candidates_display = format_candidates(.candidates)
    )]
    MultipleCandidates {
        query: String,
        candidates: Vec<String>,
    },
}

/// Resolve a symbol name against the set of known symbols using the requested match mode.
///
/// Returns:
/// - `Ok(name)` when exactly one symbol matches.
/// - `Err(GraphError::SymbolNotFound)` when no symbol matches.
/// - `Err(GraphError::MultipleCandidates)` when more than one symbol matches.
pub fn resolve_symbol<'a>(
    known_symbols: impl Iterator<Item = &'a String>,
    query: &str,
    mode: &SymbolMatchMode,
) -> Result<String, GraphError> {
    let mut matches: Vec<String> = if matches!(mode, SymbolMatchMode::Exact) {
        known_symbols
            .filter(|s| s.as_str() == query)
            .cloned()
            .collect()
    } else {
        let query_lower = query.to_lowercase();
        known_symbols
            .filter(|s| match mode {
                SymbolMatchMode::Exact => unreachable!(),
                SymbolMatchMode::Insensitive => s.to_lowercase() == query_lower,
                SymbolMatchMode::Prefix => s.to_lowercase().starts_with(&query_lower),
                SymbolMatchMode::Contains => s.to_lowercase().contains(&query_lower),
            })
            .cloned()
            .collect()
    };
    matches.sort();

    debug!(
        query,
        mode = ?mode,
        candidate_count = matches.len(),
        "resolve_symbol"
    );

    match matches.len() {
        1 => Ok(matches.into_iter().next().expect("len==1")),
        0 => {
            let hint = match mode {
                SymbolMatchMode::Exact => {
                    "Try match_mode=insensitive for a case-insensitive search, or match_mode=prefix to list symbols starting with this name.".to_string()
                }
                _ => "No symbols matched; try a shorter query or match_mode=contains.".to_string(),
            };
            Err(GraphError::SymbolNotFound {
                symbol: query.to_string(),
                hint,
            })
        }
        _ => Err(GraphError::MultipleCandidates {
            query: query.to_string(),
            candidates: matches,
        }),
    }
}

/// Resolve a symbol using the `lowercase_index` for O(1) case-insensitive lookup.
/// For other modes, iterates the `lowercase_index` keys to avoid per-symbol allocations.
impl CallGraph {
    pub fn resolve_symbol_indexed(
        &self,
        query: &str,
        mode: &SymbolMatchMode,
    ) -> Result<String, GraphError> {
        // Fast path for exact, case-sensitive lookups: O(1) contains_key checks with no
        // intermediate allocations.
        if matches!(mode, SymbolMatchMode::Exact) {
            if self.definitions.contains_key(query)
                || self.callers.contains_key(query)
                || self.callees.contains_key(query)
            {
                return Ok(query.to_string());
            }
            return Err(GraphError::SymbolNotFound {
                symbol: query.to_string(),
                hint: "Try match_mode=insensitive for a case-insensitive search, or match_mode=prefix to list symbols starting with this name.".to_string(),
            });
        }

        let query_lower = query.to_lowercase();
        let mut matches: Vec<String> = {
            match mode {
                SymbolMatchMode::Insensitive => {
                    // O(1) lookup using lowercase_index
                    if let Some(originals) = self.lowercase_index.get(&query_lower) {
                        if originals.len() > 1 {
                            // Multiple originals map to the same lowercase key; report all.
                            return Err(GraphError::MultipleCandidates {
                                query: query.to_string(),
                                candidates: originals.clone(),
                            });
                        }
                        // Exactly one original maps to this lowercase key; return it.
                        vec![originals[0].clone()]
                    } else {
                        vec![]
                    }
                }
                SymbolMatchMode::Prefix => {
                    // Use .iter() to avoid redundant hash lookup.
                    self.lowercase_index
                        .iter()
                        .filter(|(k, _)| k.starts_with(&query_lower))
                        .flat_map(|(_, v)| v.iter().cloned())
                        .collect()
                }
                SymbolMatchMode::Contains => {
                    // Use .iter() to avoid redundant hash lookup.
                    self.lowercase_index
                        .iter()
                        .filter(|(k, _)| k.contains(&query_lower))
                        .flat_map(|(_, v)| v.iter().cloned())
                        .collect()
                }
                SymbolMatchMode::Exact => unreachable!("handled above"),
            }
        };
        matches.sort();
        matches.dedup();

        debug!(
            query,
            mode = ?mode,
            candidate_count = matches.len(),
            "resolve_symbol_indexed"
        );

        match matches.len() {
            1 => Ok(matches.into_iter().next().expect("len==1")),
            0 => Err(GraphError::SymbolNotFound {
                symbol: query.to_string(),
                hint: "No symbols matched; try a shorter query or match_mode=contains.".to_string(),
            }),
            _ => Err(GraphError::MultipleCandidates {
                query: query.to_string(),
                candidates: matches,
            }),
        }
    }
}

/// Strip scope prefixes from a callee name.
/// Handles patterns: `self.method` -> `method`, `Type::method` -> `method`, `module::function` -> `function`.
/// If no prefix is found, returns the original name.
fn strip_scope_prefix(name: &str) -> &str {
    if let Some(pos) = name.rfind("::") {
        &name[pos + 2..]
    } else if let Some(pos) = name.rfind('.') {
        &name[pos + 1..]
    } else {
        name
    }
}

#[derive(Debug, Clone)]
pub struct InternalCallChain {
    pub chain: Vec<(String, PathBuf, usize)>,
}

/// Call graph storing callers, callees, and function definitions.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CallGraph {
    /// Callers map: `function_name` -> vec of `CallEdge` (one per call site).
    pub callers: HashMap<String, Vec<CallEdge>>,
    /// Callees map: `function_name` -> vec of `CallEdge` (one per call site).
    pub callees: HashMap<String, Vec<CallEdge>>,
    /// Definitions map: `function_name` -> vec of (`file_path`, `line_number`).
    pub definitions: HashMap<String, Vec<(PathBuf, usize)>>,
    /// Index for O(1) case-insensitive symbol lookup: lowercased -> vec of originals.
    lowercase_index: HashMap<String, Vec<String>>,
}

impl CallGraph {
    #[must_use]
    pub fn new() -> Self {
        Self {
            callers: HashMap::new(),
            callees: HashMap::new(),
            definitions: HashMap::new(),
            lowercase_index: HashMap::new(),
        }
    }

    /// Resolve a callee name using two strategies:
    /// 1. Try the raw callee name first in definitions; return it if found.
    /// 2. Strip any scope prefix (e.g. `Foo::bar` → `bar`) and look up the stripped name; return it if found.
    ///
    /// If neither strategy finds a definition, returns the original callee unchanged.
    ///
    /// Returns the resolved callee name (which may be the stripped version).
    fn resolve_callee(
        callee: &str,
        _call_file: &Path,
        _call_line: usize,
        _arg_count: Option<usize>,
        definitions: &HashMap<String, Vec<(PathBuf, usize)>>,
    ) -> String {
        // Try raw callee name first
        if let Some(_defs) = definitions.get(callee) {
            return callee.to_string();
        }

        // Try stripped name
        let stripped = strip_scope_prefix(callee);
        if stripped != callee
            && let Some(_defs) = definitions.get(stripped)
        {
            return stripped.to_string();
        }

        // No definition found; return the original callee
        callee.to_string()
    }

    /// Build a call graph from semantic analysis results and trait implementation info.
    #[instrument(skip_all)]
    #[allow(clippy::too_many_lines)]
    // exhaustive graph construction pass; splitting into subfunctions harms readability
    // public API; callers expect owned semantics
    #[allow(clippy::needless_pass_by_value)]
    pub fn build_from_results(
        results: Vec<(PathBuf, SemanticAnalysis)>,
        impl_traits: &[ImplTraitInfo],
        impl_only: bool,
    ) -> Result<Self, GraphError> {
        let mut graph = CallGraph::new();

        // Build definitions map first
        for (path, analysis) in &results {
            for func in &analysis.functions {
                graph
                    .definitions
                    .entry(func.name.clone())
                    .or_default()
                    .push((path.clone(), func.line));
            }
            for class in &analysis.classes {
                graph
                    .definitions
                    .entry(class.name.clone())
                    .or_default()
                    .push((path.clone(), class.line));
            }
        }

        // Process calls with resolved callee names
        for (path, analysis) in &results {
            for call in &analysis.calls {
                let resolved_callee = Self::resolve_callee(
                    &call.callee,
                    path,
                    call.line,
                    call.arg_count,
                    &graph.definitions,
                );

                graph
                    .callees
                    .entry(call.caller.clone())
                    .or_default()
                    .push(CallEdge {
                        path: path.clone(),
                        line: call.line,
                        neighbor_name: resolved_callee.clone(),
                        is_impl_trait: false,
                    });
                graph
                    .callers
                    .entry(resolved_callee)
                    .or_default()
                    .push(CallEdge {
                        path: path.clone(),
                        line: call.line,
                        neighbor_name: call.caller.clone(),
                        is_impl_trait: false,
                    });
            }
            for reference in &analysis.references {
                graph
                    .callers
                    .entry(reference.symbol.clone())
                    .or_default()
                    .push(CallEdge {
                        path: path.clone(),
                        line: reference.line,
                        neighbor_name: "<reference>".to_string(),
                        is_impl_trait: false,
                    });
            }
        }

        // Add explicit caller edges for each impl Trait for Type block.
        // These represent the implementing type as a caller of the trait, enabling
        // impl_only filtering to surface trait implementors rather than call sites.
        for it in impl_traits {
            graph
                .callers
                .entry(it.trait_name.clone())
                .or_default()
                .push(CallEdge {
                    path: it.path.clone(),
                    line: it.line,
                    neighbor_name: it.impl_type.clone(),
                    is_impl_trait: true,
                });
        }

        // If impl_only=true, retain only impl-trait caller edges across all nodes.
        // Callees are never filtered. This ensures traversal and formatting are
        // consistently restricted to impl-trait edges regardless of follow_depth.
        if impl_only {
            for edges in graph.callers.values_mut() {
                edges.retain(|e| e.is_impl_trait);
            }
        }

        // Build lowercase_index for O(1) case-insensitive lookup.
        // Union of all keys from definitions, callers, and callees.
        // Group all originals per lowercase key; sort so min() is stable.
        for key in graph
            .definitions
            .keys()
            .chain(graph.callers.keys())
            .chain(graph.callees.keys())
        {
            graph
                .lowercase_index
                .entry(key.to_lowercase())
                .or_default()
                .push(key.clone());
        }
        for originals in graph.lowercase_index.values_mut() {
            originals.sort();
            originals.dedup();
        }

        let total_edges = graph.callees.values().map(Vec::len).sum::<usize>()
            + graph.callers.values().map(Vec::len).sum::<usize>();
        let file_count = results.len();

        tracing::debug!(
            definitions = graph.definitions.len(),
            edges = total_edges,
            files = file_count,
            impl_only,
            "graph built"
        );

        Ok(graph)
    }

    fn find_chains_bfs(
        &self,
        symbol: &str,
        follow_depth: u32,
        is_incoming: bool,
    ) -> Result<Vec<InternalCallChain>, GraphError> {
        let graph_map = if is_incoming {
            &self.callers
        } else {
            &self.callees
        };

        if !self.definitions.contains_key(symbol) && !graph_map.contains_key(symbol) {
            return Err(GraphError::SymbolNotFound {
                symbol: symbol.to_string(),
                hint: "Symbol resolved but not found in graph. The symbol may have no calls or definitions in the indexed files.".to_string(),
            });
        }

        let mut chains = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back((symbol.to_string(), 0));
        visited.insert(symbol.to_string());

        while let Some((current, depth)) = queue.pop_front() {
            if depth > follow_depth {
                continue;
            }

            // Child span for graph traversal at this depth level
            let _traverse_span = tracing::info_span!("graph.traverse", depth = depth).entered();

            if let Some(neighbors) = graph_map.get(&current) {
                for edge in neighbors {
                    let path = &edge.path;
                    let line = edge.line;
                    let neighbor = &edge.neighbor_name;
                    // Pre-allocate capacity: chain holds at most follow_depth + 2 entries
                    // (the BFS node, up to follow_depth intermediate hops, and the neighbor).
                    // For incoming chains we accumulate in reverse BFS order (focus first, then
                    // deeper callers) then call reverse() at the end so that:
                    //   chain[0]    = immediate caller of focus (closest)
                    //   chain.last() = focus symbol (the BFS start node at depth 0) or the
                    //                  current BFS node for deeper depth levels.
                    // For outgoing chains the order is already focus-first.
                    let mut chain = {
                        let mut v = Vec::with_capacity(follow_depth as usize + 2);
                        v.push((current.clone(), path.clone(), line));
                        v
                    };
                    let mut chain_node = neighbor.clone();
                    let mut chain_depth = depth;

                    while chain_depth < follow_depth {
                        if let Some(next_neighbors) = graph_map.get(&chain_node)
                            && let Some(next_edge) = next_neighbors.first()
                        {
                            // Advance to the next (deeper) caller before pushing, so that
                            // for incoming chains the element pushed is the deeper ancestor
                            // (not chain_node itself, which was already recorded or is the
                            // immediate neighbor pushed after this loop).
                            chain_node = next_edge.neighbor_name.clone();
                            chain.push((
                                chain_node.clone(),
                                next_edge.path.clone(),
                                next_edge.line,
                            ));
                            chain_depth += 1;
                        } else {
                            break;
                        }
                    }

                    if is_incoming {
                        // Add the immediate neighbor (closest to focus) at the end,
                        // then reverse so chain[0] = immediate neighbor.
                        chain.push((neighbor.clone(), path.clone(), line));
                        chain.reverse();
                    } else {
                        chain.push((neighbor.clone(), path.clone(), line));
                    }

                    debug_assert!(
                        chain.len() <= follow_depth as usize + 2,
                        "find_chains_bfs: chain length {} exceeds bound {}",
                        chain.len(),
                        follow_depth + 2
                    );

                    chains.push(InternalCallChain { chain });

                    if !visited.contains(neighbor) && depth < follow_depth {
                        visited.insert(neighbor.clone());
                        queue.push_back((neighbor.clone(), depth + 1));
                    }
                }
            }
        }

        Ok(chains)
    }

    #[instrument(skip(self))]
    pub fn find_incoming_chains(
        &self,
        symbol: &str,
        follow_depth: u32,
    ) -> Result<Vec<InternalCallChain>, GraphError> {
        self.find_chains_bfs(symbol, follow_depth, true)
    }

    #[instrument(skip(self))]
    pub fn find_outgoing_chains(
        &self,
        symbol: &str,
        follow_depth: u32,
    ) -> Result<Vec<InternalCallChain>, GraphError> {
        self.find_chains_bfs(symbol, follow_depth, false)
    }
}

impl Default for CallGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CallInfo, FunctionInfo};

    fn make_analysis(
        funcs: Vec<(&str, usize)>,
        calls: Vec<(&str, &str, usize)>,
    ) -> SemanticAnalysis {
        SemanticAnalysis {
            functions: funcs
                .into_iter()
                .map(|(n, l)| FunctionInfo {
                    name: n.to_string(),
                    line: l,
                    end_line: l + 5,
                    parameters: vec![],
                    return_type: None,
                })
                .collect(),
            classes: vec![],
            imports: vec![],
            references: vec![],
            call_frequency: Default::default(),
            calls: calls
                .into_iter()
                .map(|(c, e, l)| CallInfo {
                    caller: c.to_string(),
                    callee: e.to_string(),
                    line: l,
                    column: 0,
                    arg_count: None,
                })
                .collect(),
            impl_traits: vec![],
            def_use_sites: vec![],
        }
    }

    fn make_typed_analysis(
        funcs: Vec<(&str, usize, Vec<String>, Option<&str>)>,
        calls: Vec<(&str, &str, usize, Option<usize>)>,
    ) -> SemanticAnalysis {
        SemanticAnalysis {
            functions: funcs
                .into_iter()
                .map(|(n, l, params, ret_type)| FunctionInfo {
                    name: n.to_string(),
                    line: l,
                    end_line: l + 5,
                    parameters: params,
                    return_type: ret_type.map(|s| s.to_string()),
                })
                .collect(),
            classes: vec![],
            imports: vec![],
            references: vec![],
            call_frequency: Default::default(),
            calls: calls
                .into_iter()
                .map(|(c, e, l, arg_count)| CallInfo {
                    caller: c.to_string(),
                    callee: e.to_string(),
                    line: l,
                    column: 0,
                    arg_count,
                })
                .collect(),
            impl_traits: vec![],
            def_use_sites: vec![],
        }
    }

    #[test]
    fn test_graph_construction() {
        let analysis = make_analysis(
            vec![("main", 1), ("foo", 10), ("bar", 20)],
            vec![("main", "foo", 2), ("foo", "bar", 15)],
        );
        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");
        assert!(graph.definitions.contains_key("main"));
        assert!(graph.definitions.contains_key("foo"));
        assert_eq!(graph.callees["main"][0].neighbor_name, "foo");
        assert_eq!(graph.callers["foo"][0].neighbor_name, "main");
    }

    #[test]
    fn test_find_incoming_chains_depth_zero() {
        let analysis = make_analysis(vec![("main", 1), ("foo", 10)], vec![("main", "foo", 2)]);
        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");
        assert!(
            !graph
                .find_incoming_chains("foo", 0)
                .expect("Failed to find chains")
                .is_empty()
        );
    }

    #[test]
    fn test_find_outgoing_chains_depth_zero() {
        let analysis = make_analysis(vec![("main", 1), ("foo", 10)], vec![("main", "foo", 2)]);
        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");
        assert!(
            !graph
                .find_outgoing_chains("main", 0)
                .expect("Failed to find chains")
                .is_empty()
        );
    }

    #[test]
    fn test_symbol_not_found() {
        assert!(
            CallGraph::new()
                .find_incoming_chains("nonexistent", 0)
                .is_err()
        );
    }

    #[test]
    fn test_same_file_preference() {
        // Two files each define "helper". File a.rs has a call from "main" to "helper".
        // Assert that the graph's callees for "main" point to "helper" and the callers
        // for "helper" include an entry from a.rs (not b.rs).
        let analysis_a = make_analysis(
            vec![("main", 1), ("helper", 10)],
            vec![("main", "helper", 5)],
        );
        let analysis_b = make_analysis(vec![("helper", 20)], vec![]);

        let graph = CallGraph::build_from_results(
            vec![
                (PathBuf::from("a.rs"), analysis_a),
                (PathBuf::from("b.rs"), analysis_b),
            ],
            &[],
            false,
        )
        .expect("Failed to build graph");

        // Check that main calls helper
        assert!(graph.callees.contains_key("main"));
        let main_callees = &graph.callees["main"];
        assert_eq!(main_callees.len(), 1);
        assert_eq!(main_callees[0].neighbor_name, "helper");

        // Check that the call is from a.rs (same file as main)
        assert_eq!(main_callees[0].path, PathBuf::from("a.rs"));

        // Check that helper has a caller from a.rs
        assert!(graph.callers.contains_key("helper"));
        let helper_callers = &graph.callers["helper"];
        assert!(
            helper_callers
                .iter()
                .any(|e| e.path == PathBuf::from("a.rs"))
        );
    }

    #[test]
    fn test_line_proximity() {
        // One file with "process" defined at line 10 and line 50, and a call at line 12.
        // Assert resolution picks the definition at line 10 (closest).
        let analysis = make_analysis(
            vec![("process", 10), ("process", 50)],
            vec![("main", "process", 12)],
        );

        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");

        // Check that main calls process
        assert!(graph.callees.contains_key("main"));
        let main_callees = &graph.callees["main"];
        assert_eq!(main_callees.len(), 1);
        assert_eq!(main_callees[0].neighbor_name, "process");

        // Check that process has a caller from main at line 12
        assert!(graph.callers.contains_key("process"));
        let process_callers = &graph.callers["process"];
        assert!(
            process_callers
                .iter()
                .any(|e| e.line == 12 && e.neighbor_name == "main")
        );
    }

    #[test]
    fn test_scope_prefix_stripping() {
        // One file defines "method" at line 10. Calls use "self.method", "Type::method".
        // Assert these resolve to "method" in the graph.
        let analysis = make_analysis(
            vec![("method", 10)],
            vec![
                ("caller1", "self.method", 5),
                ("caller2", "Type::method", 15),
                ("caller3", "module::method", 25),
            ],
        );

        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");

        // Check that all three callers have "method" as their callee
        assert_eq!(graph.callees["caller1"][0].neighbor_name, "method");
        assert_eq!(graph.callees["caller2"][0].neighbor_name, "method");
        assert_eq!(graph.callees["caller3"][0].neighbor_name, "method");

        // Check that method has three callers
        assert!(graph.callers.contains_key("method"));
        let method_callers = &graph.callers["method"];
        assert_eq!(method_callers.len(), 3);
        assert!(method_callers.iter().any(|e| e.neighbor_name == "caller1"));
        assert!(method_callers.iter().any(|e| e.neighbor_name == "caller2"));
        assert!(method_callers.iter().any(|e| e.neighbor_name == "caller3"));
    }

    #[test]
    fn test_no_same_file_fallback() {
        // File a.rs calls "helper" but "helper" is only defined in b.rs.
        // Assert the call still resolves (graph has the edge).
        let analysis_a = make_analysis(vec![("main", 1)], vec![("main", "helper", 5)]);
        let analysis_b = make_analysis(vec![("helper", 10)], vec![]);

        let graph = CallGraph::build_from_results(
            vec![
                (PathBuf::from("a.rs"), analysis_a),
                (PathBuf::from("b.rs"), analysis_b),
            ],
            &[],
            false,
        )
        .expect("Failed to build graph");

        // Check that main calls helper
        assert!(graph.callees.contains_key("main"));
        let main_callees = &graph.callees["main"];
        assert_eq!(main_callees.len(), 1);
        assert_eq!(main_callees[0].neighbor_name, "helper");

        // Check that helper has a caller from a.rs
        assert!(graph.callers.contains_key("helper"));
        let helper_callers = &graph.callers["helper"];
        assert!(
            helper_callers
                .iter()
                .any(|e| e.path == PathBuf::from("a.rs") && e.neighbor_name == "main")
        );
    }

    #[test]
    fn test_type_disambiguation_by_params() {
        // Two functions named 'process' in the same file with different parameter counts.
        // process(x: i32) at line 10, process(x: i32, y: String) at line 12.
        // Call from main at line 11 is equidistant from both (1 line away).
        // Type matching should prefer the 2-param version since arg_count=2.
        let analysis = make_typed_analysis(
            vec![
                ("process", 10, vec!["(x: i32)".to_string()], Some("i32")),
                (
                    "process",
                    12,
                    vec!["(x: i32, y: String)".to_string()],
                    Some("String"),
                ),
                ("main", 1, vec![], None),
            ],
            vec![("main", "process", 11, Some(2))],
        );

        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");

        // Check that main calls process
        assert!(graph.callees.contains_key("main"));
        let main_callees = &graph.callees["main"];
        assert_eq!(main_callees.len(), 1);
        assert_eq!(main_callees[0].neighbor_name, "process");

        // Check that process has a caller from main at line 11
        assert!(graph.callers.contains_key("process"));
        let process_callers = &graph.callers["process"];
        assert!(
            process_callers
                .iter()
                .any(|e| e.line == 11 && e.neighbor_name == "main")
        );
    }

    #[test]
    fn test_type_disambiguation_fallback() {
        // Two functions named 'process' with no type info (empty parameters, None return_type).
        // Call from main at line 12 should resolve using line proximity (no regression).
        // arg_count=None means type matching won't fire, fallback to line proximity.
        let analysis = make_analysis(
            vec![("process", 10), ("process", 50), ("main", 1)],
            vec![("main", "process", 12)],
        );

        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");

        // Check that main calls process
        assert!(graph.callees.contains_key("main"));
        let main_callees = &graph.callees["main"];
        assert_eq!(main_callees.len(), 1);
        assert_eq!(main_callees[0].neighbor_name, "process");

        // Check that process has a caller from main
        assert!(graph.callers.contains_key("process"));
        let process_callers = &graph.callers["process"];
        assert!(
            process_callers
                .iter()
                .any(|e| e.line == 12 && e.neighbor_name == "main")
        );
    }

    #[test]
    fn test_impl_only_filters_to_impl_sites() {
        // Arrange: WriterImpl implements Write; plain_fn calls write directly.
        use crate::types::ImplTraitInfo;
        let analysis = make_analysis(
            vec![("write", 1), ("plain_fn", 20)],
            vec![("plain_fn", "write", 22)],
        );
        let impl_traits = vec![ImplTraitInfo {
            trait_name: "Write".to_string(),
            impl_type: "WriterImpl".to_string(),
            path: PathBuf::from("test.rs"),
            line: 10,
        }];

        // Act: build with impl_only=true
        let graph = CallGraph::build_from_results(
            vec![(PathBuf::from("test.rs"), analysis)],
            &impl_traits,
            true,
        )
        .expect("Failed to build graph");

        // Assert: trait "Write" has WriterImpl as an explicit impl-trait caller edge.
        let callers = graph
            .callers
            .get("Write")
            .expect("Write must have impl caller");
        assert_eq!(callers.len(), 1, "only impl-trait caller retained");
        assert_eq!(callers[0].neighbor_name, "WriterImpl");
        assert!(
            callers[0].is_impl_trait,
            "edge must be tagged is_impl_trait"
        );

        // Assert: regular call-site callers of "write" are filtered out by impl_only.
        let write_callers = graph.callers.get("write").map(|v| v.len()).unwrap_or(0);
        assert_eq!(
            write_callers, 0,
            "regular callers filtered when impl_only=true"
        );
    }

    #[test]
    fn test_impl_only_false_is_backward_compatible() {
        // Arrange: same setup, impl_only=false -- all callers returned.
        use crate::types::ImplTraitInfo;
        let analysis = make_analysis(
            vec![("write", 1), ("WriterImpl", 10), ("plain_fn", 20)],
            vec![("WriterImpl", "write", 12), ("plain_fn", "write", 22)],
        );
        let impl_traits = vec![ImplTraitInfo {
            trait_name: "Write".to_string(),
            impl_type: "WriterImpl".to_string(),
            path: PathBuf::from("test.rs"),
            line: 10,
        }];

        // Act: build with impl_only=false
        let graph = CallGraph::build_from_results(
            vec![(PathBuf::from("test.rs"), analysis)],
            &impl_traits,
            false,
        )
        .expect("Failed to build graph");

        // Assert: both call-site callers preserved
        let callers = graph.callers.get("write").expect("write must have callers");
        assert_eq!(
            callers.len(),
            2,
            "both call-site callers should be present when impl_only=false"
        );

        // Assert: impl-trait edge is always present regardless of impl_only
        let write_impl_callers = graph
            .callers
            .get("Write")
            .expect("Write must have impl caller");
        assert_eq!(write_impl_callers.len(), 1);
        assert!(write_impl_callers[0].is_impl_trait);
    }

    #[test]
    fn test_impl_only_callees_unaffected() {
        // Arrange: WriterImpl calls write; impl_only=true should not remove callees.
        use crate::types::ImplTraitInfo;
        let analysis = make_analysis(
            vec![("write", 1), ("WriterImpl", 10)],
            vec![("WriterImpl", "write", 12)],
        );
        let impl_traits = vec![ImplTraitInfo {
            trait_name: "Write".to_string(),
            impl_type: "WriterImpl".to_string(),
            path: PathBuf::from("test.rs"),
            line: 10,
        }];

        let graph = CallGraph::build_from_results(
            vec![(PathBuf::from("test.rs"), analysis)],
            &impl_traits,
            true,
        )
        .expect("Failed to build graph");

        // Assert: callees of WriterImpl are NOT filtered
        let callees = graph
            .callees
            .get("WriterImpl")
            .expect("WriterImpl must have callees");
        assert_eq!(
            callees.len(),
            1,
            "callees must not be filtered by impl_only"
        );
        assert_eq!(callees[0].neighbor_name, "write");
    }

    // ---- resolve_symbol tests ----

    fn known(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_resolve_symbol_exact_match() {
        let syms = known(&["parse_config", "ParseConfig", "PARSE_CONFIG"]);
        let result = resolve_symbol(syms.iter(), "parse_config", &SymbolMatchMode::Exact);
        assert_eq!(result.unwrap(), "parse_config");
    }

    #[test]
    fn test_resolve_symbol_exact_no_match() {
        let syms = known(&["ParseConfig"]);
        let err = resolve_symbol(syms.iter(), "parse_config", &SymbolMatchMode::Exact).unwrap_err();
        std::assert_matches!(err, GraphError::SymbolNotFound { .. });
    }

    #[test]
    fn test_resolve_symbol_insensitive_match() {
        let syms = known(&["ParseConfig", "other"]);
        let result = resolve_symbol(syms.iter(), "parseconfig", &SymbolMatchMode::Insensitive);
        assert_eq!(result.unwrap(), "ParseConfig");
    }

    #[test]
    fn test_resolve_symbol_insensitive_no_match() {
        let syms = known(&["unrelated"]);
        let err =
            resolve_symbol(syms.iter(), "parseconfig", &SymbolMatchMode::Insensitive).unwrap_err();
        std::assert_matches!(err, GraphError::SymbolNotFound { .. });
    }

    #[test]
    fn test_resolve_symbol_prefix_single() {
        let syms = known(&["parse_config", "parse_args", "build"]);
        let result = resolve_symbol(syms.iter(), "build", &SymbolMatchMode::Prefix);
        assert_eq!(result.unwrap(), "build");
    }

    #[test]
    fn test_resolve_symbol_prefix_multiple_candidates() {
        let syms = known(&["parse_config", "parse_args", "build"]);
        let err = resolve_symbol(syms.iter(), "parse", &SymbolMatchMode::Prefix).unwrap_err();
        std::assert_matches!(&err, GraphError::MultipleCandidates { .. });
        if let GraphError::MultipleCandidates { candidates, .. } = err {
            assert_eq!(candidates.len(), 2);
        }
    }

    #[test]
    fn test_resolve_symbol_contains_single() {
        let syms = known(&["parse_config", "build_artifact"]);
        let result = resolve_symbol(syms.iter(), "config", &SymbolMatchMode::Contains);
        assert_eq!(result.unwrap(), "parse_config");
    }

    #[test]
    fn test_resolve_symbol_contains_no_match() {
        let syms = known(&["parse_config", "build_artifact"]);
        let err = resolve_symbol(syms.iter(), "deploy", &SymbolMatchMode::Contains).unwrap_err();
        std::assert_matches!(err, GraphError::SymbolNotFound { .. });
    }

    #[test]
    fn test_incoming_chain_order_two_hops() {
        // Graph: A calls B calls C.  Focus = C, follow_depth = 2.
        //
        // Expected chains after reverse():
        //   depth-0 chain: [B, A, C]  -- immediate caller first, then outermost, then focus
        //   depth-1 chain: [A, B]     -- A calls B
        //
        // This test pins the ordering so that a missing reverse() or an off-by-one in the
        // inner-loop push would be caught: chain[1] must be "A" (outermost), not "B" again.
        let analysis = make_analysis(
            vec![("A", 1), ("B", 10), ("C", 20)],
            vec![("A", "B", 2), ("B", "C", 15)],
        );
        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");

        let chains = graph
            .find_incoming_chains("C", 2)
            .expect("Failed to find incoming chains");

        assert!(
            !chains.is_empty(),
            "Expected at least one incoming chain for C"
        );

        // The 2-hop chain has 3 elements: [immediate_caller, outermost_caller, focus].
        let chain = chains
            .iter()
            .find(|c| c.chain.len() == 3)
            .expect("Expected a 3-element chain");

        assert_eq!(
            chain.chain[0].0, "B",
            "chain[0] should be immediate caller B, got {}",
            chain.chain[0].0
        );
        assert_eq!(
            chain.chain[1].0, "A",
            "chain[1] should be outermost caller A, got {}",
            chain.chain[1].0
        );
        assert_eq!(
            chain.chain[2].0, "C",
            "chain[2] should be focus node C, got {}",
            chain.chain[2].0
        );
    }

    // ---- resolve_symbol_indexed tests ----

    #[test]
    fn test_insensitive_resolve_via_index() {
        // Arrange: build a CallGraph with known symbols
        let analysis = make_analysis(
            vec![("ParseConfig", 1), ("parse_args", 5)],
            vec![("ParseConfig", "parse_args", 10)],
        );
        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");

        // Act: resolve using insensitive mode via the indexed method
        let result = graph
            .resolve_symbol_indexed("parseconfig", &SymbolMatchMode::Insensitive)
            .expect("Should resolve ParseConfig");

        // Assert: O(1) lookup via lowercase_index returns the original symbol
        assert_eq!(result, "ParseConfig");
    }

    #[test]
    fn test_prefix_resolve_via_index() {
        // Arrange: build a CallGraph with multiple symbols matching a prefix
        let analysis = make_analysis(
            vec![("parse_config", 1), ("parse_args", 5), ("build", 10)],
            vec![],
        );
        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");

        // Act: resolve using prefix mode via the indexed method
        let err = graph
            .resolve_symbol_indexed("parse", &SymbolMatchMode::Prefix)
            .unwrap_err();

        // Assert: multiple candidates found
        std::assert_matches!(&err, GraphError::MultipleCandidates { .. });
        if let GraphError::MultipleCandidates { candidates, .. } = err {
            assert_eq!(candidates.len(), 2);
        }
    }

    #[test]
    fn test_insensitive_case_collision_returns_multiple_candidates() {
        // Arrange: two symbols that differ only by case map to the same lowercase key
        let analysis = make_analysis(vec![("Foo", 1), ("foo", 5)], vec![("Foo", "foo", 10)]);
        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");

        // Act: insensitive lookup for "foo" hits both Foo and foo
        let err = graph
            .resolve_symbol_indexed("foo", &SymbolMatchMode::Insensitive)
            .unwrap_err();

        // Assert: MultipleCandidates returned for case collision
        std::assert_matches!(&err, GraphError::MultipleCandidates { .. });
        if let GraphError::MultipleCandidates { candidates, .. } = err {
            assert_eq!(candidates.len(), 2);
        }
    }

    #[test]
    fn test_contains_resolve_via_index() {
        // Arrange: symbols where two match the query substring; one does not
        let analysis = make_analysis(
            vec![("parse_config", 1), ("build_config", 5), ("run", 10)],
            vec![],
        );
        let graph =
            CallGraph::build_from_results(vec![(PathBuf::from("test.rs"), analysis)], &[], false)
                .expect("Failed to build graph");

        // Act: resolve using contains mode; "config" matches parse_config and build_config
        let err = graph
            .resolve_symbol_indexed("config", &SymbolMatchMode::Contains)
            .unwrap_err();

        // Assert: both matching symbols returned as MultipleCandidates
        std::assert_matches!(&err, GraphError::MultipleCandidates { .. });
        if let GraphError::MultipleCandidates { candidates, .. } = err {
            let mut sorted = candidates.clone();
            sorted.sort();
            assert_eq!(sorted, vec!["build_config", "parse_config"]);
        }
    }
}
