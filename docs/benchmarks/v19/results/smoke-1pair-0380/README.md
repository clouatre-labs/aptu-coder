# v19 Stage 2 — 1-Pair Smoke on aptu-coder 0.38.0 (2026-09-30)

Re-run of the v19 Stage 2 (1-pair smoke) against aptu-coder **0.38.0**, recorded next to the 0.36.1 smoke ([../smoke-1pair-0361/](../smoke-1pair-0361/README.md)) to flag any completion or verdict regression. Same frozen snapshot, same three cells (Track A hop-1 pair, native + mcp, plus the Track C mcp-gateway tax-control cell), full v18 pipeline with blinding and F1 scoring.

## Manifest

- pi 0.99.1 · aptu-coder **0.38.0** (`aptu-coder --version`, workspace install of main at `20d1d72`) · provider `zai` · model `glm-5.3-flash`
- git HEAD: `20d1d72` (includes #1709 word-boundary anchors, A9a)
- Snapshot: django pinned commit `dd6f6b1`, tarball sha256 `9dc904f5…` **re-verified against the pinned manifest** before spend; the sessions cwd checkout (`/tmp/v19-wiring/django`) diffed byte-identical against a fresh extraction of the verified tarball
- Wait deadline: 900s (A7b) — see driver-gap caveat below
- Raw artifacts: `/tmp/v19-smoke2/run1/` (this run); `/tmp/v19-smoke2/run1-0380-wait120-defect/` (aborted first attempt)
- Driver: `scripts/bench_v19/smoke_1pair.py` via an in-process wrapper setting `v18.SESSION_WAIT_TIMEOUT_S = 900` (A7b)

## Per-session results

| Arm | Task | Tokens (in/out/cache-read) | Cost | stopReason | Verdict | Precision | Recall | F1 |
|---|---|---|---:|---:|---|---:|---:|---:|
| native | A hop1 constant_time_compare | 16,041 / 10,098 / 29,952 | $0.008354 | stop | fabricated-anchor | 0.769 | 0.625 | 0.462 |
| mcp | A hop1 constant_time_compare | 33,713 / 12,266 / 115,008 | $0.014640 | stop | fabricated-anchor | 0.818 | 0.563 | 0.480 |
| mcp-gateway | C hop1 constant_time_compare | 8,024 / 118 / 8,000 | $0.001503 | stop | correct | 1.0 | 1.0 | 1.0 |

Blinding leak check: PASS (zero forbidden-key hits).

## Spend

- This run: **$0.024497** (cap: $0.05 expectation — met; stage cap $0.10 enforced fail-closed, not approached).
- Aborted first attempt (killed at the 120s default wait deadline, see caveat): $0.008561 metered.
- Smoke-stage total vs 0.38.0: **$0.033058**.

## Regression check vs 0.36.1 smoke

| Arm | 0.36.1 | 0.38.0 | Delta |
|---|---|---|---|
| native | **killed @120s, no answer, F1 0.0** | completed (stop), F1 0.462 | **completion regression fixed**: 0.38.0 finishes the session |
| mcp | completed, fabricated-anchor, F1 0.923 | completed, fabricated-anchor, F1 0.480 | no completion regression; F1 lower (over-reporting: 14 vs 6 non-expected paths), verdict class unchanged |
| mcp-gateway | correct, F1 1.0 | correct, F1 1.0 | unchanged |

**No completion or verdict regression.** The 0.36.1 WATCH-TRIP (native wait-timeout kill) did not reproduce at 0.38.0 under the A7b 900s deadline. Both Track A cells still carry `fabricated-anchor` verdicts, consistent with the sealed-stage finding that anchor fabrication is the dominant failure mode (now scored under the A9a word-boundary verifier).

## Caveats (recorded, not dropped)

1. **Driver gap (A7b not honored by `smoke_1pair.py`).** The driver imports `bench_v18.runner` directly and never imports `runner_v19`, so `SESSION_WAIT_TIMEOUT_S` was ignored at its default 120s on the first attempt: native and mcp were killed (`session-wait-timeout:120s`, fail-closed defects correctly recorded — the A7b fix works; the deadline override does not reach this driver). Fixed for this run with an in-process wrapper (the `probe.py` pattern). The driver itself should set the override at import time like `runner_v19` does.
2. **First-attempt spend is metered but discarded** ($0.008561); no result from it is counted above.
3. Session-to-session F1 variance at n=1 per cell is high (mcp over-reported 14 extra paths this run vs 1 in 0.36.1); the smoke is a wiring/completion check, not a quality measurement.
