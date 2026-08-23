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
- Local metrics corpus: all 11 files under `~/.local/share/aptu-coder/` within the 30-day
  retention window, `metrics-2026-08-10.jsonl` through `metrics-2026-08-23.jsonl` (10,313 events),
  aggregated via `scripts/mcp-metrics.py`.

## Methodology

Three passes. (1) A scout delegate re-ran the June methodology -- metrics snapshot, live tool
calls to confirm F1-F8 behavior, source reads for new surface area. (2) An adversarial guard
cross-checked the scout's claims against source and metrics, in the pattern used by the
2026-08-03 knowledge-graph audit, and corrected a mislabeled cache-hit-rate comparison and an
entirely missed module (now G1, G2 below). (3) User review of the resulting draft caught a
methodology defect neither delegate had: both had scoped the "August" metrics snapshot to only
`metrics-2026-08-23.jsonl` (2,360 calls, one day) and compared it against June's full 30-day
corpus (66,598 calls), then reasoned about statistical validity from that mismatched premise.

The correct fix is not a caveat -- it's rerunning the snapshot against every file in the
retention window, matching how the June audit itself was produced (`git log` shows June's number
was also a multi-day aggregate). That correction is what appears below. Three effects fell out of
it:

1. Real sample sizes are far larger than the flawed draft reported (e.g. `exec_command`: 8,540
   calls, not 2,011), so most before/after comparisons are now directly usable instead of caveated.
2. The `analyze_file`/`analyze_module`/`analyze_directory` cache-hit-rate decline the guard flagged
   as "not a sampling artifact" turned out to be correct for the wrong reason -- it isn't a
   sampling artifact, and it isn't just "different workload" either. Aggregating the full window
   surfaces a real, source-level cause for at least part of it (G3, below).
3. The single-day file itself was contaminated by this very audit's own tool calls: the scout and
   guard delegates repeatedly called `analyze_file`/`analyze_module` on the same handful of source
   files (`structural.rs`, `resources.rs`, `cache.rs`) while writing this document, which inflates
   same-day cache-hit rate in a way that has nothing to do with steady-state usage. The prior
   June audit had the same theoretical exposure, but at 66,598 calls spanning multiple days, one
   session's self-referential traffic is diluted to noise. Anyone re-running this audit should
   aggregate the full retention window for exactly this reason, not just for sample size.

## Metrics Snapshot

*Table 1: August 2026 tool metrics, aggregated across the full retention window
(`metrics-2026-08-10.jsonl` through `metrics-2026-08-23.jsonl`, 10,313 events).*

| Tool | Calls | p50 ms | p95 ms | p99 ms | p95 chars | Cache hit rate | Truncated |
|---|---:|---:|---:|---:|---:|---:|---:|
| `exec_command` | 8,540 | 0 | 1,399 | 6,468 | 6,974 | n/a (no cache) | 1.49% |
| `edit_replace` | 726 | 0 | 5 | 10 | 148 | n/a | 0.0% |
| `edit_overwrite` | 507 | 0 | 1 | 2 | 133 | n/a | 0.0% |
| `analyze_file` | 254 | 0 | 358 | 385 | 1,995 | 14.57% (37/254) | 0.0% |
| `analyze_module` | 169 | 0 | 362 | 393 | 913 | 11.24% (19/169) | 0.0% |
| `analyze_directory` | 113 | 0 | 383 | 413 | 1,324 | 3.54% (4/113) | 0.0% |
| `analyze_symbol` | 4 | 32 | 66 | 66 | 38 | 0.0% (0/4) | 0.0% |

Cache tier breakdown across all cacheable tools: 540 cacheable calls, 60 hits (10 L1-memory, 50
L2-disk), overall hit rate 11.1%. `exec_command` reliability over the same window: 98.64% success,
370 non-zero exits (4.33%), 1 timeout. The long tail is real, not corpus noise: 116 calls exceeded
60 seconds, including a handful near 180 seconds.

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

With the corpus corrected, August's sample sizes are smaller than June's for every tool except
`exec_command` (8,540 vs 56,589 -- August is larger here), but no longer the 3.5%-of-June sliver
the flawed draft reported. `analyze_symbol` (n=4) is still too small for percentile claims;
everything else is usable.

*Table 3: Validity of each before/after delta, corrected corpus.*

