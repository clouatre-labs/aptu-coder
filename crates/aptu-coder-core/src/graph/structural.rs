// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Structural knowledge graph over petgraph DiGraph with BFS blast-radius traversal.

use crate::analyze::FileAnalysisOutput;
use crate::graph::call_graph::CallGraph;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Class,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Node {
    File {
        path: String,
    },
    Symbol {
        name: String,
        kind: SymbolKind,
        file_path: String,
        line: usize,
    },
    Module {
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Edge {
    Contains,
    Calls,
    Imports,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralGraph {
    pub graph: DiGraph<Node, Edge>,
    #[serde(skip)]
    symbol_index: HashMap<String, Vec<NodeIndex>>,
}

type BuildNodesResult = (
    DiGraph<Node, Edge>,
    HashSet<(NodeIndex, NodeIndex)>,
    HashMap<String, Vec<NodeIndex>>,
    HashMap<NodeIndex, usize>,
);

impl StructuralGraph {
    fn build_symbol_index(graph: &DiGraph<Node, Edge>) -> HashMap<String, Vec<NodeIndex>> {
        let mut index: HashMap<String, Vec<NodeIndex>> = HashMap::new();
        for idx in graph.node_indices() {
            if let Node::Symbol { name, .. } = &graph[idx] {
                index.entry(name.clone()).or_default().push(idx);
            }
        }
        index
    }

    pub fn from_graph(graph: DiGraph<Node, Edge>) -> Self {
        let symbol_index = Self::build_symbol_index(&graph);
        StructuralGraph {
            graph,
            symbol_index,
        }
    }

    pub(crate) fn rebuild_symbol_index(&mut self) {
        self.symbol_index = Self::build_symbol_index(&self.graph);
    }

    /// Disambiguate a list of candidate nodes for a symbol using the heuristic:
    /// a. Return immediately if 0 or 1 candidates.
    /// b. Same-file preference: filter to candidates in call_file; if non-empty, use that pool.
    /// c. Line-proximity: keep only the candidate(s) with minimum distance to call_line.
    /// d. Arg-count match: if call_arg_count is Some(n), prefer a candidate matching that param count.
    /// e. Fallback: return first candidate (first-definition-wins).
    fn resolve_candidate(
        candidates: &[NodeIndex],
        graph: &DiGraph<Node, Edge>,
        call_file: &str,
        call_line: usize,
        call_arg_count: Option<usize>,
        param_counts: &HashMap<NodeIndex, usize>,
    ) -> Option<NodeIndex> {
        if candidates.is_empty() {
            return None;
        }
        if candidates.len() == 1 {
            return candidates.first().copied();
        }

        // Stage b: Same-file preference
        let same_file: Vec<NodeIndex> = candidates
            .iter()
            .filter(|idx| {
                if let Node::Symbol { file_path, .. } = &graph[**idx] {
                    file_path == call_file
                } else {
                    false
                }
            })
            .copied()
            .collect();

        let mut pool: Vec<NodeIndex> = if same_file.is_empty() {
            candidates.to_vec()
        } else {
            same_file
        };

        if pool.len() == 1 {
            return pool.first().copied();
        }

        // Stage c: Line-proximity
        let min_line_distance = pool
            .iter()
            .filter_map(|idx| {
                if let Node::Symbol { line, .. } = &graph[*idx] {
                    Some(line.abs_diff(call_line))
                } else {
                    None
                }
            })
            .min()?;

        pool.retain(|idx| {
            if let Node::Symbol { line, .. } = &graph[*idx] {
                line.abs_diff(call_line) == min_line_distance
            } else {
                false
            }
        });

        if pool.len() == 1 {
            return pool.first().copied();
        }

        // Stage d: Arg-count match
        if let Some(arg_count) = call_arg_count
            && let Some(matching) = pool
                .iter()
                .find(|idx| param_counts.get(idx) == Some(&arg_count))
        {
            return Some(*matching);
        }

        // Stage e: Fallback (first-definition-wins)
        pool.first().copied()
    }

    fn build_nodes(entries: &[FileAnalysisOutput]) -> BuildNodesResult {
        let mut graph = DiGraph::new();
        let mut seen: HashSet<(NodeIndex, NodeIndex)> = HashSet::new();
        let mut symbol_index: HashMap<String, Vec<NodeIndex>> = HashMap::new();
        let mut param_counts: HashMap<NodeIndex, usize> = HashMap::new();

        for entry in entries {
            let fp = &entry.path;
            let file = graph.add_node(Node::File {
                path: fp.to_string(),
            });

            for f in &entry.semantic.functions {
                let n = graph.add_node(Node::Symbol {
                    name: f.name.clone(),
                    kind: SymbolKind::Function,
                    file_path: fp.to_string(),
                    line: f.line,
                });
                if seen.insert((file, n)) {
                    graph.add_edge(file, n, Edge::Contains);
                }
                symbol_index.entry(f.name.clone()).or_default().push(n);
                param_counts.insert(n, f.parameters.len());
            }
            for c in &entry.semantic.classes {
                let n = graph.add_node(Node::Symbol {
                    name: c.name.clone(),
                    kind: SymbolKind::Class,
                    file_path: fp.to_string(),
                    line: c.line,
                });
                if seen.insert((file, n)) {
                    graph.add_edge(file, n, Edge::Contains);
                }
                symbol_index.entry(c.name.clone()).or_default().push(n);
            }
            for im in &entry.semantic.imports {
                if !im.module.is_empty() {
                    let n = graph.add_node(Node::Module {
                        path: im.module.clone(),
                    });
                    if seen.insert((file, n)) {
                        graph.add_edge(file, n, Edge::Imports);
                    }
                }
            }
        }

        (graph, seen, symbol_index, param_counts)
    }

    pub fn build_from_analysis(entries: &[FileAnalysisOutput]) -> Self {
        let (mut graph, mut seen, symbol_index, param_counts) = Self::build_nodes(entries);

        // Pass 2: Resolve call edges against the now-complete symbol_index using disambiguation.
        for entry in entries {
            for cl in &entry.semantic.calls {
                let caller_candidates = symbol_index
                    .get(&cl.caller)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let callee_candidates = symbol_index
                    .get(&cl.callee)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);

                let caller = Self::resolve_candidate(
                    caller_candidates,
                    &graph,
                    entry.path.as_str(),
                    cl.line,
                    None,
                    &param_counts,
                );
                let callee = Self::resolve_candidate(
                    callee_candidates,
                    &graph,
                    entry.path.as_str(),
                    cl.line,
                    cl.arg_count,
                    &param_counts,
                );

                if let (Some(c), Some(e)) = (caller, callee)
                    && seen.insert((c, e))
                {
                    graph.add_edge(c, e, Edge::Calls);
                }
            }
        }

        StructuralGraph {
            graph,
            symbol_index,
        }
    }

    /// Build a StructuralGraph from an already-built CallGraph plus the same entries used to
    /// build it. Reuses CallGraph::callees (already-resolved caller/callee names, including
    /// scope-prefix stripping) instead of re-deriving Calls edges from entry.semantic.calls, so
    /// the expensive edge-resolution pass runs exactly once across both graphs. Node/symbol_index
    /// construction (Pass 1) is unavoidable since CallGraph does not track SymbolKind or imports.
    /// Note: unlike build_from_analysis, this does not have per-call arg_count available (CallEdge
    /// does not carry it), so candidate disambiguation falls back to same-file preference and line
    /// proximity only, without the arg-count tie-break stage.
    pub fn from_call_graph(entries: &[FileAnalysisOutput], call_graph: &CallGraph) -> Self {
        let (mut graph, mut seen, symbol_index, param_counts) = Self::build_nodes(entries);

        for (caller_name, edges) in &call_graph.callees {
            let caller_candidates = symbol_index
                .get(caller_name)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);

            for edge in edges {
                let callee_candidates = symbol_index
                    .get(&edge.neighbor_name)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let call_file = edge.path.to_string_lossy();

                let caller = Self::resolve_candidate(
                    caller_candidates,
                    &graph,
                    &call_file,
                    edge.line,
                    None,
                    &param_counts,
                );
                let callee = Self::resolve_candidate(
                    callee_candidates,
                    &graph,
                    &call_file,
                    edge.line,
                    None,
                    &param_counts,
                );

                if let (Some(c), Some(e)) = (caller, callee)
                    && seen.insert((c, e))
                {
                    graph.add_edge(c, e, Edge::Calls);
                }
            }
        }

        StructuralGraph {
            graph,
            symbol_index,
        }
    }

    /// BFS traversal returning both the visited set (including start) and the tail
    /// (neighbors discovered, excluding start).
    ///
    /// The visited set contains all nodes reached up to the specified depth.
    /// The tail is the BFS-order sequence of nodes discovered, not including start.
    fn bfs_frontier(&self, start: NodeIndex, depth: usize) -> (HashSet<NodeIndex>, Vec<NodeIndex>) {
        let mut visited = HashSet::new();
        let mut result = Vec::new();
        let mut frontier = vec![start];
        visited.insert(start);
        for _ in 0..depth {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for node in frontier {
                for nb in self.graph.neighbors(node) {
                    if visited.insert(nb) {
                        result.push(nb);
                        next.push(nb);
                    }
                }
            }
            frontier = next;
        }
        (visited, result)
    }

    pub fn bfs_blast_radius(&self, symbol: &str, depth: usize) -> Vec<NodeIndex> {
        let Some(start) = self
            .symbol_index
            .get(symbol)
            .and_then(|v| v.first())
            .copied()
        else {
            return vec![];
        };
        self.bfs_frontier(start, depth).1
    }

    /// Blast-radius subgraph including both nodes and edges.
    ///
    /// Returns a tuple of (nodes, edges) where:
    /// - nodes: Vec<NodeIndex> with the start symbol first, followed by all discovered nodes in BFS order
    /// - edges: Vec<(NodeIndex, NodeIndex, Edge)> containing every edge whose source and target
    ///   are both in the visited set (not just edges walked by the BFS tree), allowing clients
    ///   to fully reconstruct the subgraph's connectivity
    ///
    /// If the symbol is not found, returns (vec![], vec![]).
    pub fn blast_radius_subgraph(
        &self,
        symbol: &str,
        depth: usize,
    ) -> (Vec<NodeIndex>, Vec<(NodeIndex, NodeIndex, Edge)>) {
        let Some(start) = self
            .symbol_index
            .get(symbol)
            .and_then(|v| v.first())
            .copied()
        else {
            return (vec![], vec![]);
        };

        let (visited, tail) = self.bfs_frontier(start, depth);

        // Build node list: start first, then all discovered nodes in BFS order
        let mut nodes = vec![start];
        nodes.extend(tail);

        // Collect all edges whose both source and target are in the visited set
        let edges: Vec<(NodeIndex, NodeIndex, Edge)> = self
            .graph
            .edge_references()
            .filter(|e| visited.contains(&e.source()) && visited.contains(&e.target()))
            .map(|e| (e.source(), e.target(), e.weight().clone()))
            .collect();

        (nodes, edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CallInfo, ClassInfo, FunctionInfo, ImportInfo, SemanticAnalysis};
    use std::path::PathBuf;

    fn make_output(
        path: &str,
        funcs: Vec<&str>,
        classes: Vec<&str>,
        imports: Vec<&str>,
        calls: Vec<(&str, &str)>,
    ) -> FileAnalysisOutput {
        FileAnalysisOutput::new(
            path.to_string(),
            format!("{}:1:1:1", path),
            SemanticAnalysis {
                functions: funcs
                    .into_iter()
                    .map(|n| FunctionInfo {
                        name: n.to_string(),
                        line: 1,
                        end_line: 6,
                        parameters: vec![],
                        return_type: None,
                    })
                    .collect(),
                classes: classes
                    .into_iter()
                    .map(|n| ClassInfo {
                        name: n.to_string(),
                        line: 1,
                        end_line: 10,
                        methods: vec![],
                        fields: vec![],
                        inherits: vec![],
                    })
                    .collect(),
                imports: imports
                    .into_iter()
                    .map(|m| ImportInfo {
                        module: m.to_string(),
                        items: vec![],
                        line: 1,
                    })
                    .collect(),
                references: vec![],
                call_frequency: Default::default(),
                calls: calls
                    .into_iter()
                    .map(|(c, e)| CallInfo {
                        caller: c.to_string(),
                        callee: e.to_string(),
                        line: 1,
                        column: 0,
                        arg_count: None,
                    })
                    .collect(),
                impl_traits: vec![],
                def_use_sites: vec![],
            },
            10,
            None,
        )
    }

    /// Test helper for creating custom FunctionInfo with explicit line numbers and parameters.
    fn make_function(name: &str, line: usize, param_count: usize) -> FunctionInfo {
        FunctionInfo {
            name: name.to_string(),
            line,
            end_line: line + 5,
            parameters: (0..param_count).map(|i| format!("p{}", i)).collect(),
            return_type: None,
        }
    }

    /// Test helper for creating custom CallInfo with explicit call and definition lines and arg count.
    fn make_call(
        caller: &str,
        callee: &str,
        call_line: usize,
        arg_count: Option<usize>,
    ) -> CallInfo {
        CallInfo {
            caller: caller.to_string(),
            callee: callee.to_string(),
            line: call_line,
            column: 0,
            arg_count,
        }
    }

    /// Test helper for building a FileAnalysisOutput with custom FunctionInfo and CallInfo.
    fn make_output_custom(
        path: &str,
        functions: Vec<FunctionInfo>,
        calls: Vec<CallInfo>,
    ) -> FileAnalysisOutput {
        FileAnalysisOutput::new(
            path.to_string(),
            format!("{}:1:1:1", path),
            SemanticAnalysis {
                functions,
                classes: vec![],
                imports: vec![],
                references: vec![],
                call_frequency: Default::default(),
                calls,
                impl_traits: vec![],
                def_use_sites: vec![],
            },
            10,
            None,
        )
    }

    #[test]
    fn test_build_happy_path() {
        let e = make_output(
            "src/main.rs",
            vec!["main", "helper"],
            vec!["Config"],
            vec!["std::collections"],
            vec![("main", "helper")],
        );
        let g = StructuralGraph::build_from_analysis(&[e]);
        assert!(g.graph.node_count() >= 4, "nodes={}", g.graph.node_count());
        assert!(g.graph.edge_count() >= 5, "edges={}", g.graph.edge_count());
        assert!(g.graph.edge_indices().any(|i| g.graph[i] == Edge::Calls));
    }

    #[test]
    fn test_build_empty_input() {
        let e = make_output("src/e.rs", vec![], vec![], vec![], vec![]);
        let g = StructuralGraph::build_from_analysis(&[e]);
        assert_eq!(g.graph.node_count(), 1);
        assert_eq!(g.graph.edge_count(), 0);
    }

    #[test]
    /// Two files with the same call edge now produce 2 Calls edges because
    /// same-file preference resolves each file's "main -> helper" call within its own file.
    /// This test verifies that no edge crosses from one file's main to the other file's helper.
    fn test_build_no_cross_file_collision() {
        let e1 = make_output(
            "src/a.rs",
            vec!["main", "helper"],
            vec![],
            vec![],
            vec![("main", "helper")],
        );
        let e2 = make_output(
            "src/b.rs",
            vec!["main", "helper"],
            vec![],
            vec![],
            vec![("main", "helper")],
        );
        let g = StructuralGraph::build_from_analysis(&[e1, e2]);
        let calls_edges: Vec<_> = g
            .graph
            .edge_indices()
            .filter(|i| g.graph[*i] == Edge::Calls)
            .collect();
        assert_eq!(
            calls_edges.len(),
            2,
            "expected 2 Calls edges (same-file preference), got {}",
            calls_edges.len()
        );

        // Verify that each edge's source and target are from the same file
        for edge_idx in calls_edges {
            let (source, target) = g.graph.edge_endpoints(edge_idx).unwrap();
            let source_file = match &g.graph[source] {
                Node::Symbol { file_path, .. } => file_path,
                _ => panic!("source must be Symbol"),
            };
            let target_file = match &g.graph[target] {
                Node::Symbol { file_path, .. } => file_path,
                _ => panic!("target must be Symbol"),
            };
            assert_eq!(
                source_file, target_file,
                "call edge must not cross files: {} -> {}",
                source_file, target_file
            );
        }
    }

    #[test]
    fn test_bfs_diamond() {
        let mut g = DiGraph::new();
        let mut sym = |n: &str| {
            g.add_node(Node::Symbol {
                name: n.into(),
                kind: SymbolKind::Function,
                file_path: "t.rs".into(),
                line: 1,
            })
        };
        let a = sym("A");
        let b = sym("B");
        let c = sym("C");
        let d = sym("D");
        g.add_edge(a, b, Edge::Calls);
        g.add_edge(a, c, Edge::Calls);
        g.add_edge(b, d, Edge::Calls);
        g.add_edge(c, d, Edge::Calls);
        let graph = StructuralGraph::from_graph(g);
        let r = graph.bfs_blast_radius("A", 2);
        assert_eq!(r.len(), 3, "expected 3 nodes, got {:?}", r);
    }

    #[test]
    fn test_bfs_symbol_not_found() {
        let graph = StructuralGraph::from_graph(DiGraph::new());
        assert!(graph.bfs_blast_radius("x", 3).is_empty());
    }

    #[test]
    fn test_build_uses_explicit_path_field() {
        // Regression test: ensure build_from_analysis uses entry.path,
        // not the first line of formatted text, when they differ.
        let mut entry = make_output("correct.rs", vec!["foo"], vec![], vec![], vec![]);
        entry.formatted = "WRONG_PATH\nsome details".to_string();

        let graph = StructuralGraph::build_from_analysis(&[entry]);

        // File node must use correct.rs
        let file_paths: Vec<&str> = graph
            .graph
            .node_weights()
            .filter_map(|n| match n {
                Node::File { path } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(file_paths, vec!["correct.rs"]);

        // Symbol node must use correct.rs
        let symbol_file_paths: Vec<&str> = graph
            .graph
            .node_weights()
            .filter_map(|n| match n {
                Node::Symbol { file_path, .. } => Some(file_path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(symbol_file_paths, vec!["correct.rs"]);
    }

    #[test]
    /// A single file with two identical (caller, callee) entries in the calls list
    /// must still collapse to exactly 1 Calls edge due to the `seen` HashSet.
    fn test_build_dedup_identical_call_within_one_file() {
        let e = make_output(
            "src/a.rs",
            vec!["main", "helper"],
            vec![],
            vec![],
            vec![("main", "helper"), ("main", "helper")], // duplicate call
        );
        let g = StructuralGraph::build_from_analysis(&[e]);
        let n = g
            .graph
            .edge_indices()
            .filter(|i| g.graph[*i] == Edge::Calls)
            .count();
        assert_eq!(
            n, 1,
            "expected 1 Calls edge (dedup identical calls), got {}",
            n
        );
    }

    #[test]
    /// Test same-file preference: when two files each define a same-named callee
    /// at the same line (so line-proximity doesn't break the tie), the caller's own
    /// file is preferred. This isolates the same-file-preference stage.
    fn test_resolve_same_file_preference() {
        // File a.rs defines helper at line 50
        // File b.rs defines helper at line 50 (same line distance to call at line 50)
        // Call in a.rs at line 50 should resolve to a.rs's helper (same file), not b.rs's
        let e_a = make_output_custom(
            "src/a.rs",
            vec![make_function("main", 1, 0), make_function("helper", 50, 0)],
            vec![make_call("main", "helper", 50, None)],
        );
        let e_b = make_output_custom("src/b.rs", vec![make_function("helper", 50, 0)], vec![]);

        let g = StructuralGraph::build_from_analysis(&[e_a, e_b]);

        // Find the Calls edge
        let calls_edges: Vec<_> = g
            .graph
            .edge_indices()
            .filter(|i| g.graph[*i] == Edge::Calls)
            .collect();
        assert_eq!(calls_edges.len(), 1);

        // Extract source and target
        let (_source, target) = g.graph.edge_endpoints(calls_edges[0]).unwrap();
        let target_file = match &g.graph[target] {
            Node::Symbol { file_path, .. } => file_path,
            _ => panic!("target must be Symbol"),
        };
        assert_eq!(
            target_file, "src/a.rs",
            "call should resolve to helper in same file"
        );
    }

    #[test]
    /// Test line-proximity fallback: when same-file preference doesn't narrow to one
    /// candidate, the candidate whose definition line is closest to the call line wins.
    /// This test puts the call in a third file so same-file preference does not apply.
    fn test_resolve_line_proximity_fallback() {
        // File a.rs defines helper at line 45 (5 away from call at line 50)
        // File b.rs defines helper at line 30 (20 away from call at line 50)
        // Call in c.rs (neutral file) should prefer a.rs based on line proximity alone
        let e_a = make_output_custom("src/a.rs", vec![make_function("helper", 45, 0)], vec![]);
        let e_b = make_output_custom("src/b.rs", vec![make_function("helper", 30, 0)], vec![]);
        let e_c = make_output_custom(
            "src/c.rs",
            vec![make_function("caller", 1, 0)],
            vec![make_call("caller", "helper", 50, None)],
        );

        let g = StructuralGraph::build_from_analysis(&[e_a, e_b, e_c]);

        // Find the Calls edge
        let calls_edges: Vec<_> = g
            .graph
            .edge_indices()
            .filter(|i| g.graph[*i] == Edge::Calls)
            .collect();
        assert_eq!(calls_edges.len(), 1);

        let (_, target) = g.graph.edge_endpoints(calls_edges[0]).unwrap();
        let target_line = match &g.graph[target] {
            Node::Symbol { line, .. } => *line,
            _ => panic!("target must be Symbol"),
        };
        assert_eq!(
            target_line, 45,
            "call should resolve to closest definition line"
        );
    }

    #[test]
    /// Test arg-count fallback: when same-file preference doesn't reduce to one
    /// candidate and line-proximity produces a tie (equal distances), the candidate
    /// whose parameter count matches the call's arg_count is preferred.
    fn test_resolve_arg_count_fallback() {
        // File a.rs defines two overloads of "helper":
        // - helper_v1 at line 5 with 1 param (distance 5 from call at line 10)
        // - helper_v2 at line 15 with 2 params (distance 5 from call at line 10)
        // Call at line 10 with 2 args should prefer helper_v2 (param count match)
        // even though both are equidistant via line-proximity
        let e_a = make_output_custom(
            "src/a.rs",
            vec![
                make_function("main", 1, 0),
                make_function("helper", 5, 1), // 1-param version at line 5
                make_function("helper", 15, 2), // 2-param version at line 15
            ],
            vec![make_call("main", "helper", 10, Some(2))],
        );

        let g = StructuralGraph::build_from_analysis(&[e_a]);

        let calls_edges: Vec<_> = g
            .graph
            .edge_indices()
            .filter(|i| g.graph[*i] == Edge::Calls)
            .collect();
        assert_eq!(calls_edges.len(), 1);

        let (_, target) = g.graph.edge_endpoints(calls_edges[0]).unwrap();
        let target_line = match &g.graph[target] {
            Node::Symbol { line, .. } => *line,
            _ => panic!("target must be Symbol"),
        };
        assert_eq!(
            target_line, 15,
            "call should resolve to 2-param version (line 15) via arg-count match"
        );
    }

    #[test]
    /// Test fallback to first-definition-wins: when all disambiguation heuristics
    /// fail to narrow down to one candidate, the first candidate in insertion order
    /// (first NodeIndex added to the symbol_index vector) wins.
    fn test_resolve_true_ambiguity_first_definition_wins() {
        // File a.rs defines two overloads of helper at the same line and with the same param count:
        // - helper_first at line 20 with 0 params (inserted first)
        // - helper_second at line 20 with 0 params (inserted second)
        // Call at line 20 with no arg_count matches both equally.
        // Same-file and line-proximity don't narrow it down.
        // Arg-count doesn't apply (no match criteria or both match).
        // First-definition-wins: the first-inserted wins.
        let e_a = make_output_custom(
            "src/a.rs",
            vec![
                make_function("main", 1, 0),
                make_function("helper", 20, 0), // inserted first
                make_function("helper", 20, 0), // inserted second
            ],
            vec![make_call("main", "helper", 20, None)],
        );

        let g = StructuralGraph::build_from_analysis(&[e_a]);

        let calls_edges: Vec<_> = g
            .graph
            .edge_indices()
            .filter(|i| g.graph[*i] == Edge::Calls)
            .collect();
        assert_eq!(calls_edges.len(), 1);

        // Both candidates are identical in all observable ways (line, file, param count)
        // so we can't directly distinguish which was picked from the graph alone.
        // Just verify that a call edge was created (the resolver didn't fail).
        let (_, target) = g.graph.edge_endpoints(calls_edges[0]).unwrap();
        match &g.graph[target] {
            Node::Symbol { name, .. } => {
                assert_eq!(name, "helper", "call should resolve to a helper symbol");
            }
            _ => panic!("target must be Symbol"),
        }
    }

    #[test]
    /// Documents the accepted divergence between `build_from_analysis()` and
    /// `from_call_graph()` on ambiguous (same-name, differing-param-count) symbols:
    /// `from_call_graph()`'s fast path reuses `CallGraph::callees`, whose `CallEdge` does not
    /// carry `arg_count` (see the comment on `from_call_graph`), so it cannot apply the
    /// arg-count tie-break stage that `build_from_analysis()`'s `resolve_candidate()` uses.
    /// When line-proximity also ties (as in this fixture), the two builders resolve to
    /// different candidates: `build_from_analysis` picks the arg-count match, while
    /// `from_call_graph` falls back to first-definition-wins.
    fn test_from_call_graph_diverges_from_build_from_analysis_on_arg_count_tie() {
        // helper_v1 (1 param, line 5) and helper_v2 (2 params, line 15) are equidistant
        // (5 lines) from the call at line 10, so line-proximity alone can't disambiguate.
        let entry = make_output_custom(
            "src/a.rs",
            vec![
                make_function("main", 1, 0),
                make_function("helper", 5, 1),
                make_function("helper", 15, 2),
            ],
            vec![make_call("main", "helper", 10, Some(2))],
        );

        fn calls_target_line(g: &StructuralGraph) -> usize {
            let calls: Vec<_> = g
                .graph
                .edge_indices()
                .filter(|i| g.graph[*i] == Edge::Calls)
                .collect();
            assert_eq!(calls.len(), 1);
            let (_, target) = g.graph.edge_endpoints(calls[0]).unwrap();
            match &g.graph[target] {
                Node::Symbol { line, .. } => *line,
                _ => panic!("target must be Symbol"),
            }
        }

        let full = StructuralGraph::build_from_analysis(std::slice::from_ref(&entry));
        assert_eq!(
            calls_target_line(&full),
            15,
            "build_from_analysis should use arg-count to pick the 2-param overload"
        );

        let call_graph = CallGraph::build_from_results(
            vec![(PathBuf::from("src/a.rs"), entry.semantic.clone())],
            &[],
            false,
        )
        .expect("call graph build should succeed for this fixture");

        let fast = StructuralGraph::from_call_graph(std::slice::from_ref(&entry), &call_graph);
        assert_eq!(
            calls_target_line(&fast),
            5,
            "from_call_graph lacks arg_count on CallEdge, so on a line-proximity tie it falls \
             back to first-definition-wins instead of matching the call's arg count"
        );
    }
}
