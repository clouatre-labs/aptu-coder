// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Structural knowledge graph over petgraph DiGraph with BFS blast-radius traversal.

use crate::analyze::FileAnalysisOutput;
use petgraph::graph::{DiGraph, NodeIndex};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Class,
    Struct,
    Enum,
    Trait,
    Impl,
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
    Implements,
    HasMethod,
    Tests,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralGraph(pub DiGraph<Node, Edge>);

impl StructuralGraph {
    pub fn build_from_analysis(entries: &[FileAnalysisOutput]) -> Self {
        let mut graph = DiGraph::new();
        let mut seen: HashSet<(NodeIndex, NodeIndex)> = HashSet::new();

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
            for cl in &entry.semantic.calls {
                let caller = graph.node_indices().find(
                    |&i| matches!(&graph[i], Node::Symbol { name, .. } if name == &cl.caller),
                );
                let callee = graph.node_indices().find(
                    |&i| matches!(&graph[i], Node::Symbol { name, .. } if name == &cl.callee),
                );
                if let (Some(c), Some(e)) = (caller, callee)
                    && seen.insert((c, e))
                {
                    graph.add_edge(c, e, Edge::Calls);
                }
            }
        }
        StructuralGraph(graph)
    }

    pub fn bfs_blast_radius(&self, symbol: &str, depth: usize) -> Vec<NodeIndex> {
        let Some(start) = self
            .0
            .node_indices()
            .find(|&i| matches!(&self.0[i], Node::Symbol { name, .. } if name == symbol))
        else {
            return vec![];
        };
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
                for nb in self.0.neighbors(node) {
                    if visited.insert(nb) {
                        result.push(nb);
                        next.push(nb);
                    }
                }
            }
            frontier = next;
        }
        result
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
        assert!(g.0.node_count() >= 4, "nodes={}", g.0.node_count());
        assert!(g.0.edge_count() >= 5, "edges={}", g.0.edge_count());
        assert!(g.0.edge_indices().any(|i| g.0[i] == Edge::Calls));
    }

    #[test]
    fn test_build_empty_input() {
        let e = make_output("src/e.rs", vec![], vec![], vec![], vec![]);
        let g = StructuralGraph::build_from_analysis(&[e]);
        assert_eq!(g.0.node_count(), 1);
        assert_eq!(g.0.edge_count(), 0);
    }

    #[test]
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
        let n =
            g.0.edge_indices()
                .filter(|i| g.0[*i] == Edge::Calls)
                .count();
        assert_eq!(n, 1, "expected 1 Calls edge, got {}", n);
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
        let graph = StructuralGraph(g);
        let r = graph.bfs_blast_radius("A", 2);
        assert_eq!(r.len(), 3, "expected 3 nodes, got {:?}", r);
    }

    #[test]
    fn test_bfs_symbol_not_found() {
        let graph = StructuralGraph(DiGraph::new());
        assert!(graph.bfs_blast_radius("x", 3).is_empty());
    }
}
