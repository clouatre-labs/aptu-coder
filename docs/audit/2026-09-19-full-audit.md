# Audit: Full Lean Audit for Subagent Workloads -- September 2026

Date: 2026-09-19  
Base: d1390ae (v0.34.2)  
Spec baseline: MCP 2026-07-28 (rmcp 3.x)  
Evidence: codebase inventory (three parallel research passes: tool surface / pi source comparison / spec research, each verified against the actual tree) + prior audits and telemetry (2026-09-14 window, 58,776 calls) + param-description-experiments exp1/exp2 results + blog synthesis (clouatre.ca, "How Much Tool Documentation Do AI Agents Actually Need?")

## See Also

- [2026-09-14-cognitive-load-audit.md](2026-09-14-cognitive-load-audit.md) -- parameter-surface audit (F1/F3/F4 landed)
- [2026-09-14-response-format-experiment-results.md](2026-09-14-response-format-experiment-results.md) -- exp2 NO-GO on the response_format enum
- [2026-09-11-param-description-experiments-audit.md](2026-09-11-param-description-experiments-audit.md) -- description-verbosity null result
- [2026-09-13-code-quality.md](2026-09-13-code-quality.md) -- structural cleanup (all 5 findings resolved)
- [pi source](https://github.com/badlogic/pi-mono) -- comparison baseline (v0.85.1, locally installed)

## Question

What is aptu-coder for, and is it optimal for subagents? The observation: pi (a coding agent harness with only built-in `read`/`grep`/`find`/`bash`/`edit` tools and zero AST tooling) often completes tasks with fewer tokens than agents using aptu-coder. Should aptu-coder become cheaper, leaner, simpler? What can be dropped or reshaped, with evidence?

## Answer (short)

aptu-coder is a subagent-facing tool server: cheap, deterministic code analysis + edit + exec that a small model calls on behalf of an orchestrator. It should stay that. Three evidence-backed conclusions:

1. **The token problem is in `exec_command` output, not the analyze suite.** `exec_command` is 72% of all calls (telemetry, 2026-09-14), and its `structuredContent` serializes the full `ShellOutput` -- including an unconditional `interleaved` field that duplicates stdout+stderr content in arrival order. Every exec response carries the output roughly twice to three times (text block + stdout + stderr + interleaved). This is the single largest validated token sink on the surface (F1).
2. **pi proves AST tools are not needed for efficiency -- but also that aptu-coder's niche is exactly the complement.** pi ships no tree-sitter dependency, no analyze tools, and wins on tokens through output shaping: raw text, relativized paths, dual truncation caps (2000 lines / 50KB) with actionable continuation hints, one-line edit successes with diffs kept out of the model-visible text. aptu-coder already matches several of these (next_cursor continuation, overflow slot files, filter-cap recovery, small-oldText guidance in `edit_replace` docs). The analyze suite is only ~8% of traffic; do not expand it, and do not merge it (F4, F8).
3. **The parameter surface is converged; remaining wins are output-side and internal.** After #1542/#1543/#1544 and the f6bfe21 description trim (blog: -12.4/-13.8% cold-call input tokens, accuracy null), the schema is lean. What remains: one confusable depth-knob pair on `analyze_symbol` (F2), response-envelope inconsistency across the four analyze tools (F5), and two internal cleanups with zero behavior change (F6, F7).

## Summary Table

| # | Finding | Verdict | Recommendation | Priority |
|---|---------|---------|----------------|----------|
| F1 | `ShellOutput.interleaved` duplicates stdout+stderr in every `exec_command` response: text block embeds `interleaved` once (`exec_command.rs:296-302`), then `serde_json::to_value(&output)` re-serializes `stdout`, `stderr`, **and** `interleaved` into `structuredContent` (`exec_command.rs:607`) | CONFIRMED | Drop `interleaved` from `ShellOutput` (keep `stdout`/`stderr`; the model-visible text block already carries the interleaved view; full interleaved capture remains in the slot file) -- [#1578](https://github.com/clouatre-labs/aptu-coder/issues/1578) | HIGH |
| F2 | `analyze_symbol` exposes both `follow_depth` and `max_depth` (`types.rs:239,246`) -- two depth knobs on one tool, differentiated only by prose | CONFIRMED | Collapse to one depth parameter (keep `max_depth` semantics; fold call-graph warning depth into server logic). Alpha policy: remove outright -- [#1579](https://github.com/clouatre-labs/aptu-coder/issues/1579) | MEDIUM |
| F3 | `edit_replace` carries both the single-edit form (`old_text`/`new_text`) and `edits[]` | CONFIRMED | **No change.** pi's production edit tool carries the same dual surface with system-prompt guidance steering to batch; the dual surface is the validated pattern, not a smell | none |
| F4 | Merge `analyze_module` into `analyze_file` | REJECTED (third pass) | No change. `analyze_module` outnumbers `analyze_file` in telemetry, owns the L2 cache fast path, and exp2 showed structural collapses don't help agents. Type-level divergence (`ModuleFunctionInfo` without params/return types) is the token saving, not bloat | none |
| F5 | Analyze-tool response envelopes are inconsistent: `cache_tier` on directory+symbol but not file+module; `analyze_module` has no `next_cursor`; `analyze_file` serializes two sections beyond its promised trio -- `references` and `calls` (`types.rs:480,484`; `call_frequency`/`impl_traits`/`def_use_sites` are correctly `#[serde(skip)]`) | PARTIAL | Drop `references`/`calls` from the default `analyze_file` response or fold them behind the existing `fields` projection; align `cache_tier` presence (cheapest: document, or drop everywhere) -- [#1580](https://github.com/clouatre-labs/aptu-coder/issues/1580) | MEDIUM |
| F6 | `migrate_legacy_metrics_dir` (`metrics_export.rs:436-461` + tests at `:506,523`) migrates a data directory from aptu-coder's previous product name -- a one-time rename with no forward value | CONFIRMED | Delete. Zero behavior change for any install that has run any recent version -- [#1581](https://github.com/clouatre-labs/aptu-coder/issues/1581) | LOW |
| F7 | The per-handler telemetry preamble (~30 identical lines: emit_received, session/client locks, trace-context extraction, 12+ span records) is copy-pasted verbatim across all 7 tool handlers in `lib.rs` | CONFIRMED | Extract a helper/macro. ~180 fewer lines, zero behavior change -- [#1582](https://github.com/clouatre-labs/aptu-coder/issues/1582) | LOW |
| F8 | pi ships zero AST tooling and wins on tokens; aptu-coder's analyze suite is ~8% of traffic and its heavier outputs cost more than pi's read+grep workflow for equivalent tasks | CONFIRMED (observation, not a defect) | **No change.** The suite is the differentiator for subagents that need signatures/call-graphs without reading whole files. Keep 7 tools (15-35% of the lowest measured tool-selection cliff) | none |
| F9 | MCP 2026-07-28: all 7 tools carry `annotations` (verified: 7 `annotations(` sites in `lib.rs`); roots/sampling correctly avoided per SEP-2577; timeouts removed per #1542. New spec affordances not yet used: `tools/list` cacheable results (`ttlMs`/`cacheScope`); PR 2636 tool manifests remains an open proposal | CONFIRMED | No code change required for conformance. Optionally adopt `ttlMs` on `tools/list` when rmcp exposes it; do not build on PR 2636 | LOW |
| F10 | pi's edit fallback normalizes NFKC, smart quotes, and nbsp beyond aptu-coder's CRLF + trailing-whitespace fallback (2c35309) | PARTIAL | Optional, low priority: telemetry shows `edit_replace` churn is `stale_content_hash`-driven (118 events) with `ambiguous` at only 4 events; match-fuzzy normalization would not have moved the dominant error class | LOW |

## Evidence Detail

### Usage distribution and where tokens go

Full-history telemetry (82,504 calls): `exec_command` 72%, top-3 (`exec_command`, `edit_replace`, `edit_overwrite`) 91.9%, analyze family ~8%. Any token-optimization effort weighted by traffic lands on `exec_command`'s output shape. F1 is the only finding that touches that hot path, and it is a pure deletion: the interleaved string is re-derivable from nothing the client needs (the text block already presents the interleaved view; the slot file preserves it for overflow recovery).

### pi comparison (v0.85.1 source)

- pi's tools: `bash, edit, find, grep, ls, read, write` -- no AST, no tree-sitter in `package.json`.
- Output shaping that wins it tokens: 2000-line/50KB dual truncation caps; one-line actionable continuation notices (`[Showing lines 1-2000 of 5432. Use offset=2001 to continue.]`); paths relativized against the search root; 500-char line caps on grep output; edit success returns one short line with diffs kept out of the model-visible text.
- aptu-coder parity check: pagination continuation (`next_cursor`) -- present; overflow recovery (`aptu-overflow://` slot files, filter-cap recovery) -- present and more capable; caps embedded in parameter descriptions -- present. The one shape gap is F1.
- pi's edit error messages are prescriptive ("provide more context to make it unique"); aptu-coder's ambiguous error carries occurrence counts and line numbers (`edit_replace.rs:303-322`, per the 2026-09-14 audit F2 verification). Parity.

### Spec alignment (MCP 2026-07-28)

- Normative floor met: JSON Schema 2020-12 input schemas, `outputSchema` conformance via `structuredContent`, `tools/list_changed` capability, standard `-32602` error routing (SEP-2164).
- SEP-2577 (deprecate roots/sampling/logging) shipped in 2026-07-28: aptu-coder's `working_dir` parameters are the sanctioned replacement pattern; no roots/sampling APIs are used. Logging: the server's observability is its own JSONL/OTel channels, not deprecated MCP logging.
- SEP-1539 (timeouts) remains a proposal; #1542 already removed tool-level timeouts. Correct posture.
- PR 2636 (tool manifests) is open, unmerged -- watch only. The spec's `ttlMs`/`cacheScope` cacheable listing is shipping and worth adopting when rmcp surfaces it; at 7 tools the win is small.
- Descriptions: the f6bfe21 trim is validated by the blog's experiments (-12.4/-13.8% cold-call input tokens, accuracy null at both tiers); no further description edits are recommended (exp1/exp2 both showed description and structure edits are null-to-negative for accuracy).

## Recommendations by ROI

1. **Drop `interleaved` from `ShellOutput`** (F1, [#1578](https://github.com/clouatre-labs/aptu-coder/issues/1578)). One-field removal on the 72%-of-traffic tool; cuts every exec response by roughly the size of the command output. Keep stdout/stderr and slot-file recovery.
2. **Collapse `analyze_symbol`'s two depth knobs into one** (F2, [#1579](https://github.com/clouatre-labs/aptu-coder/issues/1579)). Alpha policy: remove `follow_depth`, let `max_depth` carry the cap, move the >2-levels warning into server logic.
3. **Trim `analyze_file`'s default `semantic` payload** (F5, [#1580](https://github.com/clouatre-labs/aptu-coder/issues/1580)): `references` and `calls` serialize today without being promised or projected; move them behind `fields` or drop them.
4. **Delete `migrate_legacy_metrics_dir`** (F6, [#1581](https://github.com/clouatre-labs/aptu-coder/issues/1581)).
5. **Extract the 7× duplicated telemetry preamble** (F7, [#1582](https://github.com/clouatre-labs/aptu-coder/issues/1582)).

Explicit do-NOTs, all validated: no tool merges (F4), no tool-count reduction (7 tools, far below any selection cliff), no further description edits (exp1 null; f6bfe21 already landed the trim), no single/`edits[]` consolidation (F3), no build-out on PR 2636 (F9), no NFKC fallback work until telemetry shows match-miss churn (F10).

## Impact (measured facts only)

- F1 removes a duplicate copy of command output from ~72% of tool calls; the delta scales with output size (output p50 456 chars, truncation cluster at ~30KB cap -- so the mean win is modest and the tail win is up to ~30KB/response).
- F2 removes 1 of `analyze_symbol`'s 10 parameters and one confusable pair.
- F5 removes 2 unpromised payload sections (`references`, `calls`) from the default `analyze_file` response.
- F6/F7 are internal: ~220 fewer lines, zero behavior change.
- No accuracy claims are made; per policy, accuracy deltas require the exp framework. F1/F5 are response-side (post-selection), so selection accuracy is untouched by construction.

## Future Work (non-blocking)

- Re-evaluate `ttlMs`/`cacheScope` on `tools/list` when rmcp exposes the 2026-07-28 cacheable-listing surface (F9).
- If `stale_content_hash` churn persists in telemetry after #1542-#1544 adoption matures, consider counting it toward the `EDIT_STALE_CONTEXT` circuit breaker (carried over from the 2026-09-14 audit).

## Sources

- [MCP 2026-07-28 tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools), [SEP-2577](https://modelcontextprotocol.io/seps/2577-deprecate-roots-sampling-and-logging), [SEP-1539](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1539), [PR 2636](https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2636), [MCP deprecation policy](https://blog.modelcontextprotocol.io/posts/2026-07-28/)
- Internal: `crates/aptu-coder/src/lib.rs`, `crates/aptu-coder/src/tools/exec_command.rs`, `crates/aptu-coder/src/tools/edit_replace.rs`, `crates/aptu-coder/src/metrics_export.rs`, `crates/aptu-coder-core/src/types.rs`, `~/.local/share/aptu-coder/metrics-*.jsonl`
- pi v0.85.1 source (`dist/core/tools/`, `dist/core/system-prompt.js`), local install
- Blog synthesis: "How Much Tool Documentation Do AI Agents Actually Need?" (clouatre.ca, 2026-09-18) and the underlying [param-description-experiments](https://github.com/clouatre-labs/param-description-experiments) dataset (DOI 10.5281/zenodo.22844431)
