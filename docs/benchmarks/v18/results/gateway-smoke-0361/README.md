# v18 Gateway-Arm Smokes on aptu-coder 0.36.1 (2026-09-26)

Execution of the #1637 gateway-arm smokes against aptu-coder **0.36.1**
(post tool-surface-trim release). The wiring-probe numbers cited in
#1637 (3,106 / 2,821 / 4,708 first-turn tokens) were measured on
0.35.1 (2026-09-22) and are superseded by this run.

## Manifest

pi 0.87.1 · aptu-coder 0.36.1 · provider `zai` · model `glm-5.3-flash` ·
fixture `docs/benchmarks/v18/fixture` (unmodified, seed 1604) · task
q-0041 (string-constant lookup, outside sealed q-0000–0039) + one
exploratory structural scenario (`s-struct`, not part of the #1637
protocol) · raw session transcripts committed under `sessions/`.

## Stage results

| Run | Stage | Arm | First-turn input | Cost | Killed | Answer correct |
|---|---|---|---|---|---|---|
| `af8b79fd…` | wiring_smoke | mcp (`directTools: true`) | 3,973 | $0.00162 | no | yes |
| `4e3f0ce2…` | smoke | mcp-gateway (`directTools: false`) | 1,749 | $0.00089 | no | yes |
| `4b915ede…` | smoke | mcp-search (`directTools: "search"`) | 1,898 | $0.00098 | no | yes |
| `249e0421…` | smoke (exploratory) | mcp-search | 1,753 (turn 2) | $0.00570 | no | incomplete (`stopReason: toolUse`) |
| `94078ab3…` | smoke (exploratory) | mcp-gateway | 1,902 (turn 2) | $0.01140 | no | incomplete (`stopReason: toolUse`) |

Total spend: **$0.0206** · kills 0 · metering defects 0.

## Findings

1. **Wiring PASS on 0.36.1.** mcp arm surfaces exactly the four
   `aptu-coder_analyze_*` tools; gateway arm surfaces only the adapter's
   `mcp__aptu_coder` proxy plus the adapter's own tools; search arm
   surfaces the search path only. Zero direct aptu tools in the gateway
   arm, as required.
2. **First-turn inputs shifted down across all arms vs 0.35.1**
   (mcp 4,708→3,973; gateway 3,106→1,749; search 2,821→1,898),
   consistent with the #1635/#1645–#1649 schema/description removals and
   the #1636 instruction trim. The gateway-vs-mcp aptu-attributable
   delta (~2.2k first-turn) survives directionally.

   **Cold-start metadata-cache contamination (measured and recorded
   explicitly, per the #1637 acceptance criterion and the harness.md
   caveat):** warmup before measurement is infeasible in this
   single-session-per-invocation runner, so the wiring-smoke session is
   treated as the discarded warmup and the smoke-stage gateway/search
   first-turn numbers above are cold-start-contaminated — they are an
   upper bound on steady-state first-turn cost. The warm cache lives in
   the per-run shadow `PI_CODING_AGENT_DIR` (`agent-mcp-gateway`/
   `agent-mcp-search` under the run root), which is not reused across
   sessions, so no pair of runs in this record isolates the warm-cache
   delta; quantifying it requires a multi-session runner change (out of
   scope for #1637, noted as a harness limitation).
3. **Search first-attempt activation: 0/4 scorable sessions (0%) vs the
   ≥80% target. FAIL.** In every session on every arm the model solved
   the task with native `bash` (`rg`) + `read` and never issued an
   `mcp` gateway/search call. This holds even under an explicit
   "without using ripgrep or grep" instruction (the model switched to
   shell loops). Diagnosis matches the stages-1–3 pilot observation:
   the fixture task family is ripgrep-optimal and adversarial to
   structured analysis; with native bash available, gateway activation
   is tool-choice-dominated, not discoverability-dominated.
4. **Structural exploratory scenario:** both sessions hit the
   single-invocation turn limit mid-investigation with no final answer.
   The current task family cannot exercise multi-turn aptu-tool
   amortization in this harness; a v19 task family of comparable
   complexity to the v12/v13 benchmarks would be needed (per the
   stages-1–3 README).

## Verdict for #1637

- Acceptance criterion "search activation ≥80% or shortfall documented
  with diagnosis": **shortfall documented** (finding 3).
- Docs default flip: **NO-GO on current evidence.** The activation gate
  failed decisively; flipping the documented pi `directTools` default
  to gateway mode is not supported by this data. Token-delta evidence
  alone favors gateway mode, but the discoverability/activation
  behavior must be re-evaluated on a non-ripgrep-trivial task family
  first.
