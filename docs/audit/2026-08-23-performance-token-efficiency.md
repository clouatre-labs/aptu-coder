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

This is a research-only audit. No `aptu-coder` source was modified; a defect was found and filed
against the separate `scripts/mcp-metrics.py` observability tool (G4, below).

## Scope

- Current branch base: `origin/main`, fetched 2026-08-23.
- Current MCP surface: `analyze_directory`, `analyze_file`, `analyze_module`, `analyze_symbol`,
  `edit_overwrite`, `edit_replace`, `exec_command`, plus `list_resources`, `read_resource`,
  `list_resource_templates` (new since June).
- Prior findings F1-F8 from the June audit, re-verified against current source.
- New scope: `crates/aptu-coder-core/src/graph/structural.rs` and
  `crates/aptu-coder/src/tools/resources.rs`, never previously audited.
- Local metrics corpus: all 11 files under `~/.local/share/aptu-coder/` within the 30-day
  retention window, `metrics-2026-08-10.jsonl` through `metrics-2026-08-23.jsonl`, aggregated
  by hand with `jq`/Python rather than `scripts/mcp-metrics.py` (see Methodology and G4).

## Methodology

Four passes, three of them corrections.

1. A scout delegate re-ran the June methodology -- metrics snapshot via
   `scripts/mcp-metrics.py`, live tool calls to confirm F1-F8 behavior, source reads for new
   surface area.
2. An adversarial guard cross-checked the scout's claims against source and metrics, in the
   pattern used by the 2026-08-03 knowledge-graph audit, and corrected a mislabeled cache-hit-rate
   comparison and an entirely missed module (G1, G2 below).
3. User review caught a corpus-scoping defect neither delegate had: both scoped the "August"
   snapshot to only `metrics-2026-08-23.jsonl` (one day) and compared it against June's full
   30-day corpus, then reasoned about statistical validity from that mismatched premise. The fix
   was to aggregate the full 11-file retention window, matching how June's own number was
   produced.