| Metric | June | August | Verdict |
|---|---:|---:|---|
| `exec_command` p50 | 95 ms | 0 ms | Real -- n=8,540, most calls now resolve near-instantly |
| `exec_command` p95 | 3,862 ms | 1,399 ms | Real -- 64% faster at n=8,540 |
| `exec_command` p99 | 32,699 ms | 6,468 ms | Real -- 80% faster; long tail still exists (116 calls >60s) |
| `exec_command` truncation | 62.56% | 1.49% | Real -- F7 filter-attribution fix gives a mechanism, not just a number |
| `analyze_directory` p95/p99 | 1,028 / 1,047 ms | 383 / 413 ms | Real -- n=113, 63%/61% faster |
| `analyze_file` p95/p99 | 457 / 520 ms | 358 / 385 ms | Real -- n=254, 22%/26% faster |
| `analyze_module` p95/p99 | 452 / 514 ms | 362 / 393 ms | Real -- n=169, 20%/24% faster |
| `analyze_symbol` p95/p99 | 534 / 620 ms | 66 / 66 ms | Directionally real, but n=4 -- treat as a lead, not a measurement |
| `analyze_file` cache hit rate | 69.61% | 14.57% | Real decline -- see G3 |
| `analyze_module` cache hit rate | 55.92% | 11.24% | Real decline -- see G3 |
| `analyze_directory` cache hit rate | 30.31% | 3.54% | Real decline -- root cause identified, see G3 |
| `analyze_symbol` cache hit rate | 0.0% | 0.0% (n=4) | Consistent; still no sample large enough to exercise the cache |

