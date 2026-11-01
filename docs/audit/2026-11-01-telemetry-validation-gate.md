# 2026-11-01 Telemetry Validation Gate for the T1-T3 Error-Taxonomy Fixes

Issue: [#1780](https://github.com/clouatre-labs/aptu-coder/issues/1780) (evidence-only; stays open until validated)
Fixes under validation: #1766 / #1767 (T1-T3), shipped in v0.41.0 / v0.41.1

## Window

The post-fix telemetry window opens **2026-11-01**, the day v0.41.1+ builds began accumulating steady daily use. Validation runs on or after **2026-11-08** (one full week), or later if the minimum sample is not met.

## Methodology

1. Concatenate all post-fix `metrics-*.jsonl` files covering the window (schema and retention per [METRICS.md](../METRICS.md)).
2. Run the per-tool error-rate and `error_subtype` distribution queries using the jq recipes in [METRICS.md](../METRICS.md).
3. Compare against the pre-fix October 2026 baselines (46,342 events) recorded in issue #1780.
4. Post the exact query outputs and window dates as a comment on #1780 with an explicit pass/fail verdict per criterion.

Minimum sample: at least 50 `analyze_symbol` result events must exist in the window before drawing conclusions. If the sample is smaller on 2026-11-08, extend the window instead of concluding.

## Criteria (from issue #1780)

| # | Criterion | Pre-fix baseline |
|---|-----------|------------------|
| 1 | `analyze_symbol` error rate < 10% of its result events | 86% (547 errors vs 91 ok) |
| 2 | Zero `error_subtype=null` events on `analyze_file` / `analyze_module` invalid_params responses | 146 / 62 events, all `error_subtype=null` |
| 3 | `edit_replace` `batch_validation_failed` split into four distinguishable component subtypes | 119 events collapsed into one class |

Status as of window open: **all three criteria are data-dependent and cannot be evaluated yet**. The v0.41.1+ sample is too thin (zero `analyze_symbol` calls, 19 `analyze_file` calls with 4 errors). No verdict is claimed in this document; the verdict lives in the evidence comment on #1780 after the window closes.

## Outcome Handling

- Criteria pass: close #1780 with the evidence comment.
- Any criterion fails: open a separate fix issue per failure, link it from #1780, and leave #1780 open until the follow-ups land and re-validation passes.

No code, schema, retention, or jq-recipe changes are in scope for this gate.
