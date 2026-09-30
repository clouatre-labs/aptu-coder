# A9b verify-repair — fabricated-anchor cells (2026-09-30)

A9b verify-repair pass over the sealed tasks-fanin-hop2 record, per
[AMENDMENTS A9b](../../../AMENDMENTS.md). `verify_repair.py` selected
exactly the completed (stopReason `stop`, not killed, no defect)
sealed cells whose scored verdict is `fabricated-anchor` — 4 cells —
and re-ran each once (one bounded attempt) with a corrective prefix
restating the A9a word-boundary anchor rule.

Nothing under `../sealed/` was re-scored or modified; the frozen task
set was never written (filtered copy in a temp path).

## Provenance

- Binary: `aptu-coder` **0.38.0** (workspace install of `main` at
  `20d1d72`, the A9a/A9b merge; the emitted `summary.json`
  `repair_binary` field was corrected from the stale `pilot.py`
  constant 0.37.0 — see caveats).
- Harness: pi 0.99.0; provider zai, model glm-5.3-flash.
- Snapshot: django `dd6f6b1`, tarball sha256
  `9dc904f5…` **re-verified against the pinned manifest**
  (`bench_v19.snapshots.verify_snapshot`) before spend; the sessions
  cwd checkout diffed byte-identical against a fresh extraction of the
  verified tarball.
- Wait deadline 900s (A7b); turn cap 40 (fanin tier, A7).
- Command: `python3 -m bench_v19.verify_repair --summary
  …/sealed/summary.json --tasks
  …/tasks-fanin-hop2/tasks-track-a-hop2-a4.json --tier fanin --arms
  native,mcp --snapshot …/django-dd6f6b1 --run-root /tmp/v19-a9b/
  run-verify-repair --output …/a9b-verify-repair`.
- Spend before: **$0.067199** (this pass only; run-root was fresh).
  Session-wide budget across the day (smoke + this pass): $0.100258
  against the $2.00 hard cap.

## Headline

| cell (task/arm) | before F1 (verdict) | after F1 (verdict) | after fabricated | replaced | cost |
|---|---|---|---:|---|---:|
| chain/native | 0.3867 (fabricated-anchor) | — (killed @900s wait deadline, defect) | 0 | **no** | $0.0349 |
| include/mcp | 0.9167 (fabricated-anchor) | 0.8889 (fabricated-anchor) | 1 | no | $0.0147 |
| qualname/native | 0.4681 (fabricated-anchor) | 0.4000 (partial) | 0 | **yes** | $0.0030 |
| qualname/mcp | 0.4375 (fabricated-anchor) | 0.4375 (fabricated-anchor) | 3 | no | $0.0146 |

Repair-adjusted means (replacement applied): native 0.2972 (7/8
completed), mcp 0.4321 unchanged (5/8) vs as-sealed 0.3057 / 0.4321.
See `summary.json`, `before-after.json`, `arm-means.json`,
`sessions/` (archived JSONLs, one dir per run ID).

## Replacement policy (A9, verbatim)

> fabricated-anchor sessions kept; repair replaces only where the
> repair completed (stop) and re-scores with zero fabricated anchors.

Only qualname/native met both conditions. include/mcp and qualname/mcp
completed but still cite fabricated anchors under the A9a
word-boundary verifier (1 and 3 respectively), so their sealed
sessions stand. chain/native hit the 900s wait deadline mid-turn
(killed, defect `wait-timeout-killed:900s`) and stays as-sealed.

## Caveats (recorded, never dropped)

1. **Cell-selection discrepancy with RESULTS.md.** RESULTS.md line
   "5 fabricated-anchor verdicts across 20 scored sessions" counts
   across the sealed run *and* the A8 repairs ledger; the sealed run
   `summary.json` itself contains 4 completed fabricated-anchor cells
   (chain/native, include/mcp, qualname/native, qualname/mcp) — the
   population this pass is defined over. The A8 repair sessions for
   dec/mcp and include/native also scored `fabricated-anchor`, but
   their `final_text` is not preserved in the sealed record, so the
   corrective-prefix pass cannot be reconstructed for them; they are
   not part of this run and not silently dropped — flagged here.
2. **`pilot.py` `APTU_CODER_VERSION` was stale (0.37.0)** at run time;
   the emitted `summary.json` originally recorded it. Corrected to
   0.38.0 in the artifact and in `pilot.py` (factual constant update).
   All other provenance fields were recorded live by the script.
3. **Correction prefix carried no anchor list.** Sealed sessions store
   no `final_text`, so `fabricated_anchors_in` found nothing and the
   corrective prefix listed "(none)" while still restating the anchor
   rule. The repair is therefore a rule-restatement pass, not an
   anchor-by-anchor correction.
4. chain/native re-run was killed at the 900s wait deadline; its
   $0.0349 is metered, fail-closed, and its cell stays as-sealed.
5. As with A8, n=4 cells; all means are descriptive.
