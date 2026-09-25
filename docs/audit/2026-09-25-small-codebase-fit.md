# Audit: Small-Codebase Fit — Why aptu-coder Loses on Simple Tasks and How to Trim -- September 2026

Date: 2026-09-25  
Base: 927f076 (v0.35.3, post-v18-ladder)  
Spec baseline: MCP 2026-07-28 (rmcp 3.x)  
Evidence: three parallel research passes (schema/enumeration, pi-adapter surfacing modes, response-envelope analysis, each verified against the live tree or live `tools/list` output) + local JSONL telemetry (39,232 call events, 2026-09-25 window incl. `schema_surface` 10,156 B) + v18 ladder results + v19 methodology + prior audits (#1605 prior-art record, 2026-09-19 full audit, exp1/exp2 null results, clouatre.ca tool-docs ablation post)

## See Also

- [2026-09-19-full-audit.md](2026-09-19-full-audit.md) -- full lean audit (F1-F10; F8 keep-7-tools constraint)
- [2026-09-14-response-format-experiment-results.md](2026-09-14-response-format-experiment-results.md) -- exp2 NO-GO on response-shaping collapse
- [2026-09-22-v18-benchmark-readiness.md](2026-09-22-v18-benchmark-readiness.md) -- v18 ladder findings
- [../benchmarks/v19/methodology.md](../benchmarks/v19/methodology.md) -- real-codebase re-test with crossover track
- [clouatre.ca: How Much Tool Documentation Do AI Agents Actually Need?](https://clouatre.ca/blog/mcp-tool-docs/) -- -12/-14% cold-call tokens, accuracy null

## Question

v18's pilot was a failed experiment (ripgrep-optimal synthetic fixture, wrong premises); v19 corrects the design. The surviving learning: aptu-coder's fixed context overhead makes it a poor fit below some threshold of codebase size / task complexity. Where is that threshold, what exactly composes the overhead, and what can be trimmed so aptu-coder fits both simple and complex changes with minimal context bloat?

## Answer (short)

The v18 pilot's ~16.6k (MCP) vs ~4.1k (native) tokens/session gap decomposes into three layers, of which only one is genuinely aptu-coder's:

1. **Always-on surface: ~7.6k tokens/session (measured).** Live `tools/list` enumeration: input schemas + descriptions ≈ 3.9k tokens (15.7k chars), **outputSchemas ≈ 3.4k tokens (13.7k chars, 47% of the surface -- never audited before)**, server instructions ≈ 0.2k. Confirmed by the `schema_surface` telemetry event (10,156 B of input schemas alone).
2. **Surfacing mode tax: ~4.7k first-turn tokens under `directTools: true`** (pi-mcp-adapter inlining). The adapter's `directTools: false` gateway mode cuts the aptu-attributable share to under ~0.7k over pi's own floor (measured; probe arms built in #1621, executed 2026-09-25 — see S4). This layer costs zero aptu-coder code to eliminate.
3. **Per-call envelope duplication: the hot-path sink.** `exec_command` (72% of all calls) returns full stdout twice -- text block plus `structuredContent.stdout`. `analyze_directory` returns a `formatted` string **and** a parallel `files` array carrying the same data with absolute canonical paths (~60+ chars repeated per entry). Analyze-tool medians are otherwise lean (117-973 chars; telemetry).

The threshold claim is **directional and unmeasured**: for grep-shaped tasks (string lookup, one-file questions), native read+rg wins by construction, at any repo size -- aptu-coder pays the fixed surface and per-call envelope without amortization (v18 pilot: 0/20 sessions used any aptu tool). This audit intentionally publishes **no LOC cutoff**: the "roughly 20k LOC" phrasing from earlier drafts is a hypothesis, not a measurement; the crossover is exactly what v19's Track C is designed to quantify, and any number before it runs would be unsupported. Structural multi-hop tasks (callers-of-X, rename impact, transitive depth) remain the amortization case, as originally earned in v12/v13.

Recommended path (KISS, ranked): response-envelope dedup first (biggest validated hot-path lever, measurable via existing `est_output_tokens` telemetry), gateway-default second (zero server code, run the already-built probes), outputSchema slimming third (largest unexplored always-on win, outside prior-audit constraints). **Do not** reintroduce a server-side profile mechanism -- attempted and removed twice (#1278, #805).

## Summary Table

| # | Finding | Verdict | Recommendation | Priority |
|---|---------|---------|----------------|----------|
| S1 | `exec_command` duplicates full stdout in the text block and again in `structuredContent` (+ `exit_code`/truncation echoes); it is 72% of call volume | CONFIRMED (live call inspection; telemetry medians 973 chars, p90 6,723) | Dedup: keep the model-visible text block single-source; `structuredContent` carries metadata only, with the overflow slot-file path for full capture. Verify delta via `est_output_tokens` before/after -- [#1633](https://github.com/clouatre-labs/aptu-coder/issues/1633) | HIGH |
| S2 | `analyze_directory` serializes `formatted` **and** `files` (same data), with absolute canonical paths repeated per entry (`analyze.rs:56-112`) | CONFIRMED | Emit one representation; relativize paths against `working_dir`; `is_test` is the only zero-information candidate left (types.rs:298, serialized unconditionally; `subtree_counts` is already `#[serde(skip)]`+`schemars(skip)`, analyze.rs:85-88, so no trim exists there) -- [#1634](https://github.com/clouatre-labs/aptu-coder/issues/1634) | HIGH |
| S3 | `outputSchemas` are ~3.4k tokens/session (47% of tool surface) with per-field description prose; prior audits only covered descriptions/inputSchema | CONFIRMED (live `tools/list`: analyze_file 3,957ch, exec_command 1,973ch, analyze_symbol 2,930ch, edit_replace 1,807ch) | KISS path: a recursive `description`-strip post-processor over the `schema_for_type` result (no type changes; `description` is not data, so structuredContent conformance is unaffected; rustdoc keeps doc-comments). Ablation-gate like f6bfe21 -- [#1635](https://github.com/clouatre-labs/aptu-coder/issues/1635) | MEDIUM-HIGH |
| S4 | `directTools: true` first-turn tax ~4.7k tokens; adapter gateway mode (pay-to-play) cuts it to ~3.0k; probe arms (`mcp-gateway`, `mcp-search`) built in #1621 and now executed (wiring smoke, 2026-09-25) | CONFIRMED, target partially met | Gateway/search wiring smokes PASS (shadow `mcp.json` verified; gateway surfaces zero aptu tools, search surfaces only `mcp({search})`). First-turn input: gateway 3,106 tok, search 2,821 tok vs 4,708 under `directTools: true` — ~38% below, but above the <1.5k aspiration because pi's own system prompt + native tools dominate the remainder. 1-pair smokes (search first-attempt activation ≥80%, multi-turn amortization) remain before flipping the documented default -- [#1637](https://github.com/clouatre-labs/aptu-coder/issues/1637) | HIGH (sequencing) |
| S5 | Server instructions (~890 chars) mandate a discovery workflow (analyze_directory first, pagination rules, jq/metrics recipes) -- a first-call tax rg never pays | CONFIRMED | Move metrics/pagination guidance JIT into tool results; keep instructions to one short paragraph. Small (~200 tok) but zero risk -- [#1636](https://github.com/clouatre-labs/aptu-coder/issues/1636) | LOW |
| S6 | Analysis-only lean profile via env var (`APTU_PROFILE`, JUDGE_PROVIDER pattern) | REJECTED | Two prior attempts removed (#1278 removed `APTU_CODER_PROFILE`; wave10 profile demoted #805). With S3+S4 landing, a profile is redundant for a 7-tool surface. Keep the #1605 successor note as the only path if evidence later demands it | none |
| S7 | Crossover threshold "<~20k LOC / grep-shaped tasks favors native" | DIRECTIONAL | Publish no number until v19 Track C quantifies the curve; v19 already anchors both ends (Track C lookup control, Track A structural) | gate |

## Evidence Detail

### Always-on surface (live enumeration, `target/release/aptu-coder`, chars/4 estimate)

| Tool | total ~tok | composition |
|---|---|---|
| analyze_file | 1,733 | desc 470ch, input 1,393ch, outputSchema 3,957ch |
| analyze_symbol | 1,714 | desc 459ch, input 2,465ch, outputSchema 2,930ch |
| edit_replace | 1,300 | outputSchema 1,807ch |
| exec_command | 892 | outputSchema 1,973ch |
| analyze_directory | 738 | |
| analyze_module | 704 | |
| edit_overwrite | 298 | |
| **Total** | **~7,382** | + instructions ~223 tok |

Fixed per-session cost ≈ **7.6k tokens** regardless of repo size -- the entire v18 loss on small tasks, before the first tool call.

### Per-call reality (local JSONL telemetry, ~39k events, 2026-09-25)

Median `output_chars` per successful call: `exec_command` 973 (p90 6,723), `analyze_file` 884, `analyze_module` 640, `analyze_directory` 184, `edit_replace` 145, `edit_overwrite` 117. Per-call responses are **not** the problem -- duplication inside them (S1, S2) is. Every exec response currently carries its stdout roughly twice.

### pi surfacing economics (pi-mcp-adapter 2.37.0; wiring smoke executed 2026-09-25, runner run IDs `8746bb46…`/`41f620c2…`)

- `directTools: true` (current docs default): all tools inlined with schemas → ~4.7k first-turn tokens; measured baseline native 39 vs MCP 4,708.
- `directTools: false`: **measured 3,106 first-turn input tokens** (wiring-smoke session, glm-5.3-flash); zero aptu tools inlined, gateway only. The #1619 <1.5k target is not reachable at the first-turn-input level — pi's built-in system prompt and native tools account for the floor — but the marginal cost of aptu-coder's presence drops from ~4.7k to under ~0.7k over that floor.
- `directTools: "search"`: **measured 2,821 first-turn input tokens**; only the search tool surfaced.
- Both probe arms are wired into `scripts/bench_v18/runner.py` with metering, blinding, isolation tests (#1621); wiring smokes now PASS. 1-pair smokes (search first-attempt activation ≥80%, multi-turn amortization) remain before flipping the documented default.

### Decision support (typed judgment, Jev jev-1.13.0)

Ranked five candidate levers (schema strip, gateway default, lean-profile env, response dedup, instructions trim) on savings/risk/KISS: response dedup selected (p=0.68); gateway default second (p=0.29); profile env rejected (p=0.00). Run-probes-first gating: 0.64. Crossover-threshold publishability: 0.47 -- consistent with S7's wait-for-v19 posture.

### Constraint register

- F8 (2026-09-19): no tool removals from the default surface -- S1-S5 all comply.
- exp1/exp2: description-verbosity trims are null-to-negative for accuracy -- S3 targets outputSchema, which no prior experiment covered; gate it on an ablation like f6bfe21's.
- #1605 successor note: any future surface selection must be a startup env selector with byte-identical contract -- S6 records why not now.

## Suggested sequencing

1. **Run the built `mcp-gateway`/`mcp-search` probe arms** (wiring smokes PASS 2026-09-25; remaining: 1-pair smoke with search first-attempt activation ≥80% — targets per issue #1619). [#1637](https://github.com/clouatre-labs/aptu-coder/issues/1637)
2. **S1+S2 response dedup** ([#1633](https://github.com/clouatre-labs/aptu-coder/issues/1633), [#1634](https://github.com/clouatre-labs/aptu-coder/issues/1634)); before/after via `est_output_tokens`; S2 relative-paths change is behavior-visible and ships directly per alpha policy. Regression guardrails verified: output schemas must be regenerated in lockstep (`structuredContent` MUST match `outputSchema`); `exit_code`/SEP-2164 contract untouched.
3. **S5 instructions trim** ([#1636](https://github.com/clouatre-labs/aptu-coder/issues/1636)).
4. **v19 execution** (Track C anchors the crossover; S7 resolved by data).
5. **S3 outputSchema slimming** ([#1635](https://github.com/clouatre-labs/aptu-coder/issues/1635)), ablation-gated; KISS path is a recursive `description`-strip post-processor over the `schema_for_type` result (verified: `description` is not data; conformance unaffected; rustdoc keeps doc-comments).
6. **S4 documentation default** if probes support it (part of [#1637](https://github.com/clouatre-labs/aptu-coder/issues/1637)); revisit S6 only on contrary evidence.
