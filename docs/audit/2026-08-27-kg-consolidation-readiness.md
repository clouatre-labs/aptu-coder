# KG Consolidation Readiness Audit

> **Status: Superseded.** Recommendations implemented across PRs #1473, #1476, #1478, and #1481 in aptu-coder and PRs #1544, #1551, #1553, and #1554 in aptu.

Date: 2026-08-27
Related: clouatre-labs/aptu#1510, #1525, #1528, #1532, #1533
Issue: #1472

## Context

PR clouatre-labs/aptu#1532 bumped aptu-coder-core to 0.32.0 and verified Calls edge parity between aptu's `graph::builder` and `StructuralGraph::build_from_analysis`. Issue clouatre-labs/aptu#1528 is resolved. The next logical step is consolidating aptu's duplicate graph module (`crates/aptu-core/src/graph/`) into `StructuralGraph` from aptu-coder-core.

## Current State

### aptu's graph module (`graph/`)

- `builder.rs`: `build_from_analysis()` builds `GraphDb` from `SemanticAnalysis` + `CallGraph`
- `query.rs`: `blast_radius()` (bidirectional BFS over `Edge::Calls`), `render_subgraph_text()` (prompt-ready text grouped by file), `find_modified_nodes()`
- `cache.rs`: on-disk cache keyed by `(owner, repo, sha)`, WASM-safe (`#[cfg(not(target_arch = "wasm32"))]`), TTL-based expiration, postcard serialization with schema-hash header
- `mod.rs`: `Node` (File/Module/Function with visibility), `Edge` (Contains/Calls/Imports), `GraphDb = DiGraph<Node, Edge>`

### aptu-coder-core's StructuralGraph

- `build_from_analysis()` / `from_call_graph()`: builds from `&[FileAnalysisOutput]`
- `bfs_blast_radius()` / `blast_radius_subgraph()`: outgoing-only BFS via `graph.neighbors()`
- `GraphDiskStore`: sharded LRU cache with `fs2` file locking, `NamedTempFile` atomic writes, blake3 content-based cache keys
- Cross-file disambiguation (same-file preference, line proximity, arg-count fallback) - new in 0.32.0
- `Node` (File/Symbol with `SymbolKind`/`file_path`/`line`), `Edge` (Contains/Calls/Imports)

## Gap Analysis

### F1: No text rendering (Critical)

aptu's `render_subgraph_text()` (query.rs:131-176) produces prompt-ready text grouped by file path, showing `fn name [calls: a, b] [callers: c]` per function. Only `Function` nodes are rendered; `File` and `Module` nodes are skipped. `StructuralGraph` has no text rendering method. Without it, aptu cannot drop its query module.

### F2: Unidirectional BFS (Critical)

aptu's `blast_radius()` (query.rs:57-108) walks both `Direction::Incoming` (callers) and `Direction::Outgoing` (callees), filtered to `Edge::Calls` only. `StructuralGraph::bfs_frontier()` (structural.rs:309-337) uses `graph.neighbors()` which is outgoing-only. Blast-radius must include callers to be useful for impact analysis in PR review.

### F3: No max_nodes cap (Critical)

aptu caps the blast-radius subgraph at `max_nodes` (default 50,000, configurable via `GraphConfig`) to prevent prompt explosion on large repositories. `StructuralGraph`'s BFS has no node cap.

### F4: Single-seed only (Critical)

aptu accepts `&[NodeIndex]` (multiple modified functions per PR). `StructuralGraph::bfs_blast_radius()` and `blast_radius_subgraph()` accept a single `symbol: &str`, resolving via `symbol_index.get(symbol).and_then(|v| v.first())`. PRs touch multiple functions.

### F5: GraphDiskStore not WASM-safe (Critical)

`GraphDiskStore` (store.rs) uses `fs2::FileExt` (file locking) and `tempfile::NamedTempFile`, neither of which compile under `wasm32-unknown-unknown`. aptu's `cache.rs` gates all I/O behind `#[cfg(not(target_arch = "wasm32"))]` with a WASM stub that returns the provided graph uncached. aptu-coder-core must do the same to preserve aptu's WASM target. Note: `fs2` and `tempfile` are already non-optional dependencies in aptu-coder-core's Cargo.toml, so the graph module must gate their usage at the source level, not via Cargo features.

## Recommendations

### R1: Implement in aptu-coder-core (this issue, #1472)

Add to `StructuralGraph`:

1. `render_subgraph_text(&self, nodes: &[NodeIndex]) -> String` matching aptu's output format (file-grouped, `fn name [calls: ...] [callers: ...]`, skip non-Function nodes)
2. `blast_radius_bidirectional(&self, seeds: &[NodeIndex], max_nodes: usize, max_depth: usize) -> (Vec<NodeIndex>, Vec<(NodeIndex, NodeIndex, Edge)>)` walking both `Direction::Incoming` and `Direction::Outgoing` over `Edge::Calls` only, with `max_nodes` cap
3. Gate `GraphDiskStore` and its `fs2`/`tempfile` usage behind `#[cfg(not(target_arch = "wasm32"))]`, add WASM stubs for `get`/`put`

### R2: Consolidate in aptu (clouatre-labs/aptu#1533)

After aptu-coder ships a release with R1:

1. Replace `graph::builder` with `StructuralGraph::build_from_analysis` / `from_call_graph`
2. Replace `graph::query` with StructuralGraph's new methods
3. Replace `graph::cache` with `GraphDiskStore` (or thin adapter for TTL/config compatibility)
4. Remove `graph::Node`, `graph::Edge`, `graph::GraphDb`
5. Update `ast_context.rs`, `review_context.rs`, `config/graph.rs`

### R3: Release gate

No `[patch]` or git-dependency workarounds. The crates.io release is the gate, per clouatre-labs/aptu#1528's constraint.

## Conclusion

Five critical gaps remain, all in aptu-coder-core. The consolidation is a two-PR, two-release sequence: aptu-coder first (issue #1472), then aptu after a crates.io release ships (issue clouatre-labs/aptu#1533).
