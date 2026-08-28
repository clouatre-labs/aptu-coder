# Audit: Knowledge Graph Parity Verification and Adoption Guide

> **Status: Superseded.** Recommendations implemented across PRs #1473, #1476, #1478, and #1481 in aptu-coder and PRs #1544, #1551, #1553, and #1554 in aptu.

Date: 2026-08-26
Commits: aptu-coder `9a54143` (origin/main), aptu `d5e74e2` (origin/main)
Toolchain: Rust 1.98.0, petgraph 0.8.3

## Purpose

Direct response to aptu's audit
[`docs/audit/2026-08-26-graph-module-consolidation-reassessment.md`](https://github.com/clouatre-labs/aptu/blob/main/docs/audit/2026-08-26-graph-module-consolidation-reassessment.md)
(written against aptu-coder commit `f2ce662`), which reassessed PR #1512's decision to keep
aptu's `aptu-core/src/graph/` module separate from `aptu_coder_core::graph::StructuralGraph`.
That audit found aptu's real ontology already equal to `StructuralGraph`'s, and left two
concrete gaps open (its F3) plus a dead-code finding (its F2) on aptu's own side. This audit:

1. Verifies, against current code in both repos, that aptu-coder-core closed both F3 gaps —
   and confirms whether aptu's own follow-up (merged after the reassessment audit, up to and
   including `d5e74e2`) already closes its F2 independently.
2. Documents what wiring `StructuralGraph` into a consumer crate requires, as a reusable
   adoption reference for others building similar code-knowledge-graph features on top of
   aptu-coder-core.

This is a research-only audit. No source was modified in either repository.

## Scope

- aptu-coder: `crates/aptu-coder-core/src/graph/{structural,call_graph,store}.rs`,
  `crates/aptu-coder-core/src/analyze.rs` (`FileAnalysisOutput`).
- aptu: `crates/aptu-core/src/graph/{mod,builder,query,cache}.rs`,
  `crates/aptu-core/src/config/graph.rs`, `crates/aptu-core/Cargo.toml`.
- Method: direct code inspection, `git show`/`git log` across both repos (both fetched fresh
  at audit time), and running the parity test aptu added in `d5e74e2`.

---

## Findings

### F1: aptu-coder-core closed both F3 gaps from the reassessment audit (RESOLVED)

**Severity:** N/A (verification)

The reassessment audit's F3 left two input-shape gaps open against aptu-coder-core commit
`f2ce662`. Both are closed as of `9a54143`:

- **File-path convention.** `structural.rs:77` previously derived a file's path via
  `entry.formatted.lines().next()`. Commit `c4fc014` (#1459, released in aptu-coder-core
  v0.31.0) added an explicit `path: String` field to `FileAnalysisOutput`
  (`analyze.rs:122`), updated `build_from_analysis` to read `entry.path` directly
  (`structural.rs:167`), updated all 14 construction sites across 7 files, and added a
  regression test (`test_build_uses_explicit_path_field`, `structural.rs:615-644`) that
  asserts the field wins even when `formatted` contains contradictory data.
