# Audit: Knowledge Graph Implementation Review

Date: 2026-08-24
Commit: 8c5e699
Version: v0.30.0
Toolchain: Rust 1.98.0 / rmcp 3.1.4 / tokio async / petgraph 0.8.3

## Purpose

Review the knowledge graph implementation that shipped after the August 3 design audit
([2026-08-03-knowledge-graph.md](2026-08-03-knowledge-graph.md)). Assess whether the
implementation is best practice, performant, and ontology-complete. Identify concrete
opportunities to reduce LoC and code complexity while improving robustness. The prior audit
was research-only (no code existed); this audit reviews the actual implementation against
current scientific literature and industry best practices.

This is a research-only audit. No `aptu-coder` source was modified. Findings establish
the basis for follow-on implementation issues.

## Scope

- Current branch base: `origin/main`, fetched 2026-08-24.
- Graph module: `crates/aptu-coder-core/src/graph/` (4 files, 1641 LoC).
- MCP Resource surface: `crates/aptu-coder/src/tools/resources.rs` (550 LoC).
- Focused analysis pipeline: `crates/aptu-coder-core/src/analyze_focused.rs` (graph wiring).
- MCP server wiring: `crates/aptu-coder/src/lib.rs` (GraphDiskStore field, resource dispatch).
- Prior audit findings KG1-KG8 re-verified against current source.
- External research: 9 scientific papers (2024-2026), 6 industry tools, petgraph and rmcp
  documentation via Context7, MCP 2026-07-28 specification.

## Methodology

Four delegates in parallel and sequence:

1. **SCOUT** (coder-scout, gpt-5.6-luna): deep codebase analysis of the graph module, resource
   surface, focused analysis pipeline, types, caching layers, and prior audit. Identified
   ontology gaps, architectural duplication, performance patterns, and correctness bugs.
2. **Best-practices research** (gpt-5.6-luna): brave_search and Context7 for petgraph patterns,
   rmcp/MCP resource patterns, serialization recommendations, code ontology standards (Joern CPG,
   OMG KDM, GitHub stack-graphs), and performance patterns. 12 sources.
3. **Scientific literature research** (gpt-5.6-luna): brave_search for recent arxiv papers on
   code knowledge graphs, repository-level code understanding, and AI-assisted software
   development. 9 papers (2024-2026), 6 tools surveyed.
4. **GUARD** (coder-guard, mimo-v2.5): adversarial verification of all 8 scout claims against
   source code. 8/8 confirmed (1 quantification correction).

---

## Background: What Shipped Since the August 3 Audit

The prior audit (KG7) recommended a "C-hybrid" approach: petgraph DiGraph in
`aptu-coder-core/src/graph/`, built from existing analysis output (no second parse pass),
postcard-encoded persistence, used internally to accelerate `analyze_symbol`. The
implementation shipped across issues #1362 and #1363.

### What exists today

*Table 1: Graph module file inventory.*

| File | Total LoC | Production LoC | Test LoC | Purpose |
|---|---:|---:|---:|---|
| `graph/mod.rs` | 11 | 11 | 0 | Module re-exports |
| `graph/structural.rs` | 284 | 178 | 106 | petgraph DiGraph, Node/Edge enums, builder, BFS |
| `graph/store.rs` | 160 | 119 | 41 | Disk cache: postcard + fs2 + blake3 |
| `graph/call_graph.rs` | 1186 | 534 | 652 | In-memory CallGraph, symbol resolution, call-chain BFS |
| `tools/resources.rs` | 550 | 310 | 240 | MCP Resource surface (3 templates) |
| **Total** | **2191** | **1152** | **1039** | |

Two parallel graph representations exist:

- **StructuralGraph** (`structural.rs`): a `petgraph::DiGraph<Node, Edge>` built from
  `FileAnalysisOutput` entries. Persisted to disk via `GraphDiskStore` (postcard, fs2 locks,
  blake3 key). Exposed via MCP Resources (`resources.rs`). Used for blast-radius and subgraph
  queries.
