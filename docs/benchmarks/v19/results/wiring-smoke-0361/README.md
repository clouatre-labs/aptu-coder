# v19 Stage 1 — Wiring Smoke on aptu-coder 0.36.1 (2026-09-26)

Execution of the v19 benchmark ladder Stage 1 (wiring smoke) for issue
#1681, against aptu-coder **0.36.1**. This is the v18 gateway-smoke's
tool-surface assertion re-run on the v19 Django snapshot harness.

## Manifest

- pi 0.87.1 · aptu-coder 0.36.1 · provider `zai` · model `glm-5.3-flash`
- git HEAD: `ab12d45d31386ce8f41df4aaae3d225cf50801ff` (main)
- Snapshot: Django pinned commit `dd6f6b1531984823e3dc56740dfa93f3ceb09357`,
  tarball `/tmp/v19-snapshots/django-dd6f6b1531984823e3dc56740dfa93f3ceb09357.tar.gz`
  (sha256 prefix `9dc904f5a45be0ee`), extracted to `/tmp/v19-wiring/django`
  (full repo checkout at root — see Deviations)
- Task: `task-c-lookup-captured_stderr` (hop-1 Track C,
  prompt: "Locate the definition of captured_stderr and report its file path.",
  oracle: `django/test/utils.py`)
- Arms: native, mcp (`directTools: true`), mcp-gateway
  (`directTools: false`), per `bench_v19` `arm_allowed_for_cell`
- Runner: `scripts/bench_v19/runner.py` adapter over `scripts/bench_v18/runner.py`;
  stage cap `wiring_smoke` = $0.10, enforced fail-closed (pre-flight
  cumulative check + per-session metering via `meter_and_close`)
- Raw session transcripts: `/tmp/v19-wiring/run1/sessions/<run_id>/...`
  (session JSONL paths in `/tmp/v19-wiring/run1/summary.json`)

## Tool-surface assertions (from each session's system tool section)

| Arm | Expected | Observed | Verdict |
|---|---|---|---|
| native | no aptu tools | only `read, bash, grep, find` | **PASS** |
| mcp (`directTools: true`) | four `aptu-coder_analyze_*` direct, no aptu edit/exec | exactly `aptu-coder_analyze_directory`, `aptu-coder_analyze_file`, `aptu-coder_analyze_module`, `aptu-coder_analyze_symbol`; zero `aptu-coder_edit_*` / `aptu-coder_exec_command` | **PASS** |
| mcp-gateway (`directTools: false`) | gateway proxy surface, no direct aptu tools | only `mcp__aptu_coder` proxy (plus native pi tools); zero direct aptu tools | **PASS** |

All three sessions completed (`stopReason: stop`) and each answered with
the oracle path `django/test/utils.py`.

## Metering

Per-session cost parsed from JSONL `usage.cost.total` (well-formed in all
three sessions; a malformed/missing value would have been a kill + defect
per the fail-closed runner).

| Arm | Run ID | Session file | Tokens (in / out / cache-read) | Cost |
|---|---|---|---|---|
| native | `ea5f636a6ee747ce9e29b220df1da88f` | `2026-09-26T19-35-55-146Z_01a0df37-8e09-7181-b985-c7671388dd08.jsonl` | 2,246 / 322 / 4,032 | $0.000619 |
| mcp | `36b27e8b948c40a6bfd6400f1bc77126` | `2026-09-26T19-36-15-559Z_01a0df37-ddc7-7773-9d2f-64595545b1b1.jsonl` | 8,975 / 292 / 11,648 | $0.001842 |
| mcp-gateway | `f6e2bd5914484c16a9513628864a509c` | `2026-09-26T19-36-39-578Z_01a0df38-3b9a-73c7-9528-87df1ddb467b.jsonl` | 4,311 / 304 / 6,912 | $0.001006 |

**Cumulative spend: $0.003467** (cap $0.10, not approached; 0 kills,
0 metering defects, ladder not halted).

Directionally consistent with the v18 smoke: gateway arm first-turn
footprint is well below the mcp arm's.

## Deviations

1. **Full checkout, not `django/`-only scope.** The working tree given to
   each session is the full pinned Django checkout rather than a
   `django/`-only subtree. Rationale: the oracle anchors for the v19 task
   family include `tests/` paths, so a `django/`-only scope would make
   some anchors unresolvable. Documented here as an intentional,
   task-family-driven deviation.
2. **Snapshot dir is not a git checkout.** The tarball extraction carries
   no `.git`; the pin is guaranteed by the verified tarball sha256
   (`9dc904f5…`) and the pinned commit encoded in its filename, not by a
   local `git log`.

## Verdict

Stage 1 (wiring smoke) **PASS** on aptu-coder 0.36.1: per-arm tool
surfaces behave exactly as the PR #1621 spec requires, metering is
well-formed, and total spend is $0.0035 against the $0.10 cap. Cleared to
proceed to Stage 2 per the ladder runbook (not executed in this record).
