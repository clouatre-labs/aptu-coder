# v20 Ablation Benchmark Results (2026-11-19)

## Overview

This report records the execution and results of the frozen v20 ablation benchmark defined in `docs/audit/2026-11-19-v20-ablation-benchmark-design.md` (merged in #1840). The 24-cell matrix was run with the locked metered runner against the pinned repo commit, adjudicated with the pre-registered rubric before any aggregate inspection, and scored with the pre-registered decision rule applied verbatim.

Pinned facts: repo commit `617aea598d859bcc84c1492a45972e7bd28c3946`, provider `zai/glm-5.3-flash` (declared deviation), fixture draft PRs #1841-#1844 (closed after collection per the frozen design).

Raw artifacts committed under `docs/audit/v20-ablation-raw-results/`: `runner-out.jsonl` (one JSON line per cell plus the halt record), `adjudications/*.json` (17 sheets), and `score-summary.json` (score.py output). Session transcripts were too large to commit; they remain on the authoring machine at `.kg-ablation-runs/sessions/` (local only, not tracked).

## Execution summary

- 17 of 24 cells completed (no-graph 8, optional-graph 8, forced-graph 1).
- The runner halted on the pre-registered stop rule: `spend-cap-exceeded` at USD 0.685527 against the USD 0.64 cap. This halt was the pre-registered stop rule firing, not an operational failure.
- 12 cells carried a non-halting per-run cap-exceeded defect marker (per-run kill cap USD 0.03).
- Mean spend USD 0.0403 per cell (min 0.0117, max 0.0739).

## Actual spend vs registered band

Total actual spend: **USD 0.685527** (cap USD 0.64). Observed per-session costs (mean USD 0.0403) ran roughly 2x above the design band (USD 0.0173-0.0221) from which the cap was derived, which is why the cap bounded early. The cap arithmetic was correct given the registered band; the band itself underestimated real session cost under this provider/model.

## Per-cell table

| Arm | Fixture | Run | Complete | cost_usd | Defect marker |
| --- | --- | --- | --- | --- | --- |
| no-graph | family1-contract-drift | 1 | yes | 0.051816 | per-run-cap-exceeded:0.0518 |
| no-graph | family1-contract-drift | 2 | yes | 0.037163 | per-run-cap-exceeded:0.0372 |
| no-graph | family2-reference-resolution | 1 | yes | 0.011652 | none |
| no-graph | family2-reference-resolution | 2 | yes | 0.021376 | none |
| no-graph | family3-pattern-mode | 1 | yes | 0.047351 | per-run-cap-exceeded:0.0474 |
| no-graph | family3-pattern-mode | 2 | yes | 0.051209 | per-run-cap-exceeded:0.0512 |
| no-graph | clean-control | 1 | yes | 0.032132 | per-run-cap-exceeded:0.0321 |
| no-graph | clean-control | 2 | yes | 0.015928 | none |
| optional-graph | family1-contract-drift | 1 | yes | 0.048106 | per-run-cap-exceeded:0.0481 |
| optional-graph | family1-contract-drift | 2 | yes | 0.046437 | per-run-cap-exceeded:0.0464 |
| optional-graph | family2-reference-resolution | 1 | yes | 0.049459 | per-run-cap-exceeded:0.0495 |
| optional-graph | family2-reference-resolution | 2 | yes | 0.028262 | none |
| optional-graph | family3-pattern-mode | 1 | yes | 0.073884 | per-run-cap-exceeded:0.0739 |
| optional-graph | family3-pattern-mode | 2 | yes | 0.061837 | per-run-cap-exceeded:0.0618 |
| optional-graph | clean-control | 1 | yes | 0.036059 | per-run-cap-exceeded:0.0361 |
| optional-graph | clean-control | 2 | yes | 0.013322 | none |
| forced-graph | family1-contract-drift | 1 | yes | 0.059534 | per-run-cap-exceeded:0.0595 |

The remaining 7 forced-graph cells (family2-reference-resolution run 1-2, family3-pattern-mode run 1-2, clean-control run 1-2, family1-contract-drift run 2) never started because the spend cap halted the run.

## Activation gate (recorded before catch interpretation)

Per the frozen design ordering, the activation gate was evaluated from the proxy mcp-logs before interpreting any F1 result.

- 8/8 optional-graph sessions show real `search_code` traffic.
- 1/1 forced-graph sessions show real `search_code` traffic.
- Natural pulls happened (adoption signal present).

**Gate verdict: PASSED.** The tier was reachable and used in the Optional/Forced arms, so downstream catch counts are interpretable as tool-ablation effects rather than wiring failures.

## Adjudication summary

Adjudication ran against `DEFECT_SYMBOLS` with the pre-registered rubric before any aggregate inspection.

- Unique catches vs no-graph: optional-graph **[]**, forced-graph **[]**.
- family1-contract-drift was caught in ALL arms (no-graph 2/2, optional-graph 2/2, forced-graph 1/1), so `rg` alone catches it; no ablation differential.
- family2-reference-resolution and family3-pattern-mode: zero catches in any arm. No arm localized either defect.
- 8 cells (family2 and family3, both arms, both runs) carry `needs_review` with partial signals: breakage phrases present, no symbol hit.
- False positives: 2 on clean-control in optional-graph (a false-reassurance risk signal); 0 elsewhere. False reassurance total: 0.

## Score tables (fixture x arm)

| Fixture | Arm | Catch | FP | FN | False reassurance |
| --- | --- | --- | --- | --- | --- |
| clean-control | no-graph | 0 | 0 | 0 | 0 |
| clean-control | optional-graph | 0 | 2 | 0 | 0 |
| family1-contract-drift | no-graph | 2 | 0 | 0 | 0 |
| family1-contract-drift | optional-graph | 2 | 0 | 0 | 0 |
| family1-contract-drift | forced-graph | 1 | 0 | 0 | 0 |
| family2-reference-resolution | no-graph | 0 | 0 | 0 | 0 |
| family2-reference-resolution | optional-graph | 0 | 0 | 0 | 0 |
| family3-pattern-mode | no-graph | 0 | 0 | 0 | 0 |
| family3-pattern-mode | optional-graph | 0 | 0 | 0 | 0 |

## Decision rule applied verbatim

The pre-registered keep condition is NOT met in the observed cells:

- Zero unique catches for either graph arm vs no-graph.
- The family2 precision comparison is unevaluable: no arm localized family2, so there is no precision signal to compare.

The matrix is incomplete: the forced-graph upper bound is missing (1 of 8 forced cells ran), and the halt was the pre-registered stop rule, not an operational failure. Per the frozen rule, borderline/incomplete results require a larger follow-up, not a keep verdict.

**Verdict: the observed evidence does not support keeping the tier.** The evidence points to the #1807 removal playbook, pending a powered decision covering the missing forced-graph cells (with a corrected per-session cost band so the cap does not bind early).

## Follow-up requirements

- A powered follow-up must complete the forced-graph arm before any final keep/remove decision that relies on the upper bound.
- The spend cap derivation must use an observed per-session band near USD 0.04, not USD 0.0173-0.0221.
- family2/family3 zero-catch results across all arms should be reviewed against the `needs_review` partial signals before the fixtures are reused.