- **CallGraph** (`call_graph.rs`): a `HashMap<String, Vec<CallEdge>>` with callers, callees,
  and definitions maps. Built from `SemanticAnalysis` results. Request-scoped (L1 cache only,
  never persisted). Used by `analyze_symbol` for call-chain traversal.

`GraphDiskStore` is live and wired: `CodeAnalyzer` holds `Arc<GraphDiskStore>` (`lib.rs:232`),
`analyze_focused` builds and persists `StructuralGraph` (`analyze_focused.rs:464-482`), and
`read_resource_impl` reads from it (`lib.rs:804`).

---

## Findings

### F1: Import-closure MCP resource is non-functional (HIGH)

**Severity:** High
**Category:** BUG

The `import-closure` resource template
(`aptu-coder://graph/{repo_hash}/import-closure/{module}`) always returns an empty node list.

`query_to_nodes` (`resources.rs:196-206`) maps `ImportClosure` to
`graph.bfs_blast_radius(module, 1)`. But `bfs_blast_radius` (`structural.rs:109-113`) searches
for `Node::Symbol { name, .. }` only -- it never matches `Node::Module { path, .. }`. Since
module names are stored as `Node::Module`, the start node is never found and the function
returns `vec![]`.

Even if the start node were found, the BFS would still return nothing: `Imports` edges go
`File -> Module`, making `Module` nodes sinks with zero outgoing edges. A correct
import-closure query would need to follow incoming edges (reverse traversal) from the module
to find importing files, then follow those files' outgoing edges.

**Impact:** Any MCP client using the `import-closure` template receives empty results with no
error. The feature appears functional but silently returns nothing.

**Guard verification:** CONFIRMED. `bfs_blast_radius` at `structural.rs:111` matches only
`Node::Symbol`. Module nodes are unreachable.

### F2: Cross-file call edges silently dropped (MEDIUM)

**Severity:** Medium
**Category:** BUG

