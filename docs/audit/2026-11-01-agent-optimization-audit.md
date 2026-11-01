# Audit: Autonomous-Agent Optimization (Stability, Speed, Context Economy) -- November 2026

Date: 2026-11-01
Base: 68a0419 (v0.41.1)
Evidence: November telemetry re-read (48,196 events, 1,284 sessions, 2026-10-26 through 2026-11-01) + three parallel code-review passes (stability / speed / context economy) + three independent adversarial verification passes over every finding, each checking cited lines, reachability, test assertions, and regression risk + judge validation (jev-1.14.0) for the positioning section

## See Also

- [2026-10-30-error-taxonomy-audit.md](2026-10-30-error-taxonomy-audit.md) -- T1-T3 (fixed in v0.41.0/v0.41.1); this audit defers to it on error-code routing and adds the post-fix validation gate
- [2026-09-19-full-audit.md](2026-09-19-full-audit.md) -- full lean audit; F1 already identified the exec_command token sink; its conclusions are re-confirmed here
- [2026-08-23-performance-token-efficiency.md](2026-08-23-performance-token-efficiency.md) -- prior performance baseline

## Question

Where can aptu-coder still be optimized for its actual consumers -- autonomous agents running long sessions -- on stability, speed, and context economy? And does aptu-coder have a role in the x402 demand-probe plans (clouatre.ca#2126)?

## Answer (short)

Three evidence-backed conclusions:

1. **The remaining speed wins are in cache-path bookkeeping, not the analysis itself.** The L1/LRU + L2 disk cache architecture is sound, but cache-key construction re-does the work it is supposed to save: `analyze_directory` walks, filters, sorts, and builds two keys before any cache decision; `analyze_file` fully reads the file twice on an L1 miss; `analyze_symbol` clones and re-hashes the full output on L1 hits. Verified findings (#1776, #1777). Two of the eight originally proposed speed findings were refuted on verification and dropped.
2. **Context-economy trims must respect the byte-stability contract.** The codebase deliberately keeps success-path output byte-stable (golden test at `tests/exec_command.rs:1346`; the conditional filter-line comment states the intent), and the #1773/#1774 description text is curated. Four of seven proposed trims were rejected as regression-inducing; four low-risk deduplications survived (#1778).
3. **Stability residuals are real but small; one is design-first.** The graph resource path still has a silent-empty symbol-not-found (the sibling of the fixed T1), verify_anchors lacks a batch cap, and the metrics channel is unbounded -- all low-risk fixes (#1779). Cursor snapshot-binding is confirmed but needs a design note before landing. The call-graph chain loop was reviewed and explicitly rejected: its cost is O(follow_depth x E), and "fixing" it would change output semantics.

On x402: aptu-coder's audit capability (this document's methodology -- schema hygiene, error routing, truncation behavior, pagination ergonomics, description economy of an MCP server) is itself the strongest candidate engine for a demand-probe endpoint. Judge validation leaned "agent/MCP-readiness audit" (0.56) over "paid per-call analysis" (0.38) at low confidence (0.41) -- a sequencing signal, not a verdict. aptu-coder stays out of the payment path; the packaging decision remains TypeScript/Worker-native per #2126.

## Summary Table

| # | Finding | Verdict | Recommendation | Priority |
|---|---------|---------|----------------|----------|
| V1 | `analyze_directory` performs walk + git-ref filter + sort + two cache-key builds before any cache decision; L1 "hits" pay a full walk (`analyze_directory.rs:169-216`, `cache.rs:89-107`) | CONFIRMED | Derive the disk key from `DirectoryCacheKey`; do not weaken L1 validation below per-file mtimes. [#1776](https://github.com/clouatre-labs/aptu-coder/issues/1776) | HIGH |
| V2 | `analyze_symbol` clones + blake3-hashes the full formatted output on every call including L1 hits (`analyze_symbol.rs:445,487,685-696`) | CONFIRMED | Store the content hash at put time; account for the post-clone cursor suffix. [#1776](https://github.com/clouatre-labs/aptu-coder/issues/1776) | HIGH |
| V3 | `analyze_file` reads the file fully twice on an L1 miss (key read + `read_source_checked`) (`analyze_file.rs:49-50`, `analyze.rs:368-378`). The L1-hit claim from the review pass was refuted: L1 hits short-circuit before the read | CONFIRMED (partial) | Hash once, pass bytes down; do not switch to mtime-based keying. [#1776](https://github.com/clouatre-labs/aptu-coder/issues/1776) | MEDIUM |
| V4 | Triple stat per file in the directory path: eligibility metadata + read's internal stat + walker's already-collected mtime (`analyze.rs:175-184,368-378`) | CONFIRMED | Reuse `WalkEntry`-cached stats in the dir path. [#1776](https://github.com/clouatre-labs/aptu-coder/issues/1776) | MEDIUM |
| V5 | First parse anywhere compiles 7 query types x ~20 grammars behind one `LazyLock` (`parser.rs:185-210`) -- one-time stall on the first call of every session | CONFIRMED | Per-language lazy init; preserve log-and-skip failure semantics. [#1777](https://github.com/clouatre-labs/aptu-coder/issues/1777) | MEDIUM |
| V6 | edit_replace stale-hash error duplicates the retry instruction in text and meta (`edit_replace.rs:414-423`) | CONFIRMED | Trim the text copy; keep both hashes full-length in text (the retry flow copies `expected_content_hash` from there). [#1778](https://github.com/clouatre-labs/aptu-coder/issues/1778) | MEDIUM |
| V7 | Pagination usage sentence (~170 chars) repeats on every page (`analyze_directory.rs:515`, `analyze_file.rs:418`, `analyze_symbol.rs:690`) | CONFIRMED | Full sentence only when no cursor was supplied; later pages emit `NEXT_CURSOR:` alone. [#1778](https://github.com/clouatre-labs/aptu-coder/issues/1778) | MEDIUM |
| V8 | `DefUseSite.snippet` is line-bounded but not byte-bounded; a 10k-char line yields a 30k-char snippet (`types.rs:66`, `parser_elements.rs:896-920`) | CONFIRMED | Per-line clamp (~120 chars); no schema change. [#1778](https://github.com/clouatre-labs/aptu-coder/issues/1778) | LOW |
| V9 | edit_replace not-found output shows the nearest-match line twice (`edit_replace.rs:152-197`) | CONFIRMED | Drop the window copy, keep the `Nearest match:` sentence (test-pinned). [#1778](https://github.com/clouatre-labs/aptu-coder/issues/1778) | LOW |
| V10 | Graph resources return a silent empty success on symbol-not-found (`structural.rs:355-374`, consumed at `resources.rs:404-412,571-581`) -- T1's sibling on the resources path, not covered by 38fd3f3 | CONFIRMED | Map empty to an error naming the symbol; cover both consumption paths. [#1779](https://github.com/clouatre-labs/aptu-coder/issues/1779) | HIGH |
| V11 | verify_anchors: no batch cap, re-reads file + recompiles regex per anchor (`anchors.rs:27-88`, schema `types.rs:1131-1140`) | CONFIRMED | `maxItems` + early INVALID_PARAMS; group reads by path. [#1779](https://github.com/clouatre-labs/aptu-coder/issues/1779) | MEDIUM |
| V12 | MetricsSender is an unbounded fire-and-forget channel with no drop policy (`metrics.rs:505-512`) | CONFIRMED (theoretical trigger: fcntl-held `.lock` file) | Bounded channel or depth-check drop; send failure already ignorable. [#1779](https://github.com/clouatre-labs/aptu-coder/issues/1779) | LOW |
| V13 | edit_failure_counts / file_edit_locks poisoned-mutex `.expect` (`edit_overwrite.rs:200-211`, `edit_replace.rs:57-76,550-556,608-611`) | CONFIRMED (dormant: critical sections are panic-free) | Adopt `lock_or_recover` (`cache.rs:122-136`). Hardening. [#1779](https://github.com/clouatre-labs/aptu-coder/issues/1779) | LOW |
| V14 | Cursors are unbound offsets; a cursor held across a mutating edit silently shifts pages (`pagination.rs:39-44`, 5 consumer sites) | CONFIRMED | DESIGN-FIRST: additive serde-defaulted fingerprint in `CursorData`; fingerprint source needs a design note. [#1779](https://github.com/clouatre-labs/aptu-coder/issues/1779) | MEDIUM (design gate) |
| -- | exec_command success-header trim (proposed C1) | REJECTED | Golden test `tests/exec_command.rs:1346` pins byte-stable output; the conditional filter line documents this as deliberate | none |
| -- | verify_anchors drop-`path` compact output (proposed C4) | REJECTED | Breaking output-schema change; contract documented in the tool description | none |
| -- | Analyze-tool routing-description trim (proposed C5) | REJECTED | Reverts curated #1773/#1774 intent; descriptions actively maintained | none |
| -- | resources.rs overflow-file windowed read (proposed S5) | REFUTED | No slicing after read; resource contract returns the full capture; input capped at 512KB by construction | none |
| -- | subtree_counts "extra unbounded walk" (proposed S7) | REFUTED | O(n) in-memory pass over already-collected entries; no I/O | none |
| -- | call-graph inner chain loop memoization (proposed T9) | REJECTED | Cost is O(follow_depth x E), a small constant factor; the greedy first-edge walk defines chain semantics -- memoizing changes output | none |
| -- | T1-T3 post-fix telemetry validation | OPEN | Re-measure after 1-2 weeks of v0.41.1+ data. [#1780](https://github.com/clouatre-labs/aptu-coder/issues/1780) | GATE |

## Evidence Detail

### Telemetry re-read (with corrections)

October logs (48,196 events, 1,284 sessions) initially suggested an 86% `analyze_symbol` error rate, a 3,805-call `schema_surface` flood, and 836 untyped errors. All three were resolved against [2026-10-30-error-taxonomy-audit.md](2026-10-30-error-taxonomy-audit.md): the error rate is T1 (fixed in v0.41.1), `schema_surface` events are per-server-start emissions not client traffic, and the untyped errors are T2/T3 (fixed). Post-fix data is too thin to confirm the fixes (zero `analyze_symbol` calls since v0.41.1) -- hence the validation gate, [#1780](https://github.com/clouatre-labs/aptu-coder/issues/1780). The healthy contrast stands: `exec_command` at ~1% error rate with fully populated subtypes (heredoc_error 54, timeout 47, cd_path_not_found 19, working_dir_not_found 6).

### Verification methodology

Every finding from the three review passes was re-verified by an independent pass that opened the cited lines, traced callers for reachability, grepped test assertions for golden-value conflicts, and rated regression risk. Outcome: 14 confirmed or partially confirmed findings (several with corrected line numbers or narrowed claims), 3 rejected as regression-inducing despite accurate code observations, 2 refuted as factually wrong, 1 rejected as semantically risky. The rejected items are recorded above so they are not re-proposed.

### x402 positioning (public-safe summary)

External landscape (brave, verified 2026-11-01): x402-mcp integration wrappers exist and are mature; the x402 Foundation's membership makes the protocol durable; demand-probe economics reward cheap differentiated endpoints over heavy infrastructure. Judge (jev-1.14.0) compared four candidate roles for aptu-coder and leaned "agent/MCP-readiness audit endpoint" (0.56) over "paid per-call analysis endpoint" (0.38) at 0.41 confidence. Interpretation: this repository's audit methodology -- the checks applied to itself in this document -- is a differentiating, cheap-to-operate probe candidate for the aptu.dev endpoint portfolio; serving the analyze tools as paid per-call endpoints is technically trivial via existing wrappers but operationally heavier and narrower. aptu-coder itself stays a local stdio MCP server and stays out of the payment path; no code change in this repository is implied by the positioning.