- **Call-edge parity.** Previously "unverified" per the reassessment audit. aptu closed
  this itself in `d5e74e2` (#1526, merged after the reassessment audit) by adding
  `test_calls_parity_with_structural_graph` (`aptu/crates/aptu-core/src/graph/builder.rs:313`),
  which feeds one real `analyze_str` fixture through both builders and asserts identical
  `Calls` edge sets. Re-run at audit time:

  ```
  test graph::builder::tests::test_calls_parity_with_structural_graph ... ok
  test result: ok. 1 passed; 0 failed
  ```

  The test passes because of *structural* agreement, not matched filter logic:
  `StructuralGraph::build_from_analysis` builds `Calls` edges solely from
  `entry.semantic.calls` and never reads `SemanticAnalysis.references` or `.impl_traits` —
  exactly the fields aptu's `CallGraph::build_from_results` uses to synthesize the
  `<reference>` pseudo-edges and impl-trait edges that `builder.rs:88-96` must filter out.
  The two builders agree because `StructuralGraph` never ingests the synthetic edge kinds
  in the first place, not because someone kept two filter implementations in sync.

**Caveat:** the parity test covers one file, one real call, plus hand-built synthetic
noise edges. It does not exercise cross-file symbol name collisions (two files each
defining a function with the same name) — see F2.

### F2: aptu-coder-core's cross-file `Calls` disambiguation is strictly more capable than what the parity test covers, and is not yet in a published release (INFO)

**Severity:** Info

Independent of the parity test, aptu-coder commit `43812d7` (#1461/#1463) added real
cross-file symbol disambiguation to `StructuralGraph::build_from_analysis`
(`structural.rs`): a symbol index keyed by name now tracks all candidate definitions
(`HashMap<String, Vec<NodeIndex>>`), and `resolve_candidate()` disambiguates via staged
heuristics — same-file preference, then line-proximity, then arg-count match, then
first-definition fallback — applied to both caller and callee resolution. Five new tests
were added, including `test_build_no_cross_file_collision`, which asserts that two files
each defining `main -> helper` produce two edges, neither crossing files.

aptu's own `builder.rs:88-96` has no equivalent: it filters `<reference>`/`is_impl_trait`
markers out of `CallGraph.callers`, but resolution beyond that is name-only, so two
same-named functions in different files can still produce a cross-file edge. On this
specific axis, `StructuralGraph` is more precise than aptu's current builder, not merely
at parity.

**Release status:** `43812d7`, along with `ec3598a` (#1464, see F3), is merged to
aptu-coder's `origin/main` but **is not in a published release**. aptu-coder's latest tag
is `v0.31.0` (tagged 2026-08-26 23:18 UTC); both commits land after that tag, and the
workspace version on `main` (`0.32.0`) has not been cut. aptu's `Cargo.toml` pins
`aptu-coder-core = "0.31.0"` and `Cargo.lock` resolves exactly that from crates.io — so
aptu does not yet receive the cross-file disambiguation improvement. This is not a defect
in either repo; it is a normal release-boundary gap, recorded here so it isn't mistaken
for an unaddressed finding.

### F3: A second aptu-coder-core fix (redundant rebuild collapse) is also unreleased, with one documented trade-off (INFO)

**Severity:** Info

Commit `ec3598a` (#1464) removed a redundant second edge-resolution pass on the
`analyze_focused` cache-miss path: `CallGraph::build_from_results` and
`StructuralGraph::build_from_analysis` were both independently re-deriving `Calls` edges
from the same `entry.semantic.calls`. The new `StructuralGraph::from_call_graph()`
(`structural.rs:266-311`) reuses the already-resolved `call_graph.callees` map instead.
The unconditional `analysis_results.clone()` that previously ran before the cache-tier
check was also deferred until after it, so L1/L2 cache hits skip the clone.

Two comparative benchmarks were added (`benches/analysis.rs:407-429`); no dedicated unit
test guards the fast path's output against the full-rebuild path. The code comments a
known trade-off: `from_call_graph`'s fast path drops the arg-count tie-break stage present
in the full `resolve_candidate()` pipeline (`CallEdge` doesn't carry arg count), so a
cache-miss-driven graph could in principle diverge from a from-scratch build on highly
ambiguous symbols. Same release-boundary note as F2 applies: unreleased, aptu does not yet
consume this.

### F4: aptu closed its own F1/F2 (dead ontology, stale doc comment) independently, with one residual doc-comment gap (RESOLVED, minor residual)

**Severity:** Low (residual only)

Unprompted by this audit, aptu merged its own fixes for the reassessment audit's F1/F2
before `d5e74e2`:

- `4aac720` (#1523) corrected the false ontology-divergence doc comment in
  `aptu-core/src/graph/mod.rs`.
- `8be8746` (#1524) removed all 7 dead `Node`/`Edge` variants
  (`Struct`/`Enum`/`Trait`/`Impl`/`Implements`/`HasMethod`/`Tests`) and updated
  `cache.rs`'s schema string accordingly (now `File|Module|Function|Contains|Calls|Imports`).
  `Edge::Modifies`, flagged by the reassessment audit as needing its own keep-or-remove
  decision, was removed in the same commit rather than kept.

`mod.rs`'s doc comment (lines 20-68 at audit time) now accurately states current emissions
and reaffirms #1510's "keep separate" decision, citing ontology equality, workload
precedent, and the now-closed parity blockers — tracked under #1520/#1521/#1523/#1524/#1525.

**Residual:** `query.rs`'s doc comment (around lines 134-136) still says rendering can
produce "Struct, Enum, Trait, and Impl nodes" — those variants no longer exist after
`8be8746`; only `File`/`Module`/`Function` nodes render (`query.rs:167-169`). Cosmetic,
does not affect behavior. Not filed as an issue here since it is entirely within aptu's
repository.

---

## Section: What it would take to wire aptu's own graph module to `StructuralGraph`

Not prescribed — aptu's `mod.rs` doc comment reaffirms the keep-separate decision as of
`d5e74e2`, for reasons independent of this audit's findings (workload precedent, release
topology). Recorded here as the concrete scope, factually, in case that decision is
revisited later.

Given F1 (ontology now equal) and F2/F3 (StructuralGraph's call-edge resolution is at
parity or better, once aptu bumps past `0.31.0`), the remaining work to replace aptu's
`builder.rs`/`query.rs`/`cache.rs` with direct `StructuralGraph` consumption is:

1. **Bump the dependency pin** past whatever release contains `43812d7`/`ec3598a` (not yet
   cut; currently unreleased on aptu-coder's `main` as `0.32.0`-in-progress).
2. **Replace `CallGraph`-based input with `FileAnalysisOutput`-based input.** aptu's
   `build_from_analysis` (`aptu-core/src/graph/builder.rs`) takes `&SemanticAnalysis` plus
   a pre-built `CallGraph`; `StructuralGraph::build_from_analysis` takes
   `&[FileAnalysisOutput]` directly. aptu would need to either analyze via
   `aptu_coder_core::analyze_file`/`analyze_str` directly (bypassing its own
   `SemanticAnalysis`-only path) or keep a thin adapter that assembles
   `FileAnalysisOutput` values from data it already has.
3. **Re-verify `blast_radius()` output does not regress**, per #1510's own acceptance
   criterion — `StructuralGraph::blast_radius_subgraph(symbol, depth)` is the equivalent of
   aptu's own BFS query surface (`query.rs`) and would need a parity test analogous to
   `test_calls_parity_with_structural_graph`, extended to blast-radius output rather than
   just edge sets.
4. **Decide what to do with aptu's narrower ontology intentionally being a subset.** aptu's
   builder only ever emits `File`/`Function`/`Module` + `Contains`/`Imports`/`Calls` — a
   strict subset of `StructuralGraph`'s (which also carries `Symbol` variants for classes).
   Consuming `StructuralGraph` directly means aptu's consumers would see `Symbol` nodes for
   classes they don't currently produce; call sites iterating `Node::Function` pattern
   matches (as aptu's builder tests do) would need to switch to `Node::Symbol` with a
   `SymbolKind` check.
5. **Drop `aptu-core/src/graph/{builder,query,cache}.rs`** (~1300 LoC) and adopt
   `GraphDiskStore` (`aptu-coder-core/src/graph/store.rs`) in place of aptu's own
   mtime-keyed `CallGraphCacheKey` cache — a strict improvement, since `GraphDiskStore`
   already keys on blake3 content hashes rather than mtime (an aptu-coder-core issue,
   #1453, that aptu's own cache does not currently benefit from).

None of this is committed work; it is the factual scope if the keep-separate decision is
revisited after aptu-coder cuts the release containing `43812d7`/`ec3598a`.

---

## Section: Adoption guide — wiring aptu-coder-core's KG into a new consumer crate

General reference, using aptu's own integration as the worked example, for other crates
that want a code-knowledge-graph feature without reimplementing one.

### 1. Add the dependency and feature flags

```toml
[dependencies]
aptu-coder-core = { version = "0.31.0", optional = true }
petgraph = { version = "0.8", features = ["serde-1"], optional = true }
postcard = { version = "1", default-features = false, features = ["alloc"], optional = true }

[features]
graph = ["dep:petgraph", "dep:postcard"]
ast-context = ["dep:aptu-coder-core"]
```

aptu gates its graph module on `all(feature = "ast-context", feature = "graph")`
(`aptu-core/src/graph/builder.rs`) — `ast-context` pulls in aptu-coder-core itself,
`graph` pulls in the serialization dependencies needed for `GraphDiskStore`. Keep them
separate if a consumer might want `ast-context` (symbol lookups, no graph) without the
graph machinery.

### 2. Produce `FileAnalysisOutput` entries

```rust
use aptu_coder_core::analyze::{analyze_str, analyze_file};

let output = analyze_str(source_code, "rust", None)?;   // in-memory
let output = analyze_file("src/lib.rs", None)?;          // on disk
```

Each call returns one `FileAnalysisOutput` (`analyze.rs:111`), carrying `path: String`
(explicit since v0.31.0), `semantic: SemanticAnalysis` (`functions`, `classes`, `imports`,
`calls`), `line_count`, and `formatted`. Collect one per file into a `Vec`.

### 3. Build the graph

```rust
use aptu_coder_core::graph::StructuralGraph;

let entries: Vec<FileAnalysisOutput> = /* from step 2 */;
let graph = StructuralGraph::build_from_analysis(&entries);
```

Internally this builds `File`/`Symbol`/`Module` nodes, then resolves `Calls` edges via
the staged disambiguation described in F2 above (same-file, then line-proximity, then
arg-count, then first-definition fallback).

### 4. Query it

- `graph.blast_radius_subgraph(symbol, depth)` — bounded-depth BFS returning both nodes
  and edges, for impact-analysis-style queries.
- `graph.graph` — the underlying `petgraph::DiGraph<Node, Edge>`, for custom traversals.
  `Node` variants: `File`, `Symbol { name, kind, file_path, line }`, `Module`. `Edge`
  variants: `Contains`, `Calls`, `Imports`.

### 5. Persist it (optional)

```rust
use aptu_coder_core::graph::GraphDiskStore;

let store = GraphDiskStore::new(cache_dir);                  // default cap 512 MB
let key = GraphDiskStore::cache_key(repo_root, &file_hashes); // blake3 content hashes, not mtime
store.put(&key, &graph);
let cached: Option<StructuralGraph> = store.get(&key);
```

`GraphDiskStore` handles postcard encoding, a `FORMAT_VERSION` header for schema-change
invalidation, per-shard `fs2` locking, atomic writes via `NamedTempFile::persist`, and
LRU eviction by mtime once the size cap is exceeded.

### 6. Gate at runtime

aptu's own pattern (`aptu-core/src/config/graph.rs`): a `GraphConfig` struct with
`enabled: bool` defaulting to `false`, toggled via `[graph] enabled = true` in the
consumer's own config file — not on by default even though the feature is compiled in.

### When not to consume `StructuralGraph` directly

aptu deliberately did not take this path for its PR-review call-graph-diffing workload,
per its own `graph/mod.rs` doc comment: it needed a narrower, request-scoped
`CallGraph`-based representation with its own symbol-matching modes, built from raw
`SemanticAnalysis` rather than `FileAnalysisOutput`, and filtered specifically for its
own PR-diff use case. If a consumer's workload is closer to IDE-style "what calls this /
what does this call" navigation across a whole repository, `StructuralGraph` as described
above is the direct fit; if it's closer to diffing call relationships between two revisions
of a small change, a purpose-built representation (as aptu built) may still be warranted —
this is a workload-shape decision, not a completeness gap in `StructuralGraph`.

---

## Summary

*Table 1: Findings.*

| ID | Severity | Category | Finding | Status |
|---|---|---|---|---|
| F1 | — | Verification | Both F3 gaps from aptu's reassessment audit (file-path convention, call-edge parity) are closed | RESOLVED |
| F2 | Info | Positive | Cross-file `Calls` disambiguation makes `StructuralGraph` more precise than aptu's current builder on same-named-different-file collisions; not yet in a published release | INFO |
| F3 | Info | Positive | Redundant-rebuild collapse also unreleased; one documented trade-off (fast path drops arg-count tie-break) | INFO |
| F4 | Low | Doc debt | aptu independently closed its own F1/F2 (dead ontology, stale doc comment); one residual stale doc line in `query.rs` | RESOLVED (residual cosmetic) |

*Table 2: Recommendations.*

| ID | Priority | Fixes | Recommendation |
|---|---|---|---|
| R1 | Low | F4 residual | Correct `aptu/crates/aptu-core/src/graph/query.rs`'s doc comment to drop the reference to removed Struct/Enum/Trait/Impl node rendering. Entirely within aptu's repository. |
| R2 | Info | F2/F3 | When aptu-coder cuts the release containing `43812d7`/`ec3598a`, bump aptu's `aptu-coder-core` pin to pick up the cross-file disambiguation and rebuild-collapse fixes. |
| R3 | Info | — | If `43812d7`'s fast-path trade-off (F3) matters for correctness-sensitive callers, add a regression test asserting `from_call_graph()` output matches `build_from_analysis()` on an ambiguous-symbol fixture, mirroring aptu's own `test_calls_parity_with_structural_graph`. |

## Conclusion

On every point raised in aptu's own 2026-08-26 reassessment audit, aptu-coder-core's
`StructuralGraph` is at parity or ahead: the ontology gap was already false (per that
audit's own F1), the two real input-shape gaps it identified are both closed, and one of
aptu-coder's fixes (cross-file disambiguation) is strictly more precise than aptu's current
filtering-only approach on cross-file name collisions. The only asymmetry is a release
boundary — two of the three aptu-coder fixes are merged but unreleased, so aptu's pinned
`0.31.0` doesn't yet carry them. aptu, independently, already closed the matching findings
on its own side (dead ontology, stale doc comment) before this audit was written.