4. The user then flagged the resulting cache-hit-rate figures as "incredibly low" on inspection.
   That prompted reading `scripts/mcp-metrics.py` itself rather than trusting its output: every
   real tool call emits **two** JSONL lines -- a `"received"` marker at request entry
   (`duration_ms: 0`, `cache_hit: null`, unconditionally) and an `"ok"`/`"error"` line at
   completion (`crates/aptu-coder/src/tools/server.rs:174-193`). Roughly half of every day's
   corpus is these placeholder rows. `scripts/mcp-metrics.py` never filters them out, so its
   cache hit rates and duration percentiles were computed over a denominator half-filled with
   non-calls. The live OTel export path already guards against exactly this
   (`crates/aptu-coder/src/metrics.rs:456-457`, `"received"` events are explicitly skipped so
   they don't pollute latency histograms); the JSONL analysis script never got the same guard.
   Filed as **G4** (#1410). Every number below is recomputed directly from the JSONL, filtering
   `result == "received"`, rather than trusting the script's aggregate.

Net effect of steps 3 and 4 together: real August sample sizes are larger than the flawed first
draft reported, but noticeably smaller than the raw per-file line counts suggest, because roughly
half of every file is a marker row, not a call. Cache hit rates are real and lower than June's
across every per-file/per-directory tool -- less dramatically than the buggy script first
suggested, but still a genuine decline, with a partial source-level explanation (G3).

## Metrics Snapshot

*Table 1: August 2026 tool metrics, aggregated by hand across the full retention window
(`metrics-2026-08-10.jsonl` through `metrics-2026-08-23.jsonl`), counting only completed calls
(`result` is `"ok"` or `"error"`, excluding `"received"` marker rows -- see G4).*

| Tool | Calls | p50 ms | p95 ms | p99 ms | p95 chars | Cache hit rate | Truncated |
|---|---:|---:|---:|---:|---:|---:|---:|
| `exec_command` | 4,330 | 39 | 2,363 | 15,354 | 10,169 | n/a (no cache) | 2.93% |
| `edit_replace` | 365 | 1 | 7 | 10 | 152 | n/a | 0.0% |
| `edit_overwrite` | 244 | 1 | 2 | 2 | 139 | n/a | 0.0% |
| `analyze_module` | 82 | 6 | 369 | 398 | 1,230 | 28.36% (19/67 checked) | 0.0% |
| `analyze_file` | 78 | 4 | 378 | 394 | 2,375 | 47.44% (37/78) | 0.0% |
| `analyze_directory` | 40 | 36 | 404 | 430 | 3,708 | 10.00% (4/40) | 0.0% |
| `analyze_symbol` | 2 | 66 | 66 | 66 | 42 | 0.0% (0/1 checked) | 0.0% |

`analyze_module`'s hit-rate denominator is 67, not 82: 15 completed calls carry `cache_hit: null`
with no other distinguishing field set, most likely from an earlier point in the retention window
before a metrics field was added -- a schema-vintage artifact, not a new gap. `analyze_symbol` has
only 2 completed calls total (1 `ok`, 1 `error`); the `error` call never reaches the cache check
by design, leaving a single checkable sample -- too small to conclude anything.

`exec_command` reliability over the same window: 116 errors (2.68%), 1 timeout. The long tail is
real, not corpus noise: 17 calls exceeded 60 seconds, including three near 180 seconds.

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

It is unknown whether June's own numbers already excluded `"received"` rows -- the June audit
document doesn't say, and G4 shows the standard script wouldn't have excluded them on its own.
If June's figures also include marker rows, the true June baseline was even better than reported
and today's tools improved on a stronger baseline than Table 3 credits; if June counted correctly,
Table 3 is directly comparable. This is unresolved and worth settling before the next re-run
(see Remaining Opportunities).

### Comparison validity

With both the corpus scope and the `"received"`-row contamination corrected, sample sizes for
`analyze_directory` (40), `analyze_file` (78), and `analyze_module` (82) are smaller than June's
but large enough for percentile claims. `analyze_symbol` (n=1 checkable) is not.

*Table 3: Validity of each before/after delta, corrected corpus.*

| Metric | June | August | Verdict |
|---|---:|---:|---|
| `exec_command` p50 | 95 ms | 39 ms | Real -- n=4,330, 59% faster |
| `exec_command` p95 | 3,862 ms | 2,363 ms | Real -- 39% faster |
| `exec_command` p99 | 32,699 ms | 15,354 ms | Real -- 53% faster; long tail still exists |
| `exec_command` truncation | 62.56% | 2.93% | Real -- F7 filter-attribution fix gives a mechanism, not just a number |
| `analyze_directory` p95/p99 | 1,028 / 1,047 ms | 404 / 430 ms | Real -- n=40, 61%/59% faster |
| `analyze_file` p95/p99 | 457 / 520 ms | 378 / 394 ms | Real -- n=78, 17%/24% faster |
| `analyze_module` p95/p99 | 452 / 514 ms | 369 / 398 ms | Real -- n=82, 18%/23% faster |
| `analyze_symbol` p95/p99 | 534 / 620 ms | 66 / 66 ms | Directionally consistent, but n=1 -- a lead, not a measurement |
| `analyze_file` cache hit rate | 69.61% | 47.44% | Real decline (~32% relative) -- see G3 |
| `analyze_module` cache hit rate | 55.92% | 28.36% | Real decline (~49% relative) -- see G3 |
| `analyze_directory` cache hit rate | 30.31% | 10.00% | Real decline (~67% relative) -- root cause identified, see G3 |
| `analyze_symbol` cache hit rate | 0.0% | 0.0% (n=1) | Consistent; still no sample large enough to exercise the cache |

Latency improved across every tool with a usable sample. Cache hit rates fell across every
per-file and per-directory tool; the decline is smaller than the pre-G4-fix numbers suggested, but
still real and, for `analyze_directory`, the largest relative drop of the three.

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
| G4 | High | Bug | `scripts/mcp-metrics.py` includes `"received"` placeholder events in latency/cache aggregates | [#1410](https://github.com/clouatre-labs/aptu-coder/issues/1410) | Open |

## F1-F8: Verification Against Current Source

All eight fixes are confirmed present and correct in source, independent of the corpus and
tooling corrections above:

- **F1** -- `crates/aptu-coder/src/tools/exec_command.rs` has no cache lookup path; metrics report
  a consistent 0.0% hit rate across 4,330 completed calls; tool description no longer advertises
  caching.
- **F2** -- `analyze_module` routes through the lightweight `ModuleInfo` extraction path
  (functions + imports only), confirmed by direct tool call and source read.
- **F3** -- `crates/aptu-coder-core/src/traversal.rs`'s `walk_directory` takes `max_depth` into
  the `WalkBuilder` itself rather than filtering post-walk.
- **F4** -- `process_file_entry` in `analyze.rs` reads each eligible file exactly once.
- **F5** -- `CallGraphCache` (`crates/aptu-coder-core/src/cache.rs`) is a real LRU keyed on root
  path, git ref, follow depth, match mode, `impl_only`, and file mtimes. The corrected corpus has
  only 1 checkable `analyze_symbol` call, still far too few to exercise repeated lookups; the
  implementation is unit-tested and present, but effectiveness stays unverified by metrics.
- **F6** -- `analyze_file.rs:261` calls `semantic.project(params.fields.as_deref())`. The
  corrected corpus shows this is exercised in practice, not just theoretically wired up:
  `fields_projected` is `true` on a real subset of `analyze_file` calls in the aggregated window.
- **F7** -- exec metrics events now carry `filter_applied`, giving filter-tuning visibility that
  didn't exist in June; truncation dropped from 62.56% to 2.93% at n=4,330.
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
warrant. This is the most concrete, source-confirmed explanation available for
`analyze_directory`'s hit rate falling from 30.31% (June) to 10.00% (August, n=40), the largest
relative decline of the three per-file/per-directory tools. It plausibly also contributes to the
`analyze_file`/`analyze_module` declines to a lesser degree, since directory-level cache misses
cascade into re-analyzing the files underneath, but those two tools have their own per-file
`CacheKey { path, modified, mode }` (`cache.rs:65-71`) which should be far less sensitive to
unrelated churn -- their decline (69.61%->47.44%, 55.92%->28.36%) needs separate investigation
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

### G4: `scripts/mcp-metrics.py` includes `"received"` placeholder events in latency/cache aggregates

**Severity:** High
**Type:** Bug
**File:** `scripts/mcp-metrics.py` (`load_records`, `compute_latency`, `compute_cache`)

**Observed state:**

- Every real tool call emits two JSONL lines: a `"received"` marker at request entry
  (`emit_received_metric`, `crates/aptu-coder/src/tools/server.rs:174-193`, always
  `duration_ms: 0`, `cache_hit` left at `Default` -> `null`) and an `"ok"`/`"error"` line at
  completion. Roughly half of every day's file is marker rows.
- `crates/aptu-coder/src/metrics.rs:456-457` already guards the OTel export path against this:
  `"received"` events are explicitly skipped before recording, with a comment noting
  `duration_ms=0` would pollute latency histograms otherwise.
- `scripts/mcp-metrics.py`'s `load_records` applies no equivalent filter. `compute_cache` treats
  every record for a cacheable tool as either a hit or a miss (`misses = len(recs) - hits`),
  so `"received"` rows -- which are neither -- count as misses. `compute_latency` includes their
  `duration_ms: 0` in every percentile calculation.
- Effect measured on this repo's own corpus: the script reports `analyze_file`'s cache hit rate
  as 14.57%; the correct rate, excluding `"received"` rows, is 47.44%. `analyze_directory`:
  3.54% reported vs 10.00% correct. `dur_p50` is reported as 0ms for nearly every tool, an
  artifact of `"received"` rows making up roughly half the corpus.
- `docs/METRICS.md` does not document the `"received"` result value at all. AGENTS.md's own
  documented jq one-liner for cache hit rate (`select(.cache_hit!=null)`) already applies the
  correct filter by construction -- only the script has the gap.

**Impact:** Every prior use of `scripts/mcp-metrics.py --format json` for cache-hit-rate or
latency-percentile claims -- including this audit's own first corrected draft -- understated
cache effectiveness and overstated how much of the corpus resolves near-instantly. This is a
tooling defect independent of `aptu-coder`'s own runtime behavior, but it directly undermines the
reliability of any audit or dashboard built on the script's output.

**Fix direction:** Filter `result == "received"` out of `load_records` before any `compute_*`
function runs, matching the OTel path's existing behavior. Document the `"received"` event kind
in `docs/METRICS.md`.

**Acceptance criteria:**

- `load_records` excludes `"received"` rows from all aggregates by default.
- `docs/METRICS.md` documents the `"received"` result value and its exclusion.
- `uv run ruff check`, `uv run ruff format --check`, `uv run pyright` pass.

## Best Practices Affirmed

- The two-tier cache design (L1 in-memory, L2 disk) established by F5 and extended by the
  knowledge-graph work is sound and consistent across subsystems, except for the gaps in G1 and
  G3.
- JSONL metrics carry enough attribution (`filter_applied`, `cache_tier`, `fields_projected`,
  `output_truncated`) to drive this kind of audit without extra instrumentation -- provided the
  full retention window is aggregated and `"received"` marker rows are excluded (G4).
- The live OTel export path already handles the `"received"`-row problem correctly
  (`metrics.rs:456-457`); the gap is isolated to the offline JSONL analysis script.
- Bounded traversal (F3) and fast paths (F2) hold up under direct testing, and latency improved
  across every tool with a usable sample size.

## Remaining Opportunities

1. Fix G4 ([#1410](https://github.com/clouatre-labs/aptu-coder/issues/1410)) first -- every
   other metrics-based claim in this and future audits depends on it being correct.
2. Establish a controlled, fixed-workload benchmark for exec/analyze latency and cache
   effectiveness, complementing (not replacing) corrected JSONL aggregation. Tracked as
   [#1408](https://github.com/clouatre-labs/aptu-coder/issues/1408): extend the existing
   `crates/aptu-coder-core/benches/analysis.rs` criterion harness with `CallGraphCache` and
   `StructuralGraph::build_from_analysis` benchmarks, in-process against the repo's own `src/`.
3. Fix G3 ([#1409](https://github.com/clouatre-labs/aptu-coder/issues/1409), `analyze_directory`
   cache key scoping) -- the highest-confidence, most actionable *product* finding in this audit,
   and the largest relative cache-hit-rate decline measured.
4. Close G1 ([#1406](https://github.com/clouatre-labs/aptu-coder/issues/1406), graph cache)
   before resource-surface usage grows past occasional PR-review queries.
5. Investigate whether the `analyze_file`/`analyze_module` cache-hit decline shares G3's root
   cause or is workload-driven, using [#1408](https://github.com/clouatre-labs/aptu-coder/issues/1408)'s
   controlled benchmark rather than further corpus reading.
6. Validate F5's `CallGraph` cache effectiveness under a workload that actually repeats symbol
   lookups; no corpus sampled across either audit does. Covered by
   [#1408](https://github.com/clouatre-labs/aptu-coder/issues/1408).
7. Confirm whether June's reported numbers already excluded `"received"` rows before treating
   Table 3's deltas as final; see the note under Table 2.
8. Revisit G2 ([#1407](https://github.com/clouatre-labs/aptu-coder/issues/1407)) only if depth
   limits are relaxed or graph size grows materially.

## Process Note

This audit went through three corrections after its first draft, in the same session:

1. **Scout and guard scoped the corpus to one file, not the retention window.** Both compared a
   single day (2,360 raw lines) against June's full 30-day aggregate and reasoned about
   statistical validity from that mismatched premise. Caught by user review.
2. **The corrected aggregate still looked wrong.** Cache hit rates dropped further on
   recomputation across all 11 files, and the user flagged them as "incredibly low" rather than
   accepting the number at face value.
3. **That challenge led to reading `scripts/mcp-metrics.py` itself**, which surfaced G4: roughly
   half of every corpus file is a `"received"` placeholder row the script never filters out,
   deflating every cache-hit-rate and latency-percentile figure it has ever produced.

The common thread: the guard's adversarial pass checked the scout's *claims* against source, but
never checked the scout's *inputs* -- neither the corpus scope nor the tooling computing the
numbers. Both defects were caught by a human asking "are we sure about this number?", not by
either delegate. Future audit re-runs delegated this way should have an explicit step -- guard or
otherwise -- that verifies the measurement tool itself against a hand-computed sample before any
of its output is trusted, not just that the tool's claims are internally consistent.
