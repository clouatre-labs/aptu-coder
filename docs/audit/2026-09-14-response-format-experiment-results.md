# response_format Collapse: Experiment Results (Issue #1545)

> **Status: Executed; decision NO-GO — keep the 4-param response-shaping surface.**

Date: 2026-09-14
Related: [Experiment design](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/audit/2026-09-14-response-format-experiment-design.md), [Cognitive-load audit F5](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/audit/2026-09-14-cognitive-load-audit.md), [issue #1545](https://github.com/clouatre-labs/aptu-coder/issues/1545), [issue #1543](https://github.com/clouatre-labs/aptu-coder/issues/1543), [param-description experiments](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/audit/2026-09-11-param-description-experiments-audit.md)

## What ran

The A/B experiment specified in the [design doc](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/audit/2026-09-14-response-format-experiment-design.md)
was executed on the external
[param-description-experiments](https://github.com/clouatre-labs/param-description-experiments)
harness (experiment `exp2-response-format`, all raw data, scores, and analysis live there
under `experiments/exp2-response-format/`; this entry records the results).

- **Arms:** baseline (current 4-param surface; fixtures are the real schemars
  serialization of `AnalyzeFileParams`/`AnalyzeSymbolParams` at this worktree's
  origin/main HEAD, post-#1543 `mode` enum) vs collapsed (single payload-carrying
  `response_format` enum per the design doc's mapping table, including `Full` =
  tri-state auto-summarize semantics).
- **Grid:** 2 arms x 2 models (claude-haiku-4-5-20251001, claude-sonnet-5) x 8
  tool-bound prompts x 5 runs = 160 calls = 40 observations per cell per model, meeting
  the design doc's n >= 40. (A plan-text slip said "320 calls"; the correct grid is
  2 arms x 2 models x 8 prompts x 5 runs = 160.)
- **Calling:** `OPENROUTER_API_KEY` via OpenRouter's async Batch API; no explicit
  temperature/top_p/top_k. Pilot (one call per cell/model) clean before the run.
- **Scoring:** arm-blind over the union surface; four-way categorical intent checks
  plus un-pooled trap checks, per the experiment's `rubric.md`.

## Telemetry non-degeneracy check (pre-registered)

Already complete before this run (recorded here per the design doc): 17 real
`analyze_symbol` calls in the JSONL metrics; `impl_only`, `match_mode` beyond default,
and `def_use` were never set — all-degenerate co-occurrence. Flat enum (no nested
variants) is safe from a telemetry standpoint; the NO-GO below makes this moot for now.

## Results

Primary test — Mann-Whitney U, collapsed (b) vs baseline (a) on `param_fill_score`,
two-sided, per model:

| Model | mean a | mean b | U | p | r (95% CI) |
| --- | --- | --- | --- | --- | --- |
| claude-haiku-4-5-20251001 | 0.65 | 0.75 | 880.0 | 0.335 | -0.10 [-0.30, 0.10] |
| claude-sonnet-5 | 0.95 | 0.875 | 740.0 | 0.242 | 0.075 [-0.05, 0.20] |

Secondary test — same on `usage.input_tokens` (schema token cost):

| Model | mean a | mean b | U | p | r |
| --- | --- | --- | --- | --- | --- |
| claude-haiku-4-5-20251001 | 2419.6 | 2339.6 | 0.0 | 1.2e-14 | 1.00 |
| claude-sonnet-5 | 3029.8 | 2899.8 | 0.0 | 1.2e-14 | 1.00 |

Tool-selection accuracy: baseline 0.938 (sonnet 1.00, haiku 0.875) vs collapsed 0.90
(sonnet 0.925, haiku 0.875).

Failure modes: haiku-baseline failed the `impl_only` prompt 5/5 (empty/degenerate
calls); both arms failed the `import_lookup` prompt's pre-registered
`follow_depth`-absence trap at similar rates (a: 6/40, b: 5/40 — models set
`follow_depth: 0`, a harmless value the server ignores, penalized symmetrically).
Post-hoc sensitivity (dropping that trap check entirely, both arms) does not change the
conclusion: haiku 0.875 vs 0.75, p = 0.157; sonnet 0.90 vs 1.00, p = 0.043 (a
significant regression for the collapsed arm).

## Decision: NO-GO

The GO gate required a statistically significant accuracy win with no token-cost
regression. Neither model shows a significant accuracy win (haiku directional +
0.10, p = 0.335; sonnet directional -0.075, p = 0.242; sonnet flips to a significant
regression under the sensitivity check). The collapsed enum does significantly reduce
per-call input tokens (~80-130 tokens/call, p ~ 1e-14) — a real cost improvement, but
not sufficient under the pre-registered gate.

Per the keep-and-document gate: the 4-param surface (`summary`, `fields`, `mode`,
`impl_only`) is kept. The cognitive-load audit's F5 finding stands as measured, not
fixed: the combination-space cost is real, but collapsing it does not measurably help
agents fill parameters and directionally hurts the strongest model. No enum rollout PR
is planned; if the surface is revisited (e.g. after #1543 adoption matures in
telemetry), this experiment's fixtures and rubric are reusable as-is.

## Raw data

- Harness repo: `param-description-experiments/experiments/exp2-response-format/`
  (`raw/`, `label-map.json` sealed 2026-09-14, `scores.json`, `analysis.json`,
  `protocol.md`, `rubric.md`, `prompts.json`, `fixtures/`).
- Harness changes: cells derived from fixture files, per-cell multi-tool fixtures,
  arm-blind exp2 scoring in `recipe/scorer.py`, two-arm MWU + bootstrap CIs in
  `recipe/analyze.py`. exp1 artifacts untouched (read-only).