`build_from_analysis` (`structural.rs:50-105`) processes `FileAnalysisOutput` entries
sequentially, adding symbols to `symbol_index` as it encounters them. When processing calls
(`structural.rs:88-99`), it looks up caller and callee in `symbol_index`. If the callee is
defined in a later entry (a file processed after the caller's file), the callee is not yet in
the index, and the call edge is silently dropped.

**Impact:** In multi-file projects where file processing order (filesystem iteration order)
does not match definition order, cross-file call edges are lost. Blast-radius and subgraph
queries under-report reachability. The `CallGraph` in `call_graph.rs` does not have this bug
because it builds from complete `SemanticAnalysis` results with all symbols indexed first.

**Guard verification:** CONFIRMED. `symbol_index.entry().or_insert()` at `structural.rs:93`
with sequential processing.

### F3: Ontology is aspirational -- 3 of 6 edge types never emitted (MEDIUM)

**Severity:** Medium
**Category:** DESIGN

The `Edge` enum (`structural.rs:37-44`) declares six variants: `Contains`, `Calls`, `Imports`,
`Implements`, `HasMethod`, `Tests`. The builder only emits `Contains`, `Calls`, and `Imports`.
Similarly, `SymbolKind` includes `Trait` and `Impl`, but the builder only creates `Function`
and `Class` symbols.

The unused variants are serialized to disk via postcard, consuming space in the type enum
without producing graph edges. Future code referencing them will find zero edges with no
compiler warning.

**Scientific context:** CodexGraph (arxiv 2408.03910) uses `CONTAINS, HAS_METHOD, INHERITS,
USES, CALLS` -- all emitted. Joern's CPG emits all declared edge types. Dead ontology variants
are not best practice.

**Guard verification:** CONFIRMED. `build_from_analysis` only creates `Contains`, `Calls`,
`Imports` edges.

### F4: Resource payloads return nodes without edges (MEDIUM)

**Severity:** Medium
**Category:** DESIGN
**Tracking:** issue #1449

`query_to_nodes` (`resources.rs:196-206`) returns a flat list of `serde_json::Value` node
serializations. No edge information is included in the response payload. An MCP client
receiving a blast-radius result gets a list of nodes but cannot determine which nodes call
which, what the containment hierarchy is, or what the traversal path was.

**Best practice:** Research indicates resource payloads should include "explicit node and edge
types, stable IDs, source spans, revision, truncation status, and traversal limits." CodexGraph
and KGCompass both return typed edges in their query results.

### F5: BFS start lookup is O(V) (LOW)

**Severity:** Low
**Category:** PERFORMANCE

`bfs_blast_radius` (`structural.rs:109-113`) scans all `node_indices()` with `.find()` to
locate the start node. On the current codebase (~1898 nodes) this is sub-millisecond, but it
scales linearly with project size. A `HashMap<String, NodeIndex>` maintained alongside the
graph would provide O(1) lookup.

The `CallGraph` in `call_graph.rs` already maintains a `lowercase_index: HashMap<String,
usize>` for O(1) exact and case-insensitive symbol resolution. The same pattern could be
applied to `StructuralGraph`.

**Guard verification:** CONFIRMED. Linear scan at `structural.rs:109-113`.

### F6: mtime-based cache key, not content hashes (LOW)

**Severity:** Low
**Category:** ROBUSTNESS

`GraphDiskStore::cache_key` (`store.rs:66-76`) hashes the root path and sorted `(path, mtime)`
pairs. Mtime granularity and filesystem behavior can permit stale cache reuse in unusual
scenarios (e.g., rapid edits within the same mtime tick, copied files preserving mtime).

The `CallGraphCacheKey` in `cache.rs` uses the same mtime-based approach. This is consistent
within the codebase but is a known weakness vs. content-hash keys used by tools like Joern
(sha256 of file content) and GitNexus (commit SHA).

### F7: Two parallel graph representations add complexity (INFO)

**Severity:** Info
**Category:** ARCHITECTURE

`StructuralGraph` (petgraph, persisted, MCP Resources) and `CallGraph` (HashMap, request-scoped,
`analyze_symbol`) serve different workloads but duplicate graph construction logic. The
`StructuralGraph` builder is simpler but has bugs (F2, F3); the `CallGraph` builder is more
mature with indexed symbol resolution, ambiguity handling, and impl-only filtering.

The prior audit recommended keeping both representations ("C-hybrid"). The current evidence
supports this decision: `CallGraph` is specialized for focused analysis with features
(impl_only, match modes, call chains) that `StructuralGraph` does not need, and
`StructuralGraph` provides persistent, queryable graph snapshots for MCP clients.

**Recommendation:** Do not unify. Instead, fix the `StructuralGraph` builder to match
`CallGraph`'s construction quality (two-pass indexing, complete symbol table before edge
resolution).

### F8: Prior audit findings status (INFO)

*Table 2: Prior audit findings re-verification.*

| ID | Prior Finding | Status |
|---|---|---|
| KG1 | `FocusedAnalysisOutput` chain fields `#[serde(skip)]` breaks L2 pagination | **Resolved** -- fields now carry `#[serde(default)]`, not `#[serde(skip)]` (`analyze.rs:523,527,531`). Issue #1361 is closed. |
| KG2 | No persistent structural graph | **Resolved** -- `StructuralGraph` + `GraphDiskStore` shipped. |
| KG3 | aptu#1420 reference schema | **Adopted** -- Node/Edge types mirror aptu's schema. |
| KG4 | Do not commit raw graph to repo | **Honored** -- graph stored in `APTU_CODER_DISK_CACHE_DIR`. |
| KG7 | Implement C-hybrid | **Implemented** -- graph module + postcard persistence. |
| KG8 | MCP Resources surface | **Implemented** -- 3 resource templates, pagination, cursor support. |

---

## Best Practices Comparison

*Table 3: Current implementation vs. research-derived best practices.*

