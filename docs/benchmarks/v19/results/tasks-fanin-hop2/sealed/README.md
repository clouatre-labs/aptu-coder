# Sealed stage — tasks-fanin-hop2 (v19 pilot ladder)

Sealed run of the 8-task fanin/hop-2 Track A pool (see
`../tasks-track-a-hop2-a4.json`), 2 arms (native, mcp), 16 sessions,
executed 2026-09-29 per amendments A1–A7 (see `../../AMENDMENTS.md`).

## Provenance

- Binary: `aptu-coder` **0.37.0** (workspace build of `main` at
  `da0686d`, includes the #1692 call-graph cache fix; A5 oracle gate
  re-run against this binary before spend: pass, results identical to
  the recorded gate).
- Harness: pi 0.87.1; provider zai, model glm-5.3-flash.
- Snapshot: django `dd6f6b1`, sha256 `9dc904f5…` (frozen).
- Scorer: post-A6 (snapshot-verified fabricated anchors); turn cap 40
  per A7; `SESSION_WAIT_TIMEOUT_S=900` per A7b.
- Command: `scripts/bench_v19/pilot.py --tasks …/tasks-track-a-hop2-a4.json
  --tier fanin --arms native,mcp --cells hop2 --snapshot …django-dd6f6b1…
  --run-root /tmp/v19-pilot/run-sealed-hop2-b`.

## Spend

- Valid run (`summary.json`): **$0.2283** (mcp $0.0952, native $0.1331).
- Invalidated first attempt (`invalidated-attempt1/`, silently
  truncated by the 300s wait deadline, A7b): $0.1482 metered.
- Sealed-stage total: **$0.3765** (cap $2.00).

## Headline (descriptive; see crossover.csv / summary.json)

| arm    | mean F1 | completed (stop) | notes |
|--------|---------|------------------|-------|
| mcp    | 0.432   | 5/8              | 3 sessions ended on provider `error`; 1 wait-timeout defect (visible per A7b) |
| native | 0.306   | 7/8              | 1 provider `error` |

F3 activation gate: pass (6/7 active = 0.857; `pending` recorded at 7
scorable sessions < min sample 8, passes vacuously per A4b).

Per-task verdicts (mcp arm): correct ×2 (skipIfDBFeature 0.953,
register_lookup 0.811), fabricated-anchor ×2, partial ×1, no-anchor ×3
(2 of which ended on provider `error`, not model choice).

## A8 repair re-run (`repairs/`)

The 4 provider-`error` sessions above were re-run on 2026-09-29 (same
binary 0.37.0, same SHA256-verified snapshot, 900s wait deadline) per
[AMENDMENTS A8](../../../AMENDMENTS.md); filtered task files lived
under `/tmp` (frozen set untouched). Repair spend: **$0.0881**.
See `repairs/summary.json`, `repairs/before-after.json`,
`repairs/arm-means.json`, `repairs/sessions/`.

- **Repaired (replaced):** dec/mcp (F1 0.0 → 0.129), include/native
  (F1 0.0 → 0.795).
- **Not repaired (kept as-sealed):** chain/mcp — repair hit the A7
  turn cap 40; timezone/mcp — repair hit the 900s wait deadline again
  (timezone/mcp appears structurally slow under the mcp arm, not a
  transient provider failure).

Repair-adjusted means (error sessions replaced only where the repair
completed): mcp 0.4482 (6/8 completed), native 0.4051 (8/8
completed) vs as-sealed 0.4321 / 0.3057 (5/8, 7/8).

## Known caveats (recorded, not silently dropped)

- 4 mcp + 1 native sessions ended with `stopReason: "error"`
  (provider-side failures mid-loop); they are scored on their partial
  transcripts. This depresses mcp-arm completion (5/8 vs native 7/8).
- `timezone`/mcp was killed at the 900s wait deadline (defect
  recorded, spend metered) — the only structural non-completion.
- Native-arm fabricated-anchor verdicts (chain, qualname) are genuine
  per the A6 verifier: cited `path:line` anchors whose line lacks the
  symbol, not gold-set misses.
