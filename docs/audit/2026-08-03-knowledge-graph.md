# Audit: Knowledge Graph Feature Gap

Date: 2026-08-03
Commit: b31dce9
Version: v0.26.2
Toolchain: Rust / rmcp 3.1.0 / tokio async

## Purpose

Determine whether aptu-coder-core is missing a persistent knowledge graph capability, what
that capability would provide, whether agents should commit a graph artifact to the repository,
and how to align with MCP 2026-07-28 and current scientific literature on AI SDLC.

This is a research-only audit. No code was modified. Findings establish the design basis for
follow-on implementation issues.

## Methodology

Three delegates in sequence: (1) codebase scout reading the full aptu-coder workspace plus the
companion aptu repository (aptu#1420, aptu#1408, aptu#1418, aptu#1432); (2) literature and web
research covering seven recent papers, industry tooling (stack-graphs, GitNexus, Codebase-Memory,
CodexGraph, KGCompass, RepoGraph), MCP 2026-07-28 SEPs, and format evaluation; (3) adversarial
guard cross-checking every scout claim against source files and rmcp API surface via Context7,
producing corrections before the final recommendation was set.

---

## Background: What Exists Today

### Graph computation in aptu-coder-core

`crates/aptu-coder-core/src/graph.rs` (1185 lines, 40 functions) builds an in-memory
`CallGraph` -- `HashMap<String, Vec<CallEdge>>` maps for callers, callees, and definitions --
from a full directory walk and parse pass via `build_from_results`. The `CallGraph` and
`CallEdge` structs carry no `Serialize`/`Deserialize` derive (graph.rs:214-226).

`crates/aptu-coder-core/src/cache.rs` (608 lines) holds `CallGraphCache`, an L1 in-memory LRU
keyed by `CallGraphCacheKey` (root path, git ref, follow depth, match mode, impl_only,
ast_recursion_limit, sorted file mtimes).

`crates/aptu-coder-core/src/cache_disk.rs` (443 lines) implements `DiskCache`: sharded files,
`fs2` locking, atomic writes via `tempfile`. It is generic (`get<T: DeserializeOwned>`,
`put<T: Serialize>`) and requires no API changes to serve a new graph store.

**Guard correction (scout error):** The L2 disk cache for `analyze_symbol` already exists.
`symbol_focused.rs:313-316` calls `ctx.disk_cache.get::<FocusedAnalysisOutput>("analyze_symbol",
&disk_key)`. `FocusedAnalysisOutput` is serializable (`#[derive(Serialize, Deserialize)]` at
`analyze.rs:496`). The scout's "L1 memory only" claim was wrong. The real gap is narrower:
`prod_chains`, `test_chains`, `outgoing_chains`, and `def_count` are `#[serde(skip)]` on
`FocusedAnalysisOutput` (analyze.rs:496-540), so cross-session pagination is broken on L2
reload -- chains come back empty. This bug is orthogonal to the knowledge graph question and
requires a separate fix.

`crates/aptu-coder-core/src/types.rs` holds `SemanticAnalysis`, which carries `calls`,
`impl_traits`, and `def_use_sites` as `#[serde(skip)]` fields -- never persisted.

The `CacheTier` enum (`L1Memory`/`L2Disk`/`Miss`/`L1OnlyMiss`/`L1L2Miss`) already appears in
`structuredContent.cache_tier` on `analyze_symbol` responses -- a ready extension point for a
new tier.

The `PaginationMode` enum (`Default`/`Callers`/`Callees`/`DefUse`) and `CursorData` (base64
JSON) are directly reusable for graph-query pagination without new infrastructure.

### What aptu#1420 actually implements

aptu PR #1420 (merged 2026-08-01), title "feat(review): add petgraph-backed structural graph
context for PR review", adds `crates/aptu-core/src/graph/` to the companion **aptu** repository
-- not aptu-coder-core. Four new files, 1183 lines total:

- `builder.rs` -- constructs `petgraph::DiGraph<Node, Edge>` from `build_ast_context` /
  `build_call_graph_context` string output; no direct source re-parse
- `cache.rs` -- serializes via postcard (after aptu#1432 migration from bincode) to
  `~/.local/share/aptu/graph/<repo>/<sha>.bin`, keyed by commit SHA
- `query.rs` -- bounded BFS blast-radius traversal over Calls/Implements/HasMethod/Tests edges
  from Modifies-tagged nodes, capped by `GraphConfig::max_nodes`
- `mod.rs` -- wired into `review_context.rs` between `call_graph` and `ast_context` priority
  tiers under the existing budget ceiling

Node types: `File`, `Module`, `Function`, `Struct`, `Enum`, `Trait`, `Impl`.
Edge types: `Contains`, `Calls`, `Imports`, `Implements`, `HasMethod`, `Modifies` (ephemeral,
never cached), `Tests`.

Built under the hard constraint from issue #1408: no second tree-sitter/AST parse pass;
consume only text output that aptu-coder-core already produces.

**aptu-coder-core itself was not modified by #1408, #1418, or #1420.** The only persisted,
queryable graph in the two-repo ecosystem is scoped to PR-review blast-radius for Rust code in
the downstream aptu repo.

### MCP capability gap

A search across `crates/aptu-coder/src/lib.rs` and `src/tools/server.rs` returns zero matches
for `resources/`, `list_resources`, or `read_resource`. aptu-coder is a tools-only MCP server.

**Guard correction (audit error):** The prior audit draft stated MCP Resources "might be blocked
like server/discover." This is wrong. `server/discover` (SEP-2575) is BLOCKED because rmcp
3.1.0 has not published full SEP-2575 support. MCP Resources predate 2026-07-28 entirely.
Context7 confirms rmcp 3.1.0 has `list_resources` (default empty list), `read_resource`
(default `METHOD_NOT_FOUND`), `list_resource_templates`, and
`ServerCapabilities::builder().enable_resources()`. Resources are fully usable today.

---

## Background: Scientific Literature and Industry Practice

### Evidence for persistent code knowledge graphs

Seven papers reviewed (2024-2026):

**KGCompass** (arXiv:2503.21710): 89.7% of successful fault localizations required 2+ hop graph
traversal. Multi-hop is the primary value source over flat retrieval or embeddings.

**RepoGraph** (ICLR 2025): code graphs boost existing agents by 32.8% relative improvement on
SWE-bench.

**Codebase-Memory** (arXiv:2603.27277): tree-sitter + SQLite + 14 MCP tools; 10x fewer tokens,
2.1x fewer tool calls versus file-exploration agents; sub-ms query latency at 2.1M nodes /
4.9M edges; ~1.2s incremental re-index; full Linux kernel indexed in ~3 minutes. Explicitly
chose SQLite for zero-infrastructure, single-file persistence. Omits semantic embeddings and
remains competitive on quality.

**CodexGraph** (arXiv:2408.03910, NAACL 2025), **Knowledge Graph Based Repo-Level Code
Generation** (arXiv:2505.14394), **Code Graph Model** (arXiv:2505.16901), **Graph-based Agent
Memory** (arXiv:2602.05665): consistent conclusions across systems.

Data most worth persisting (consensus across all reviewed systems): structural/containment
hierarchy, call edges, import/dependency edges, type/inheritance edges. Semantic embeddings are
optional and secondary.

Query patterns agents use: multi-hop path traversal, fixed-radius subgraph extraction (2-hop
neighborhood), call-chain BFS, Cypher-like structured queries. All achievable from the call
graph data aptu-coder-core already computes.

### Industry tooling consensus

No open-source project commits a raw graph (nodes + edges) artifact to version control. All
treat the KG as a regenerable, incrementally-updated local index:

- **GitHub stack-graphs**: SQLite via `SQLiteWriter`, keyed by file-content identity; local CLI
  index only, not committed.
- **GitNexus**: builds a structural KG then auto-generates SKILL.md / AGENTS.md summaries which
  ARE committed; the graph itself is not.
- **Codebase-Memory**: local SQLite under a cache directory, not committed.
- **Understand Anything** (15k+ stars): graph in external/local store outside the source repo.

The pattern that is committed in practice is curated natural-language distillation derived from
a graph, not the raw graph data. Committing a raw graph artifact is not recommended.

### Cross-language schema complexity

**Guard correction (audit overstatement):** The audit flagged "cross-language schema for 18
languages" as a major complexity driver. Analysis of the actual language handlers shows this is
bounded. All 15 language handlers produce language-agnostic output structs: `FunctionInfo`,
`ClassInfo`, `ImportInfo`, `CallInfo`. Per-language divergence is in query depth only (Rust has
`impl_trait_query` and `defuse_query`; Python and TypeScript do not), not in output shape. A
5-node schema (`File`, `Symbol`, `Module`, `Call`, `Import`) covers 90%+ across all 18
supported languages without per-language specialization. Cross-language schema is a manageable
constraint, not a major blocker.

---

## Approach Comparison

Three approaches were evaluated. An adversarial guard challenged the initial recommendation.

### Approach A -- Status quo (downstream-only)

Leave aptu-coder-core as an ephemeral, request-scoped engine. Downstream consumers own their
own graph, as the aptu repo does via #1420.

**Pros:** zero API risk; matches the no-second-parse-pass constraint from aptu#1408.
**Cons:** every downstream consumer rebuilds independently; agents using aptu-coder as an MCP
server directly get no cross-session benefit; the concrete gap is deferred, not closed.

### Approach B -- L3 persisted call graph cache (rejected by guard)

Add `Serialize`/`Deserialize` to `CallGraph`/`CallEdge`. Extend `CacheTier` with `L3Kg`. Extend
`DiskCache` to cover call graphs. No new MCP surface.

**Guard rejection rationale:**

1. **Solves a non-problem.** `analyze_symbol` is 0.2% of all tool calls (223 of 118,113 in
   31 days, per the 2026-07-05 observability audit), 156ms average, ~35 seconds per month total
   exposure. An L3 cache saves ~27 seconds per month on the least-used tool.

2. **B's serde work does not transfer to C.** B requires serde on `CallGraph` + `CallEdge`
   (two existing types). C requires serde on new petgraph `Node`/`Edge` types. Zero type
   overlap. Doing B first creates two independent serialization migrations, not one staged one.

3. **The L2 disk cache already exists** (symbol_focused.rs:313-316). B's premise -- that there
   is no disk persistence for `analyze_symbol` -- is incorrect. The issue is the `#[serde(skip)]`
   fields breaking pagination on reload, which B does not fix either.

4. **Creates rework.** B would be replaced by C-hybrid. The only shared infrastructure
   (`DiskCache`) is already generic and requires no changes.

**Verdict:** do not implement B as a standalone step.

### Approach C-hybrid -- Internal persisted graph module (recommended)

Introduce `crates/aptu-coder-core/src/graph/` (or `kg/`) backed by `petgraph::DiGraph<Node,
Edge>`, built from existing `analyze.rs` output (no second tree-sitter parse, per aptu#1408),
persisted via postcard (per aptu#1432 precedent) in `APTU_CODER_DISK_CACHE_DIR`. Used
internally to accelerate `analyze_symbol`: on cold cache, build the graph once and persist;
on warm cache, reload the graph and recompute BFS chains from it (cheap, avoids the L2
pagination bug). No new MCP surface in this phase.

*Table 1: Approach comparison.*

| Criterion | B | C-hybrid | C-full (+Resources) |
|---|---|---|---|
| Serde work transfers to C | No | -- | Same as C-hybrid |
| Solves a real hotspot | No (0.2% of calls) | Yes (any large-repo cold start) | Yes |
| New dependency (petgraph) | No | Yes (pure Rust, no_std, validated in aptu#1420) | Yes |
| MCP surface change | None | None | Resources (usable today in rmcp 3.1.0) |
| Creates rework | Yes | No | No |
| Fixes pagination L2 bug | No | Orthogonal (separate fix needed) | Orthogonal |
| Risk level | Medium | Low | Medium |
| Estimated files touched | 6 | ~8 (new graph/ module + integrate) | ~12 |

**Guard safety ranking (highest to lowest safety):**

1. C-hybrid (internal graph, no MCP surface) -- recommended
2. A (status quo)
3. B (L3 cache tier for CallGraph)
4. C-full (with MCP Resources surface)

---

## Should Agents Commit a Knowledge Graph to the Repository?

No. The industry consensus is unambiguous: treat code KGs as regenerable, gitignored local
indices. The `APTU_CODER_DISK_CACHE_DIR` pattern already in place for L2 analysis results is
the correct model. A `.aptu-kg/` project-local directory is technically possible (following the
`.aptu/filters.toml` precedent for `exec_command`) but would produce unreadable binary diffs for
a postcard-encoded graph. A canonically-sorted JSONL export is acceptable as an opt-in on a
future C-full issue but is not part of C-hybrid scope.

---

## MCP 2026-07-28 Alignment

*Table 2: MCP 2026-07-28 features relevant to a future knowledge graph.*

| Feature | SEP | Relevance | Status in rmcp 3.1.0 |
|---|---|---|---|
| `structuredContent` unrestricted JSON | SEP-2322 | Graph node/edge payloads can be any JSON value; tool results can return raw graph slices | Available |
| `ttlMs` + `cacheScope` on Resources | SEP-2549 | Long TTL for a KG that only changes on rebuild | rmcp API pending |
| `subscriptions/listen` | SEP-2549 | `resourceUpdated` after edit operations invalidate graph; agents skip polling | rmcp API pending |
| Resources (`list_resources`, `read_resource`) | pre-spec | Expose KG as a queryable MCP resource | Available (rmcp 3.1.0) |
| Extensions framework | SEP-2133 | Not required; `structuredContent` is sufficient | Available |
| MRTR | SEP-2322 | Not the right mechanism for multi-hop traversal; cursor pagination is correct | Available |
| `server/discover` | SEP-2575 | No KG-specific concern | BLOCKED (C04 in 2026-08-01 audit) |

The 2026-07-28 stateless protocol shift (SEP-2567/SEP-2575, no `Mcp-Session-Id`) directly
favors an on-disk KG queryable per-call over any in-memory session-scoped graph.

---

## Storage Format

*Table 3: Format evaluation for a persisted code knowledge graph.*

| Format | Diff-friendly | Query perf | Ecosystem | Agent usability | Verdict |
|---|---|---|---|---|---|
| postcard binary | Very poor | Excellent | Rust-first (serde) | Low (opaque) | Best local cache; matches aptu#1432 |
| SQLite | Very poor | Excellent (sub-ms at 2.1M nodes) | Excellent (rusqlite) | High (SQL via exec_command) | Strong alternative if SQL queryability needed |
| JSONL | Good (per-line diffs) | None built-in | Universal (jq) | High | Best for opt-in committable export |
| Flat files + index | Excellent | Poor (N reads) | None (bespoke) | Moderate | Impractical at scale |
| RDF/JSON-LD | Poor | Requires triplestore | Semantic Web only | Low | No precedent in code-KG literature |
| RocksDB/LMDB | Very poor | Excellent (KV) | Moderate | Low | Worse than SQLite on all axes |

**Decision for C-hybrid:** postcard-encoded `petgraph` graph for the local on-disk cache
(following aptu#1432; no_std-compatible, smallest binary, no server process). SQLite remains
the alternative if full SQL queryability from outside the MCP server becomes a requirement.

---

## Open Issues

| Issue | Relation to This Audit |
|---|---|
| [aptu#1408](https://github.com/clouatre-labs/aptu/issues/1408) | Closed design issue. Established no-second-parse-pass constraint; aptu-coder-core must honor it. |
| [aptu#1420](https://github.com/clouatre-labs/aptu/pull/1420) | Merged 2026-08-01. Reference schema for Node/Edge types; postcard migration in aptu#1432. |
| [aptu#1432](https://github.com/clouatre-labs/aptu/pull/1432) | Merged 2026-08-03. postcard replaces bincode; follow for serialization format. |
| [#998](https://github.com/clouatre-labs/aptu-coder/issues/998) | MCP 2026-07-28 migration tracking. C-full Resources wiring uses ttlMs/cacheScope from SEP-2549; defer until rmcp exposes the API. |
| [#1361](https://github.com/clouatre-labs/aptu-coder/issues/1361) | fix(cache): pagination chains lost on analyze_symbol L2 cache reload (KG1). `#[serde(skip)]` on `FocusedAnalysisOutput.prod_chains`, `.test_chains`, `.outgoing_chains` breaks cross-session pagination. Orthogonal to the KG question; filed separately. |
| [#1362](https://github.com/clouatre-labs/aptu-coder/issues/1362) | feat(kg): structural knowledge graph module in aptu-coder-core (C-hybrid, KG7). petgraph DiGraph in aptu-coder-core/src/graph/, internal use only, postcard persistence. |
| [#1363](https://github.com/clouatre-labs/aptu-coder/issues/1363) | feat(kg): MCP Resource surface for knowledge graph, C-full design issue (KG8). Resources surface, ttlMs/cacheScope, subscriptions/listen invalidation, optional JSONL export. |

---

## Summary

*Table 4: Findings.*

| ID | Severity | Category | Finding |
|---|---|---|---|
| KG1 | High | GAP | `analyze_symbol` L2 disk cache exists (`symbol_focused.rs:313-316`) but cross-session pagination is broken: `prod_chains`, `test_chains`, `outgoing_chains` are `#[serde(skip)]` on `FocusedAnalysisOutput` and lost on reload |
| KG2 | Medium | GAP | No persistent structural graph (nodes + typed edges) anywhere in aptu-coder-core; all cross-file relationship data is ephemeral |
| KG3 | Info | CONTEXT | aptu#1420 (merged 2026-08-01) implements a petgraph-backed graph in the downstream aptu repo, not here; schema and postcard serialization pattern are directly reusable |
| KG4 | Info | DECISION | Committing a raw graph artifact to repos is not recommended (industry consensus); APTU_CODER_DISK_CACHE_DIR gitignored pattern is correct; opt-in JSONL export is acceptable for a future C-full issue |
| KG5 | Info | DECISION | Approach B (L3 cache tier) is rejected: solves 0.2%-of-calls non-hotspot, serde work does not transfer to C, creates rework |
| KG6 | Info | DECISION | MCP Resources are available in rmcp 3.1.0 and not blocked; server/discover (SEP-2575) is the blocked item, not Resources |
| KG7 | Info | RECOMMENDATION | Implement C-hybrid: petgraph DiGraph in aptu-coder-core/src/graph/, internal use, postcard persistence; fix pagination L2 bug separately |
| KG8 | Info | RECOMMENDATION | File C-full (MCP Resources surface, ttlMs/cacheScope, subscriptions/listen) as a follow-on design issue once C-hybrid ships |

**Recommended action order:**

1. **Fix KG1 (pagination L2 bug) -- #1361** -- remove `#[serde(skip)]` from `FocusedAnalysisOutput`
   chain fields or redesign the L2 serialization so cross-session pagination works. Small scope,
   high correctness value.

2. **Implement C-hybrid -- #1362** -- new `crates/aptu-coder-core/src/graph/` module; petgraph
   `DiGraph<Node, Edge>`; built from existing `analyze.rs` output (no second parse pass, per
   aptu#1408 constraint); postcard-encoded persistence in `APTU_CODER_DISK_CACHE_DIR` following
   aptu#1432; used internally to accelerate `analyze_symbol` and enable multi-hop traversal.
   Reference: Codebase-Memory (arXiv:2603.27277) for production-scale architecture validation.

3. **File C-full as a design issue -- #1363** -- MCP Resources surface (`list_resources` / `read_resource`
   usable today in rmcp 3.1.0), `ttlMs`/`cacheScope` once rmcp exposes the SEP-2549 API,
   `subscriptions/listen`-based invalidation after edit operations, optional JSONL export for
   opt-in committable artifacts.

4. **Do not implement Approach B** as a standalone step.