| Practice | Source | Current Status | Gap |
|---|---|---|---|
| Typed, attributed, directed multigraph | CodexGraph, Joern CPG | petgraph `DiGraph<Node, Edge>` with typed enums | None |
| Stable symbol identifiers independent of NodeIndex | petgraph docs, CodexGraph | Bare-name string keys; NodeIndex not persisted | None |
| Separate persisted model from runtime indices | petgraph docs, research | StructuralGraph persisted directly via postcard serde | Minor -- NodeIndex is in the serialized graph but petgraph serde handles this |
| Symbol index for O(1) lookup | petgraph docs, CallGraph (internal) | `symbol_index: HashMap<String, NodeIndex>` in builder only | F5 -- index not retained on StructuralGraph |
| Bounded BFS with visited set and depth/node budget | Research, petgraph Bfs | `bfs_blast_radius` has visited set + depth limit + MAX_GRAPH_DEPTH=5 | None |
| Forward and reverse adjacency for both directions | Research, CodexGraph | `neighbors()` is bidirectional in petgraph (undirected BFS) | Design choice -- bidirectional BFS may over-report for directed semantics |
| Complete ontology: only emit edges backed by data | CodexGraph, Joern | 3 of 6 edge types emitted | F3 |
| Two-pass construction: index all symbols before resolving calls | Research, CallGraph (internal) | Single-pass sequential processing | F2 |
| Return edges in query results | CodexGraph, KGCompass | Nodes only | F4 |
| Content-hash or revision-based cache key | Joern, GitNexus, KGCompass | mtime-based | F6 |
| Versioned serialization with migration | Research | FORMAT_VERSION=1 in store.rs | None |
| Atomic writes with locking | Research | fs2 locks + NamedTempFile::persist | None |
| Postcard for local Rust caches | Research, aptu#1432 | postcard | None |
| No committed raw graph artifacts | Industry consensus | Stored in XDG data dir | None |

### Scientific Literature Alignment

*Table 4: Key papers and their relevance to the current implementation.*

| Paper | Year | Key Insight | Alignment |
|---|---|---|---|
| CodexGraph (2408.03910) | 2024 | Graph-database interface for LLM agents; ontology: MODULE, CLASS, FUNCTION, METHOD, FIELD, GLOBAL_VARIABLE; CONTAINS, HAS_METHOD, INHERITS, USES, CALLS | Our ontology is a subset (File, Symbol, Module; Contains, Calls, Imports). Missing: INHERITS, USES, HAS_METHOD. |
| KGCompass (2503.21710) | 2025 | Repository-aware KG linking issues/PRs to code entities; 69.7% of localized bugs require multi-hop traversal | We support multi-hop BFS but do not link to issues/PRs. Out of scope for a code-analysis tool. |
| RepoGraph | 2024 | Line-level dependency graph with ego-subgraph retrieval | We operate at symbol level, not line level. Appropriate for our use case. |
| CGM (2505.16901) | 2025 | Graph-integrated LLM via adapter; 43% SWE-bench-Lite | Exposes stable node IDs and typed edges for downstream graph-RAG. We provide node JSON but lack edge context (F4). |
| GraphCoder (2406.07003) | 2024 | Statement-level code context graph with coarse-to-fine retrieval | Statement-level is below our granularity. We operate at function/class level. |
| CPG + LLMs (2603.24837) | 2026 | Security-oriented CPG queries; found CVE-2025-6021 | Our graph lacks CFG/PDG/data-flow edges. Out of scope for tree-sitter-based analysis. |
| stack-graphs (GitHub) | -- | Demand-driven name binding and cross-file resolution | We use bare-name matching, not scope-aware resolution. Appropriate trade-off for our scope. |
| Codebase-Memory MCP | -- | Persistent local KG MCP server, 158 languages, sub-ms queries | Direct competitor. We are narrower (tree-sitter languages) but simpler. |

**Key themes from literature:**

1. Multi-hop graph traversal is consistently associated with repository-level comprehension and
   repair. Our BFS blast-radius supports this.
