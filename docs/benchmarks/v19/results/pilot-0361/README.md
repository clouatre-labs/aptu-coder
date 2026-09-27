# v19 stage-3 pilot — branch `bench/v19-calibration-0361`

Outcome: **HALTED by the F3 activation gate** during the fanin Track C
batch (designed fail-closed halt, amendment A1/F3). The control tier was
**not run**. Total spend: **$0.1766** (cap was $0.60).

## Manifest facts

- aptu-coder: 0.36.1
- pi: 0.87.1
- snapshot: dd6f6b1 (sha256 9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e)
- repo HEAD: b5aa382add07a3323a41a8e2e42570a5d321b645
- provider/model: zai / glm-5.3-flash
- amendments: A1 (fair design), A2 (judge/Jev layer dropped — no judge calls made)
- carry-overs: 300s wait timeout, per-session turn cap (fanin=25), Track A file:line anchor rewording
- end-to-end dry run (1 pair) passed before spend: JSONL extraction, scoring, artifacts all validated

## Per-tier / per-arm results

Tier: fanin (turn cap 25). Track A = 8 tasks × {native, mcp}; Track C = 1 of 8 tasks completed before the gate halt (3 arms).

| Tier | Arm | Sessions | Activation (≥1 aptu call) | Kills | Defects | Cost |
|---|---|---|---|---|---|---|
| fanin A | native | 9 (incl. 1 dry-run task) | n/a (control arm) | 0 | 0 | $0.0794 |
| fanin A | mcp | 9 (incl. 1 dry-run task) | **8/9 = 89%** | 0 | 0 | $0.0949 |
| fanin C | native | 1 | n/a | 0 | 0 | $0.0006 |
| fanin C | mcp | 1 | 0/1 | 0 | 0 | $0.0013 |
| fanin C | mcp-gateway | 1 | 0/1 | 0 | 0 | $0.0023 |

No session was killed; no metering or turn-cap defects occurred; no
wait-timeout or turn-cap threshold trips (0 of the ≥3 stop condition).

## Kept set (discriminative filter)

1 task kept: `task-fanin-dict` (out of 8 fanin-A pairs evaluable).

## F1 / categorical

- fanin Track A mean F1: native 0.254, mcp 0.206 (all 16 pair-sessions scored by the AST oracle)
- Track C: single completed task (`task-fanin-c-lookup-F`) — native/mcp scored no-anchor; mcp-gateway partial (F1 0.4)
- Verdict distribution (fanin A): native 3 fabricated-anchor / 1 partial / 1 incorrect / 4 no-anchor; mcp 3 fabricated-anchor / 2 partial / 1 incorrect / 3 no-anchor
- Note: 9 sessions ended with last stopReason `toolUse` (model exited mid-tool-loop without a final answer); these score as no-anchor under the deterministic oracle. Native 4, mcp 5.

## Crossover observations

- fanin A: mcp costs more per arm ($0.0949 vs $0.0794, ~1.2×) at lower
  mean F1 (0.206 vs 0.254) — no quality/cost crossover in favor of the
  tool arm on this (partial) sample.
- Cost-at-iso-quality cannot be assessed on Track C: gate halted after
  the first task.

## Activation-gate verdict

- fanin Track A batch: PASS — 8/8 scorable mcp sessions active (100%).
- fanin Track C batch: **FAIL — 0/2 scorable tool-arm sessions active
  (0% < 80%)** → ladder halted per F3, fail-closed. Prompts were not
  touched; the halt is recorded as a designed outcome. Root signature:
  hop-1 lookup tasks are answered directly from context without any
  repository-analysis tool call.

## Code fixes during the pilot (this branch)

- `scripts/bench_v19/pilot.py`: `extract_paths` crashed with
  `ValueError` when the final answer mentioned a path outside the
  snapshot (observed live: `/private/tmp/super_anchors.txt`).
  Non-subpath candidates are now skipped instead of crashing.
  That crash had aborted the first fanin-A invocation after 14
  completed sessions; those sessions were retained and re-scored
  offline (deterministic oracle re-run, no extra LLM spend).
