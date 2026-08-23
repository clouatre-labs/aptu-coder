# Audit: Performance and Token Efficiency, August 2026 (Re-run)

Date: 2026-08-23
Commit: 175f0ad
Version: v0.29.2
Toolchain: Rust / rmcp 3.1.4 / tokio async

## Purpose

Re-run of [2026-06-performance-token-efficiency.md](2026-06-performance-token-efficiency.md) to
measure how `aptu-coder` has evolved since June and to find further optimization opportunities.
All eight tracked findings from that audit (F1-F8, issues #1039-#1046) are closed. Since then the
project shipped MCP 2026-07-28 spec alignment, a structural knowledge-graph module with an MCP
Resource surface, cursor-aware `list_resources`/`list_resource_templates` pagination, and an L2
disk-cache fix for `FocusedAnalysisOutput` chain fields -- none of which had a token/latency
baseline before this audit.

This is a research-only audit. No code was modified.

## Scope

- Current branch base: `origin/main`, fetched 2026-08-23.
- Current MCP surface: `analyze_directory`, `analyze_file`, `analyze_module`, `analyze_symbol`,
  `edit_overwrite`, `edit_replace`, `exec_command`, plus `list_resources`, `read_resource`,
  `list_resource_templates` (new since June).
- Prior findings F1-F8 from the June audit, re-verified against current source.
- New scope: `crates/aptu-coder-core/src/graph/structural.rs` and
  `crates/aptu-coder/src/tools/resources.rs`, never previously audited.
- Local metrics corpus from `~/.local/share/aptu-coder/metrics-2026-08-23.jsonl`.

## Methodology

Two delegates in sequence: (1) a scout re-running the June methodology -- fresh metrics
snapshot, live tool calls to confirm F1-F8 behavior, source reads for new surface area; (2) an
adversarial guard cross-checking every scout claim against source and metrics, in the same
pattern used by the 2026-08-03 knowledge-graph audit. The guard corrected three material errors
in the scout's draft, folded into this document:

1. The scout compared an August corpus of 2,360 calls (single recent session) against June's
   66,598 calls (multi-day production use) and reported the deltas as straight percentage
   improvements. Given the ~3.5% sample size and different workload composition, most headline
   latency percentages are not statistically defensible and are caveated or dropped below.
2. The scout reported August cache hit rates for `analyze_file` (52.8%) and `analyze_module`
   (40.0%) as improvements; they are lower than June's (69.6%, 55.9%) and are declines, not gains.
3. The scout searched for a "knowledge-graph module," found nothing under that literal name, and
   concluded it did not exist -- misattributing it to the F5 `CallGraph` cache. The module exists
   at `crates/aptu-coder-core/src/graph/structural.rs` (284 lines) with an MCP resource handler at
   `crates/aptu-coder/src/tools/resources.rs` (419 lines); neither had been examined. Both are
   audited below (G1, G2).

## Metrics Snapshot

*Table 1: August 2026 tool metrics (post-call events, `duration_ms > 0`).*

| Tool | Calls | p50 ms | p95 ms | p99 ms | p95 chars | Cache hit rate | Truncated |
|---|---:|---:|---:|---:|---:|---:|---:|
| `exec_command` | 2,011 | 30 | 2,130 | 26,856 | 10,148 | 0.0% | 4.4% |
| `edit_replace` | 148 | 1 | 7 | 10 | 148 | n/a | 0.0% |
| `edit_overwrite` | 89 | 1 | 2 | 2 | 136 | n/a | 0.0% |
| `analyze_file` | 53 | 4 | 384 | 394 | 2,366 | 52.8% | 0.0% |
| `analyze_module` | 40 | 7 | 392 | 398 | 906 | 40.0% | 0.0% |
| `analyze_directory` | 18 | 35 | 373 | 373 | 1,237 | 16.7% | 0.0% |
| `analyze_symbol` | 1 | 67 | 67 | 67 | 0 | 0.0% | 0.0% |

*Table 2: June 2026 tool metrics, reproduced from the prior audit for reference.*

| Tool | Calls | p50 ms | p95 ms | p99 ms | p95 chars | Cache hit rate | Truncated |
|---|---:|---:|---:|---:|---:|---:|---:|
| `exec_command` | 56,589 | 95 | 3,862 | 32,699 | 7,851 | 0.0% | 62.56% |
| `edit_replace` | 6,153 | 2 | 5 | 7 | 165 | n/a | 0.0% |
| `edit_overwrite` | 1,458 | 1 | 3 | 5 | 132 | n/a | 0.0% |
| `analyze_file` | 1,312 | 3 | 457 | 520 | 7,536 | 69.61% | 0.0% |
| `analyze_directory` | 574 | 115 | 1,028 | 1,047 | 1,758 | 30.31% | 0.0% |
| `analyze_module` | 269 | 5 | 452 | 514 | 6,442 | 55.92% | 0.0% |
| `analyze_symbol` | 39 | 208 | 534 | 620 | 796 | 0.0% | 0.0% |

### Comparison validity

August's corpus is a single recent session (2,360 calls); June's is multi-day accumulated
production use (66,598 calls) -- roughly 3.5% the sample size, with different workload
composition. Percentile statistics on the August side (especially `analyze_directory` and
`analyze_symbol`, n=18 and n=1) are not reliable enough to support point comparisons.

*Table 3: Validity of each apparent before/after delta.*

| Metric | June | August | Verdict |
|---|---:|---:|---|
| `exec_command` p50 | 95 ms | 30 ms | Misleading -- workload composition differs, do not cite as 3.2x |
| `exec_command` p95 | 3,862 ms | 2,130 ms | Caveated -- directionally plausible, not confirmed |
| `exec_command` truncation | 62.56% | 4.4% | Defensible -- F7 filter-attribution fix gives a mechanism, not just a number |
| `analyze_directory` p50/p95 | 115 / 1,028 ms | 35 / 373 ms | Drop -- n=18, not usable for percentile claims |
| `analyze_file` cache hit rate | 69.61% | 52.8% | Decline, not improvement |
| `analyze_module` cache hit rate | 55.92% | 40.0% | Decline, not improvement |
| `analyze_symbol` cache hit rate | 0.0% | 0.0% (n=1) | Consistent, but neither sample exercises the cache |

The only metric defensible as a real improvement across both corpora is the truncation drop,
because it is backed by a source-level mechanism (F7's `filter_applied` attribution), not just a
sample-to-sample percentage. Everything else needs a controlled, fixed-workload benchmark before
being cited as a measured gain.

## Summary

*Table 4: Prior findings and new findings.*

| ID | Severity | Type | Finding | Issue | Status |
|---|---|---|---|---|---|
| F1 | High | Bug | `exec_command` cache scaffolding documented but inactive | [#1039](https://github.com/clouatre-labs/aptu-coder/issues/1039) | Closed (#1049) |
| F2 | High | Refactor | `analyze_module` bypassed the module fast path | [#1046](https://github.com/clouatre-labs/aptu-coder/issues/1046) | Closed (#1051) |
| F3 | High | Refactor | Shallow `analyze_directory` performed unbounded walk | [#1044](https://github.com/clouatre-labs/aptu-coder/issues/1044) | Closed (#1051) |
| F4 | Medium | Refactor | Directory analysis read eligible files twice | [#1041](https://github.com/clouatre-labs/aptu-coder/issues/1041) | Closed (#1050) |
| F5 | Medium | Feature | `analyze_symbol` had no reusable cache | [#1040](https://github.com/clouatre-labs/aptu-coder/issues/1040) | Closed (#1052) |
| F6 | High | Feature | `analyze_file(fields=...)` didn't project structured output | [#1045](https://github.com/clouatre-labs/aptu-coder/issues/1045) | Closed (#1053) |
| F7 | Medium | Feature | JSONL metrics couldn't show which exec filter fired | [#1042](https://github.com/clouatre-labs/aptu-coder/issues/1042) | Closed (#1050) |
| F8 | Low | Refactor | Tool guidance needed a token-efficiency pass | [#1043](https://github.com/clouatre-labs/aptu-coder/issues/1043) | Closed (#1387) |
| G1 | Medium | Refactor | `StructuralGraph::build_from_analysis` has no cache check before rebuilding | [#1406](https://github.com/clouatre-labs/aptu-coder/issues/1406) | Open |
| G2 | Low | Refactor | Graph resource handler materializes full node set before paginating | [#1407](https://github.com/clouatre-labs/aptu-coder/issues/1407) | Open |

## F1-F8: Verification Against Current Source

All eight fixes are confirmed present and correct in source, independent of the corpus caveats
above:

- **F1** -- `crates/aptu-coder/src/tools/exec_command.rs` has no cache lookup path; metrics report
  a consistent 0.0% hit rate; tool description no longer advertises caching.
- **F2** -- `analyze_module` routes through the lightweight `ModuleInfo` extraction path
  (functions + imports only), confirmed by direct tool call and source read.
- **F3** -- `crates/aptu-coder-core/src/traversal.rs`'s `walk_directory` takes `max_depth` into
  the `WalkBuilder` itself rather than filtering post-walk.
- **F4** -- `process_file_entry` in `analyze.rs` reads each eligible file exactly once.
- **F5** -- `CallGraphCache` (`crates/aptu-coder-core/src/cache.rs`) is a real LRU keyed on root
  path, git ref, follow depth, match mode, `impl_only`, and file mtimes. Neither corpus exercises
  repeated symbol lookups, so hit-rate effectiveness is unverified by metrics, but the
  implementation is unit-tested and present.
- **F6** -- `analyze_file.rs:261` calls `semantic.project(params.fields.as_deref())`, so
  `structuredContent` is filtered, not just the text payload.
- **F7** -- exec metrics events now carry `filter_applied`, giving filter-tuning visibility that
  didn't exist in June.
- **F8** -- tool descriptions were trimmed (#1387, merged 2026-08-10) while the
  `analyze_directory -> analyze_module -> analyze_file` routing guidance was preserved.

## New Findings

### G1: `StructuralGraph::build_from_analysis` rebuilds the graph on every call

**Severity:** Medium
**Type:** Optimization opportunity
**File:** `crates/aptu-coder-core/src/graph/structural.rs:50-105`

**Observed state:**

- `build_from_analysis` walks every `FileAnalysisOutput` entry and constructs a fresh
  `petgraph::DiGraph` of files, functions, classes, imports, and calls on each invocation.
- There is no cache lookup keyed on file-set identity before reconstruction, unlike the
  `CallGraphCache` (F5) or the L2 disk cache used by `analyze_symbol`.
- Unit tests (`test_build_happy_path`, `test_build_empty_input`, `test_build_dedup_edges`)
  construct a new graph each time; none exercise a cached path because none exists.

**Impact:** Repeated graph-resource requests over the same file set pay full reconstruction cost
each time. Currently the graph is only built when the MCP resource surface is accessed, so this
is not yet on the hot path most agents exercise -- but it is the same class of gap F5 closed for
`analyze_symbol`, and will matter as resource usage grows.

**Fix direction:** Add a cache keyed on a hash of sorted file paths + mtimes (matching the
`CallGraphCacheKey` pattern), backed by L1 in-memory and optionally L2 via the existing
`GraphDiskStore` (`crates/aptu-coder-core/src/graph/store.rs`).

**Acceptance criteria:**

- Cache hit/miss behavior for `build_from_analysis` covered by tests.
- Cache key includes file content or mtime so stale graphs are never served.
- `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` pass.

### G2: Graph resource handler materializes the full node set before paginating

**Severity:** Low
**Type:** Robustness
**File:** `crates/aptu-coder/src/tools/resources.rs:177-187, 255-295`

**Observed state:**

- `query_to_nodes` runs BFS traversal and collects every reachable node into a `Vec` before
  `read_resource_impl` applies `skip(offset).take(PAGE_SIZE)` (`PAGE_SIZE = 50`, line 23).
- Default BFS depth is 3 (`parse_graph_uri`, line 123), which bounds this in practice today.
- The final response payload is correctly paginated and cursor-encoded; the risk is intermediate
  memory use on unusually large or densely connected graphs, not token cost.

**Impact:** Low under current defaults. Becomes relevant only if callers are allowed to raise
depth significantly or the graph grows very large.

**Fix direction:** Either document and enforce a depth ceiling, or move to streaming/early-exit
pagination so the full reachable set is never materialized for deep traversals.

**Acceptance criteria:**

- Depth parameter has a documented and enforced upper bound, or pagination short-circuits BFS
  once `offset + PAGE_SIZE` nodes are found.
- A large-graph test case exists to catch regressions.

## Best Practices Affirmed

- The two-tier cache design (L1 in-memory, L2 disk) established by F5 and extended by the
  knowledge-graph work is sound and consistent across subsystems, except for the gap in G1.
- JSONL metrics now carry enough attribution (`filter_applied`, `cache_tier`,
  `output_truncated`) to drive this kind of audit without extra instrumentation.
- Bounded traversal (F3) and fast paths (F2) hold up under direct testing.

## Remaining Opportunities

1. Establish a controlled, fixed-workload benchmark for exec/analyze latency instead of comparing
   opportunistic metrics corpora -- the comparison-validity gaps in Table 3 will recur on every
   future re-run otherwise. Tracked as
   [#1408](https://github.com/clouatre-labs/aptu-coder/issues/1408): extend the existing
   `crates/aptu-coder-core/benches/analysis.rs` criterion harness with `CallGraphCache` and
   `StructuralGraph::build_from_analysis` benchmarks, in-process against the repo's own `src/`.
2. Close G1 ([#1406](https://github.com/clouatre-labs/aptu-coder/issues/1406), graph cache)
   before resource-surface usage grows past occasional PR-review queries.
3. Validate F5's `CallGraph` cache effectiveness under a workload that actually repeats symbol
   lookups; neither corpus sampled here does. Covered by the same benchmark work in
   [#1408](https://github.com/clouatre-labs/aptu-coder/issues/1408).
4. Revisit G2 ([#1407](https://github.com/clouatre-labs/aptu-coder/issues/1407)) only if depth
   limits are relaxed or graph size grows materially.