2. The dominant retrieval pattern is hybrid: lexical/embedding retrieval selects anchors, then
   typed graph traversal expands the subgraph. We provide graph traversal only; lexical
   retrieval is handled by `analyze_symbol` and `analyze_file` independently.
3. Local, precomputed, deterministic indexing paired with MCP interfaces is the emerging
   pattern. Our implementation aligns.
4. No universal ontology exists. CodexGraph's 5 edge types (CONTAINS, HAS_METHOD, INHERITS,
   USES, CALLS) are the closest to a consensus minimal set. We implement 3 (Contains, Calls,
   Imports).

---

## Recommendations

### R1: Fix the builder -- two-pass symbol index (fixes F2)

**Priority:** High
**LoC impact:** Neutral (~+10 lines for second pass, removes silent data loss)

Change `build_from_analysis` to two passes:
1. First pass: iterate all entries, add all File, Symbol, and Module nodes, populate
   `symbol_index` completely.
2. Second pass: iterate all entries again, resolve call edges against the complete index.

This matches `CallGraph::build_from_results` which already indexes all definitions before
resolving calls. The fix eliminates silent cross-file call edge loss.

### R2: Fix or remove import-closure (fixes F1)

**Priority:** High
**LoC impact:** -30 to -50 lines if removed; +20 if fixed

**Option A (remove):** Delete the `ImportClosure` variant from `GraphQuery`, remove the
resource template from `list_resource_templates_impl`, and remove the `ImportClosure` branch
from `query_to_nodes`. This reduces complexity and removes a non-functional feature. The
blast-radius and subgraph queries cover the primary use cases.

**Option B (fix):** Add a `bfs_reverse` or `neighbors_directed` traversal that follows
incoming `Imports` edges from a `Module` node. Also fix `bfs_blast_radius` to match
`Node::Module` or add a separate `find_node` method.

**Recommendation:** Option A (remove). Import-closure is a niche query, the current
implementation is broken, and fixing it adds complexity for marginal value. If import-closure
becomes needed later, it can be re-added with correct semantics.

### R3: Remove dead Edge and SymbolKind variants (fixes F3)

**Priority:** Medium
**LoC impact:** -10 to -15 lines

Remove `Edge::Implements`, `Edge::HasMethod`, `Edge::Tests` and `SymbolKind::Trait`,
`SymbolKind::Impl` from the enums. These are never emitted by the builder. Bump
`FORMAT_VERSION` to 2 in `store.rs` to invalidate stale caches (the version check already
handles this gracefully by returning `None` on mismatch).

If these edge types are needed in the future, they can be re-added when the builder is
extended to emit them. Dead variants in a serialized enum are not documentation -- they are
silent dead code.

### R4: Add edge context to resource payloads (fixes F4)

**Priority:** Medium
**LoC impact:** +15 to +20 lines

Extend `query_to_nodes` to return both nodes and edges in the JSON payload:

```json
{
  "nodes": [...],
  "edges": [{"source": 0, "target": 1, "kind": "Calls"}],
  "next_cursor": "...",
  "total": 42
}
```

Collect edges during BFS traversal by tracking `(source, target, edge_weight)` tuples alongside
visited nodes. This gives MCP clients enough context to reconstruct the subgraph structure.

### R5: Retain symbol_index on StructuralGraph (fixes F5)

**Priority:** Low
**LoC impact:** +8 to -12 lines (net reduction by removing O(V) scan)

Add a `symbol_index: HashMap<String, NodeIndex>` field to `StructuralGraph`. Populate it during
`build_from_analysis`. Use it in `bfs_blast_radius` for O(1) start lookup. The index is
transient (not serialized); rebuild it on deserialization via a `post_deserialize` method or
compute it lazily on first query.

**Trade-off:** Adds a field to the struct but eliminates the linear scan. Net complexity
reduction: the O(V) scan is replaced by a simpler HashMap lookup.

### R6: Do not unify graph representations (addresses F7)

**Priority:** Info
**LoC impact:** 0

