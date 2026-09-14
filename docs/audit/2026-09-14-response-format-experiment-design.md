# response_format Collapse: Experiment Design (Issue #1545)

> **Status: Proposed design; not yet executed; rollout gated on experiment outcome and #1543.**

Date: 2026-09-14
Related: [Cognitive-load audit F5](2026-09-14-cognitive-load-audit.md), [issue #1545](https://github.com/clouatre-labs/aptu-coder/issues/1545), [issue #1543](https://github.com/clouatre-labs/aptu-coder/issues/1543), [param-description experiments](2026-09-11-param-description-experiments-audit.md)

## Purpose

The cognitive-load audit (finding F5) flags the four-way response-shaping surface of
`analyze_file` — `summary` (tri-state), `fields`, `def_use`, `impl_only` — as a
combination-space the reading model must reason about on every call, on top of
orthogonal params (`cursor`, `match_mode`, `follow_depth`). This design specifies a
payload-carrying `response_format` enum that collapses the four response-shaping
parameters into one value, and an A/B experiment to decide whether the collapse
actually helps agents before any schema change ships.

**This PR implements only this design document and one behavior-locking integration
test. No schema, handler, or parameter changes.** Implementation is gated on the
experiment win (see Decision gates) and sequenced after #1543 lands.

## Proposed enum shape

```text
ResponseFormat (payload-carrying, default = Full):
  Full       default; preserves today's tri-state summary=None semantics,
             including auto-summarize above the 50K-char output threshold
  Summary    equivalent to summary=true
  Fields(Vec<AnalyzeFileField>)   equivalent to fields=[...]
  DefUse     equivalent to def_use=true
  ImplOnly   equivalent to impl_only=true
```

Non-negotiable semantic constraint: the default (`Full`) must preserve the tri-state
`summary=None` behavior — auto-summarize at >50K output chars — not a forced
always-Full render. Mapping unset `summary` to an unconditional full render would
change large-file behavior and is out of scope.

Orthogonality caveats: `cursor` (pagination) composes with full and projected output
but not with summary; `match_mode` and `follow_depth` are lookup semantics, not
response shaping, and stay top-level regardless of the outcome. If telemetry shows
non-degenerate co-occurrence of a response-format payload with these params (see
Telemetry), the enum stays nested rather than flattened.

## Mapping table (current surface -> enum)

Exhaustive over the four response-shaping parameters in their current combinations:

| summary | fields | def_use | impl_only | -> enum value | Loss? |
| --- | --- | --- | --- | --- | --- |
| unset | unset | unset | unset | `Full` (auto-summarize >50K preserved) | none |
| false | unset | unset | unset | `Full` | none |
| true | unset | unset | unset | `Summary` | none |
| unset/any | Some | unset | unset | `Fields(vs)` | none |
| unset/any | unset | true | unset | `DefUse` | none |
| unset/any | unset | unset | true | `ImplOnly` | none |
| true | Some | unset | unset | `Summary` | **behavior-preserving narrowing**: today `fields` is silently ignored when `summary=true`; the enum reproduces this exactly. Locked by `crates/aptu-coder/tests/analyze_file_fields_summary.rs`. Not lossy. |
| true | Some | true/true | any | impossible today | Today `summary=true` short-circuits before def_use/impl_only rendering; no loss. |
| Some | Some | true | unset | `Fields(vs)` or `DefUse` — today mutually exclusive | Current handler resolves the conflict; enum forces one payload. Needs the same conflict rule as today (documented at implementation time). |
| true | unset | unset | true | `Summary` or `ImplOnly` — same resolution as above | Same. |

Summary of losslessness: the only combination whose output depends on parameter
ordering today is `fields` + `summary=true`, and there the observed behavior is that
summary wins and fields is dropped. That behavior is deterministic (integration test
in this PR) and the enum's `Summary` variant preserves it.

## A/B experiment

Run on the external param-description-experiments harness pattern (see
[2026-09-11 audit](2026-09-11-param-description-experiments-audit.md)); this repo
cannot execute model-in-the-loop benchmarks itself.

- **Arms:** baseline (current 4-param surface) vs collapsed enum surface, on both
  `analyze_file` and `analyze_symbol` tool listings.
- **Sample size:** n >= 40 tool calls per cell per model.
- **Models:** two Claude models (current production and next-tier), same task
  prompts across arms.
- **Metrics:** parameter-filling accuracy (correct combination for the task intent,
  including the >50K auto-summarize case) and schema token cost per tool listing.
- **Statistics:** Mann-Whitney U on accuracy and on token cost; report effect sizes
  and CIs, not just p-values.
- **Recording:** results go into a follow-up docs/audit entry; this doc stays the
  canonical spec.

## Telemetry non-degeneracy check

Before implementation, use the JSONL metrics (see docs/METRICS.md) to check that no
collapsed combination is load-bearing in a way the enum cannot express:

- Per-param presence for `impl_only` and `match_mode` co-occurrence on
  `analyze_symbol`: if calls set both with meaningful frequency and distinct
  outcomes, `ImplOnly` cannot stay flat.
- `def_use` + `impl_only` co-occurrence on `analyze_symbol`: any non-degenerate
  co-occurrence (both set together with non-trivial frequency) forces a nested enum
  (e.g. `DefUse(ImplOnly)`) or keeping one param orthogonal.

Query pattern: jq over `metrics-*.jsonl` selecting `tool=="analyze_symbol"` and
tabulating truth vectors of the params above. If any pair is non-degenerate, the
design escalates to nested variants rather than dropping a capability.

## Decision gates

- **Rollout gate:** implement the enum (with outright legacy param removal, per
  alpha policy) only on a statistically significant accuracy win with no token-cost
  regression. Sequenced after #1543 lands to avoid double-churn on
  `SymbolAnalysisMode`; do not collapse `SymbolAnalysisMode` into
  `response_format` in the same change.
- **Keep-and-document gate:** on a null result, keep the 4-param surface and update
  the cognitive-load audit to record the measured negative result.

## Validation in this PR

One integration test locks the observed behavior the mapping table depends on:
`analyze_file` with `fields=[functions]` and `summary=true` succeeds and returns
summary-mode output with `fields` silently ignored. This pins the one ambiguous row
of the mapping table so the future enum implementation can be verified against it.