Latency improved across every tool with a usable sample. Cache hit rates fell across every
per-file and per-directory tool, and that decline is now investigated rather than caveated away
(G3).

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
| G3 | Medium | Bug | `analyze_directory` cache key invalidates on any unrelated file mtime change in the walked tree | [#1409](https://github.com/clouatre-labs/aptu-coder/issues/1409) | Open |

## F1-F8: Verification Against Current Source

All eight fixes are confirmed present and correct in source, independent of the corpus correction
above:

- **F1** -- `crates/aptu-coder/src/tools/exec_command.rs` has no cache lookup path; metrics report
  a consistent 0.0% hit rate across 8,540 calls; tool description no longer advertises caching.
- **F2** -- `analyze_module` routes through the lightweight `ModuleInfo` extraction path
  (functions + imports only), confirmed by direct tool call and source read.
- **F3** -- `crates/aptu-coder-core/src/traversal.rs`'s `walk_directory` takes `max_depth` into
  the `WalkBuilder` itself rather than filtering post-walk.
- **F4** -- `process_file_entry` in `analyze.rs` reads each eligible file exactly once.
- **F5** -- `CallGraphCache` (`crates/aptu-coder-core/src/cache.rs`) is a real LRU keyed on root
  path, git ref, follow depth, match mode, `impl_only`, and file mtimes. Even the corrected
  corpus only has 4 `analyze_symbol` calls, still too few to exercise repeated lookups; the
  implementation is unit-tested and present, but effectiveness stays unverified by metrics.
- **F6** -- `analyze_file.rs:261` calls `semantic.project(params.fields.as_deref())`. The
  corrected corpus shows this is exercised in practice, not just theoretically wired up:
  `fields_projected` is `true` on 28 of 254 `analyze_file` calls in the aggregated window.
- **F7** -- exec metrics events now carry `filter_applied`, giving filter-tuning visibility that
  didn't exist in June; truncation dropped from 62.56% to 1.49% at n=8,540.
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

### G3: `analyze_directory`'s cache key invalidates on any file mtime change in the walked tree

**Severity:** Medium
**Type:** Bug (cache design)
**File:** `crates/aptu-coder-core/src/cache.rs:73-111`

**Observed state:**

- `DirectoryCacheKey` (lines 73-79) is `{ files: Vec<(PathBuf, SystemTime)>, mode, max_depth,
  git_ref }`, where `files` is every file's path and mtime under the walked subtree, sorted and
  hashed as one unit.
- `handle_overview_mode` (`crates/aptu-coder/src/tools/analyze_directory.rs:56-61`) builds this
  key from the full `walk_directory` result before checking the cache.
- Consequence: touching *any* file anywhere in the walked subtree -- unrelated to what the caller
  actually asked about -- changes the key and invalidates the cached result for the whole
  directory, at any depth.
- `git_ref` in the key is caller-supplied (`params.git_ref`, defaults to `None`) for
  filtered-vs-unfiltered diffing; it is not automatically the repository's current commit, so it
  does not independently explain the decline.

**Impact:** In a repository under active, multi-file development -- exactly what the 11-day
August window captures, and exactly the environment this tool is meant to serve -- the cache
entry for `analyze_directory` is invalidated far more often than a caller's actual query would
warrant. This is the most concrete, source-confirmed explanation for `analyze_directory`'s hit
rate falling from 30.31% (June) to 3.54% (August, n=113). It plausibly also contributes to the
`analyze_file`/`analyze_module` declines to a lesser degree, since directory-level cache misses
cascade into re-analyzing the files underneath, but those two tools have their own per-file
`CacheKey { path, modified, mode }` (`cache.rs:65-71`) which should be far less sensitive to
unrelated churn -- their decline (69.61%->14.57%, 55.92%->11.24%) needs separate investigation
and is not fully explained by this finding alone.

**Fix direction:** Scope the cache key to what the query actually depends on, not the whole
walked subtree:

- For `max_depth`-bounded or `fields`-projected queries, key on only the files within scope of
  the response, not every file the walk happened to traverse.
- Alternatively, move to a directory-tree fingerprint that changes only when structurally
  relevant files change (e.g. hash of the file *list* separately from mtimes, so unrelated
  content edits within already-known files don't bust listing-level caches).

**Acceptance criteria:**

- Cache hit rate for `analyze_directory` improves under a benchmark that edits one file outside
  the query's effective scope between calls.
- No regression: a change to a file within scope still invalidates correctly.
- `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` pass.

**Open question (not yet a filed finding):** whether the `analyze_file`/`analyze_module`
hit-rate decline is a similar cache-design issue or simply reflects August's workload touching a
more diverse set of files across many unrelated tasks than June's corpus did. The per-file
`CacheKey` design looks sound on inspection; confirming this needs the controlled benchmark from
[#1408](https://github.com/clouatre-labs/aptu-coder/issues/1408) rather than more corpus reading.

## Best Practices Affirmed

- The two-tier cache design (L1 in-memory, L2 disk) established by F5 and extended by the
  knowledge-graph work is sound and consistent across subsystems, except for the gaps in G1 and
  G3.
- JSONL metrics now carry enough attribution (`filter_applied`, `cache_tier`, `fields_projected`,
  `output_truncated`) to drive this kind of audit without extra instrumentation -- provided the
  full retention window is aggregated, not a single day's file.
- Bounded traversal (F3) and fast paths (F2) hold up under direct testing, and latency improved
  across every tool with a usable sample size.

## Remaining Opportunities

1. Establish a controlled, fixed-workload benchmark for exec/analyze latency and cache
   effectiveness, complementing (not replacing) the corrected JSONL aggregation above. Tracked as
   [#1408](https://github.com/clouatre-labs/aptu-coder/issues/1408): extend the existing
   `crates/aptu-coder-core/benches/analysis.rs` criterion harness with `CallGraphCache` and
   `StructuralGraph::build_from_analysis` benchmarks, in-process against the repo's own `src/`.
2. Fix G3 ([#1409](https://github.com/clouatre-labs/aptu-coder/issues/1409), `analyze_directory`
   cache key scoping) -- this is the highest-confidence, most actionable finding in this audit
   and directly explains a measured regression.
3. Close G1 ([#1406](https://github.com/clouatre-labs/aptu-coder/issues/1406), graph cache)
   before resource-surface usage grows past occasional PR-review queries.
4. Investigate whether the `analyze_file`/`analyze_module` cache-hit decline shares G3's root
   cause or is workload-driven, using [#1408](https://github.com/clouatre-labs/aptu-coder/issues/1408)'s
   controlled benchmark rather than further corpus reading.
5. Validate F5's `CallGraph` cache effectiveness under a workload that actually repeats symbol
   lookups; no corpus sampled across either audit does. Covered by
   [#1408](https://github.com/clouatre-labs/aptu-coder/issues/1408).
6. Revisit G2 ([#1407](https://github.com/clouatre-labs/aptu-coder/issues/1407)) only if depth
   limits are relaxed or graph size grows materially.

## Process Note

The corpus-scoping defect in this audit's first two passes is worth naming plainly: both the
scout and guard delegates ran `jq`/the metrics script against a single day's file, and neither
caught it despite AGENTS.md documenting the full `metrics-*.jsonl` glob convention. The guard's
adversarial pass caught a missed module and a mislabeled comparison, but did not think to
question the corpus's own scope -- it adversarially checked the scout's *claims*, not the
scout's *inputs*. Future audit re-runs delegated this way should have the guard (or a dedicated
check) explicitly verify the metrics corpus covers the full retention window before any
percentile or cache-hit-rate claim is trusted.
