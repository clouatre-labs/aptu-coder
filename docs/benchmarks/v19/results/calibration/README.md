# v19 calibration run — aptu-coder 0.36.1 (#1686, gates #1681 stage 1)

**Date:** 2025-09-26 · **Verdict: FREEZE GATE FAILED — adjudication layer NOT frozen. No pilot spend.**

## Setup

- Judge model pin: `jev-1.13.0` (echoed verbatim in every answer record; 30/30 identical, no fallbacks, 0 retries, 0 errors)
- Anchors: 30 (all real sites in the pinned Django snapshot `dd6f6b1`, `/tmp/v19-wiring/django`); no archived stage-2 citations were usable (stage-2 answers were no-anchor), so anchors are synthetic-but-real snapshot sites for the 15 task symbols
- Role coverage: call_of_target 7, definition_of_target 7, other_symbol_same_name 6, textual_mention_only 5, unrelated 5
- Judge calls executed through the MCP judge tool with the exact payloads from
  `calibrate.py --emit-requests` (see `requests.json`); answers recorded verbatim in `judge-answers.json`

## Agreement (calibrate.py --score, exit 1)

| role | agreement | n |
|---|---|---|
| call_of_target | 1.00 | 7 |
| definition_of_target | 1.00 | 7 |
| other_symbol_same_name | **0.00** | 6 |
| textual_mention_only | 1.00 | 5 |
| unrelated | 0.60 | 5 |
| **overall** | **0.733** | 30 |

Gate (≥0.9 overall, ≥0.75 per role): **FAILED**.

## Failure analysis (every disagreement hand-verified against the snapshot)

| anchor | human | judge | adjudication |
|---|---|---|---|
| `_redirect_to_login` call sites ×2 (#20, #21) | other_symbol_same_name | call_of_target | judge wrong — near-name confusion (`_redirect_to_login` vs `redirect_to_login`) |
| `geos_version_tuple()` sites ×2 (#22, #23) | other_symbol_same_name | call_of_target | judge wrong — near-name confusion (`geos_version_tuple` vs `get_version_tuple`) |
| `inspect.ismethod` line, target `get_func_args` (#28) | unrelated | definition_of_target | judge wrong — nothing defines the target there |
| `def decorator(view_func):`, target `redirect_to_login` (#19) | other_symbol_same_name | definition_of_target | both wrong — taxonomy gap: no role covers *defining* a different symbol |
| `def captured_stderr():`, target `captured_stdout` (#24) | other_symbol_same_name | unrelated | ambiguous — criteria for `other_symbol_same_name` says "a **call** to a different symbol"; this is a definition with a near name |
| docstring inside `def patch_vary_headers` (#29) | unrelated | definition_of_target | judge right, human label wrong |

## Findings for #1681 (amend or drop — decision requested)

1. **Dominant defect: near-name confusion.** Jev classifies calls to similarly-named
   *other* symbols as `call_of_target` (5 of 8 disagreements). The frozen protocol maps
   that verdict to `true-positive` when in the oracle set, or worse, penalizes correct
   precision when out — either way the adjudication layer would corrupt the F1 layer.
2. **Taxonomy gap:** `other_symbol_same_name` requires a *call*; definitions of
   similarly-named symbols fall outside all five roles.
3. Human labeling also contributed (1 clear mislabel, 2 ambiguous items) — the label set
   itself needs tighter construction guidance for near-name cases.

Candidate amendments (maintainer decision, then a fresh calibration run required):
- (a) split/extend the taxonomy, e.g. a `similarly_named_other_symbol` role covering both
  calls and definitions, with criteria text explicitly requiring callee-name equality;
- (b) strengthen `ANCHOR_ROLE_CRITERIA` wording to demand the cited callee string equal
  the target symbol exactly;
- (c) drop the adjudication layer and rely on the AST/F1 layer alone (precision-penalty
  for all non-oracle anchors, as pre-v19).

## Files

- `labels.json` — 30 labeled anchors (human labels; 3 flagged ambiguous above)
- `requests.json` — judge payloads + model pin (emitted by calibrate.py)
- `judge-answers.json` — verbatim judge answers (choice, confidence, probabilities, model, usage)
- Labels/judge request construction agent transcript archived in-session; no LLM-spend
  beyond judge calls (~30 × <100 output tokens).
