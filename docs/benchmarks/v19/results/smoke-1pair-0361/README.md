# v19 Stage 2 — 1-Pair Smoke on aptu-coder 0.36.1 (2026-09-26)

Execution of the v19 benchmark ladder Stage 2 (1-pair smoke) for issue
#1681, against aptu-coder **0.36.1**, through the full runner pipeline:
v19 adapter invocations, opaque uuid run IDs, per-arm shadow
PI_CODING_AGENT_DIR, session JSONL archival, fail-closed metering, v18
blinding, and v19 F1 set-recall scoring.

> **WATCH-TRIP (prominent):** the **native** Track A session hit the
> runner's built-in single-invocation wait window and was killed
> mid-turn. Run `eb45b98b2e804f95a2f27dcd96aa647e`; spend at kill
> **$0.004835**; last assistant `stopReason: toolUse` (no final text;
> session ends on a toolResult awaiting the next turn). Wall time 120.0s
> (19:40:31.328Z → killed exactly at the `meter_and_close`
> `proc.wait(timeout=120)` deadline). No threshold was re-cut; per the
> stage protocol this WATCH-TRIP halts the stage here.

> **Metering defect (prominent):** that kill was recorded as
> `killed: false, defect: null` because the session cost parsed
> successfully. `meter_and_close` kills at the 120s wait deadline but
> has no code path distinguishing "wait-timeout kill" from "finished".
> The runner needs a wait-timeout branch that sets a defect (e.g.
> `turn-limit-kill`) — otherwise turn-limit trips are invisible in the
> ladder's defect ledger. Filed here as a runner gap for issue #1681
> follow-up; not patched in this stage (no thresholds re-cut).

## Manifest

- pi 0.87.1 · aptu-coder 0.36.1 · provider `zai` · model `glm-5.3-flash`
- git HEAD: `ab12d45d31386ce8f41df4aaae3d225cf50801ff` (main)
- Snapshot: Django pinned commit `dd6f6b1531984823e3dc56740dfa93f3ceb09357`,
  tarball sha256 prefix `9dc904f5a45be0ee` (re-verified), sessions cwd
  `/tmp/v19-wiring/django` (full checkout — see stage 1 Deviations)
- Runner: `scripts/bench_v19/runner.py` adapter over
  `scripts/bench_v18/runner.py`; stage `smoke` cap **$0.10** enforced
  fail-closed (`run_stage_budget_ok` pre-flight + `meter_and_close`
  per-session); per-session kill $0.25 (never tripped)
- Blinding: v18 `blinding.py` — opaque uuid run IDs
  (`uuid.uuid4().hex`), sealed `label-map.json`, `write_scores`
  sanitizer + forbidden-key scan
- Scoring: `bench_v19.score` F1 set-recall (`score_task` for Track A;
  set-exact categorical for the Track C control)
- Raw artifacts: `/tmp/v19-smoke2/run1/` (`summary.json`, `scores.json`,
  `label-map.json`, `rescore.json`, `sessions/<run_id>/...`)
- Driver: `scripts/bench_v19/smoke_1pair.py` (stage 2 record)

## Tasks

- **Pair (Track A, hop depth 1):** `task-q-a-hop1-constant_time_compare`
  — "List every file in this repository that calls constant_time_compare
  within 1 hop(s) of indirection. Report file paths only." Oracle: 6
  caller files (expected_sha `96fc64dd39048490`). Exercises the F1
  set-recall path with native + mcp arms.
- **Tax-control cell (Track C, hop 1):** `task-c-lookup-constant_time_compare`
  with the `mcp-gateway` arm (`directTools: false`), permitted on hop-1
  Track C cells per `arm_allowed_for_cell`. Budget allowed inclusion.

## Per-session results

| Arm | Task | Run ID | Tokens (in/out/cache-read) | Cost | stopReason | Verdict | Precision | Recall | F1 |
|---|---|---|---|---|---|---|---|---|---|
| native | A hop1 constant_time_compare | `eb45b98b2e804f95a2f27dcd96aa647e` | 10,125 / 5,300 / 22,208 | $0.004835 | **toolUse (killed @120s)** | no-anchor | 0.0 | 0.0 | 0.0 |
| mcp | A hop1 constant_time_compare | `c8a342edffcf4847ab5541a1f6b0b285` | 7,202 / 2,030 / 26,560 | $0.002892 | stop | fabricated-anchor | 0.857 | 1.0 | 0.923 |
| mcp-gateway | C hop1 constant_time_compare | `f4a04158a5f247feb665ac2512d498d2` | 2,663 / 156 / 3,200 | $0.000573 | stop | correct | 1.0 | 1.0 | 1.0 |

Answer-set details (joined post-unblinding):

- **mcp:** 7 paths — all 6 expected callers plus
  `django/utils/crypto.py` (the definition file itself). Recall 1.0;
  the extra non-expected file makes it a fabricated anchor per the
  scorer (`fabricated-anchor` overrides the F1 ≥ 0.9 "correct" band).
  F1 0.923.
- **mcp-gateway:** reported the absolute path under `/private/tmp/...`;
  normalized to `django/utils/crypto.py` = oracle. Verdict correct.
- **native:** killed before emitting any final text → `no-anchor`
  (defined verdict, scorer did not raise).

## Blinding leak check

`/tmp/v19-smoke2/run1/scores.json` contains only `expected_sha`,
`f1`, `precision`, `recall`, `task_id`, `verdict`. Forbidden-key scan
(`arm`, `model`, `run_id`, `label`, `labels`, `run-label`) over the
emitted blob: **zero hits — PASS**. The arm↔run-id join lives only in
the sealed `label-map.json`.

## Metering

| Session | Cost (usage.cost, fail-closed) | Kill |
|---|---|---|
| native | $0.004835 (well-formed) | 120s wait-timeout kill, **not recorded as defect** (gap above) |
| mcp | $0.002892 | none |
| mcp-gateway | $0.000573 | none |

**Cumulative spend: $0.008301** against the $0.10 stage cap (8.3%;
0 per-session $0.25 kills; ladder not halted by budget). Stage stopped
anyway per the WATCH-TRIP protocol.

## Deviations

1. **Extraction fix, not a threshold change:** the driver's initial
   answer-path regex broke on `v19-wiring` (dash) and on absolute
   `/private/tmp/...` paths (macOS `/tmp` symlink). Scoring was redone
   offline over the same archived JSONLs (zero additional cost); no
   session was re-run and no threshold touched.
2. **Tax-control cell included** on a hop-1 Track C cell (budget
   allowed: $0.0006 marginal).
3. Same snapshot/checkout deviations as stage 1 (full pinned checkout,
   no `.git`; pin by verified tarball sha256).

## Verdict

Stage 2 **HALTED on WATCH-TRIP**: the native Track A session reproduced
the v18 single-invocation turn-limit phenomenon (killed at the runner's
120s window with $0.0048 spent, no answer emitted), and the runner
fails to ledger such kills as defects. mcp scored F1 0.923 with
`fabricated-anchor` (definition file over-reported), mcp-gateway
correct on its control cell. Blinding leak check PASS; cumulative spend
$0.008301 / $0.10 cap. **Do not proceed to stage 3** until the runner
records wait-timeout kills as defects and the turn-limit trip is
adjudicated per the ladder runbook.