The two representations serve different workloads. Unifying them would increase complexity,
not reduce it. The `CallGraph` has features (impl_only filtering, match modes, call-chain
formatting) that `StructuralGraph` does not need. The `StructuralGraph` has persistence and
MCP exposure that `CallGraph` does not need. Keep both, but fix the `StructuralGraph` builder
to match `CallGraph`'s construction quality (R1).

### R7: Resolve KG1 -- fix serde-skipped pagination fields (already resolved, no action needed)

**Priority:** N/A -- resolved prior to this audit
**LoC impact:** None (fix already shipped)

This recommendation is stale. `FocusedAnalysisOutput.prod_chains`, `.test_chains`,
`.outgoing_chains` in `analyze.rs` already carry `#[serde(default)]`, not `#[serde(skip)]`.
Issue #1361 is closed. Retained here only for historical continuity with the prior audit's
KG1 finding; see F8.

---

## Summary

*Table 5: Findings.*

| ID | Severity | Category | Finding |
|---|---|---|---|
| F1 | High | BUG | Import-closure MCP resource always returns empty -- `bfs_blast_radius` matches `Node::Symbol` only, never `Node::Module` |
| F2 | Medium | BUG | Cross-file call edges silently dropped -- sequential `symbol_index` population misses callees in later-processed files |
| F3 | Medium | DESIGN | 3 of 6 `Edge` variants (`Implements`, `HasMethod`, `Tests`) and 2 of 6 `SymbolKind` variants (`Trait`, `Impl`) never emitted by builder |
| F4 | Medium | DESIGN | Resource payloads return nodes without edges -- clients cannot reconstruct subgraph structure (issue #1449) |
| F5 | Low | PERF | BFS start lookup is O(V) linear scan -- no retained symbol-to-NodeIndex index |
| F6 | Low | ROBUST | mtime-based cache key, not content hashes -- can go stale in edge cases |
| F7 | Info | ARCH | Two parallel graph representations (`StructuralGraph` petgraph vs `CallGraph` HashMap) -- justified by different workloads |
| F8 | Info | STATUS | KG1 resolved (issue #1361 closed, `#[serde(default)]` fix already shipped); KG2-KG8 resolved |

*Table 6: Recommendations.*

| ID | Priority | LoC Impact | Recommendation |
|---|---|---|---|
| R1 | High | Neutral | Two-pass symbol index in `build_from_analysis` (fixes F2) |
| R2 | High | -30 to -50 | Remove broken import-closure resource template (fixes F1) |
| R3 | Medium | -10 to -15 | Remove dead Edge/SymbolKind variants, bump FORMAT_VERSION (fixes F3) |
| R4 | Medium | +15 to +20 | Add edge context to resource payloads (fixes F4, issue #1449) |
| R5 | Low | -4 net | Retain `symbol_index` on `StructuralGraph` for O(1) lookup (fixes F5) |
| R6 | Info | 0 | Do not unify graph representations (addresses F7) |
| R7 | N/A | None | Already resolved prior to this audit -- KG1 fix shipped as `#[serde(default)]`, issue #1361 closed |

**Net LoC impact of the still-actionable recommendations (R1-R5):** -29 to -49 lines
reduction, with improved correctness and robustness. The largest reduction comes from removing
the broken import-closure feature (R2) and dead enum variants (R3). R7 is excluded from this
total: it was already resolved before this audit and requires no further action.

**Recommended action order:**

1. **R1 (builder fix)** -- two-pass symbol index in `build_from_analysis`. Eliminates silent
   cross-file call edge loss.
2. **R2 (remove import-closure)** -- delete non-functional resource template and related code.
   Reduces complexity.
3. **R3 (remove dead variants)** -- clean up ontology, bump FORMAT_VERSION. Reduces serialized
   graph size.
4. **R4 (edge context)** -- add edges to resource payloads. Improves MCP client usability.
5. **R5 (symbol_index)** -- O(1) BFS start lookup. Performance improvement for large codebases.
