// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Structural knowledge graph over petgraph DiGraph with BFS blast-radius traversal.

use crate::analyze::FileAnalysisOutput;
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
    symbol_index: HashMap<String, NodeIndex>,
}

impl StructuralGraph {
    fn build_symbol_index(graph: &DiGraph<Node, Edge>) -> HashMap<String, NodeIndex> {
        let mut index = HashMap::new();
        for idx in graph.node_indices() {
            if let Node::Symbol { name, .. } = &graph[idx] {
                index.entry(name.clone()).or_insert(idx);
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

    pub fn build_from_analysis(entries: &[FileAnalysisOutput]) -> Self {
        let mut graph = DiGraph::new();
        let mut seen: HashSet<(NodeIndex, NodeIndex)> = HashSet::new();
        let mut symbol_index: HashMap<String, NodeIndex> = HashMap::new();

        // Pass 1: Add all File, Symbol, and Module nodes; populate symbol_index.
        for entry in entries {
            let fp = entry.formatted.lines().next().unwrap_or("");
            let file = graph.add_node(Node::File {
                path: fp.to_string(),
            });

            for f in &entry.semantic.functions {
                let n = graph.add_node(Node::Symbol {
                    name: f.name.clone(),
                    kind: SymbolKind::Function,
                    file_path: fp.to_string(),
                });
                if seen.insert((file, n)) {
                    graph.add_edge(file, n, Edge::Contains);
                }
                symbol_index.entry(f.name.clone()).or_insert(n);
            }
            for c in &entry.semantic.classes {
                let n = graph.add_node(Node::Symbol {
                    name: c.name.clone(),
                    kind: SymbolKind::Class,
                    file_path: fp.to_string(),
                });
                if seen.insert((file, n)) {
                    graph.add_edge(file, n, Edge::Contains);
                }
                symbol_index.entry(c.name.clone()).or_insert(n);
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

        // Pass 2: Resolve call edges against the now-complete symbol_index.
        for entry in entries {
            for cl in &entry.semantic.calls {
                // Name-only keying preserves the prior first-definition-wins semantics: the first node inserted for a given name wins.
                let caller = symbol_index.get(&cl.caller).copied();
                let callee = symbol_index.get(&cl.callee).copied();
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
        let Some(start) = self.symbol_index.get(symbol).copied() else {
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
        let Some(start) = self.symbol_index.get(symbol).copied() else {
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

    fn make_output(
        path: &str,
        funcs: Vec<&str>,
        classes: Vec<&str>,
        imports: Vec<&str>,
        calls: Vec<(&str, &str)>,
    ) -> FileAnalysisOutput {
        FileAnalysisOutput::new(
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
    /// Two files with the same call edge produce exactly 1 Calls edge because the
    /// HashMap index resolves both callers to the same first-definition node, and
    /// the `seen` HashSet deduplicates the edge.
    fn test_build_dedup_edges() {
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
        let n = g
            .graph
            .edge_indices()
            .filter(|i| g.graph[*i] == Edge::Calls)
            .count();
        assert_eq!(
            n, 1,
            "expected 1 Calls edge (first-definition-wins), got {}",
            n
        );
    }

    #[test]
    fn test_bfs_diamond() {
        let mut g = DiGraph::new();
        let mut sym = |n: &str| {
            g.add_node(Node::Symbol {
                name: n.into(),
                kind: SymbolKind::Function,
                file_path: "t.rs".into(),
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
}
