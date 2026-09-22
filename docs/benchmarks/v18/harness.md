# v18 Benchmark Harness Artifacts

Status: **implemented harness, not executed.** Live ladder execution is
post-merge and maintainer-only. This document describes the five artifacts
from issue 1604 and how they fit the merged v18 methodology
([methodology.md](methodology.md)).

## Artifacts

- `scripts/bench_v18/generate_fixture.py`: deterministic (seeded) generator
  for the frozen 50k+ LOC Python/Rust/TypeScript fixture with a `_source`
  provenance field in `manifest.json`.
- `docs/benchmarks/v18/fixture/`: the committed frozen fixture, generated
  once with seed 1604. CI regenerates and compares by hash.
- `scripts/bench_v18/oracle.py`: builds `oracle.json` (40 ground-truth
  answers with anchors) from the fixture using ripgrep, falling back to
  pure-Python `re` with a loud warning when `rg` is absent. The oracle
  shares no code with aptu-coder.
- `scripts/bench_v18/runner.py`: the stage ladder
  (`wiring_smoke`, `smoke`, `pilot`, `sealed`) with fail-closed cost
  metering: a session whose JSONL usage is missing, malformed, or above
  USD 0.25 is killed and recorded as a defect; the ladder halts at the
  cumulative USD 5 ceiling and before entering any stage over its cap.
- `scripts/bench_v18/blinding.py`: opaque uuid4 run IDs, a sealed
  `label-map.json` written at run time, and a `scores.json` emitter that
  refuses to output arm, model, or label fields.
- `scripts/bench_v18/score.py`: categorical scorer (`correct`,
  `incorrect`, `omitted`, `fabricated-anchor`, `no-target-read`) that
  resolves cited file:line anchors against the fixture.
- `scripts/bench_v18/stats.py`: exact sign test (pure-Python fallback when
  scipy is absent), Wilcoxon signed-rank, rank-biserial, bootstrap CIs.
  scipy is imported here and nowhere else.
- `scripts/bench-v18-run.sh`: thin entry point delegating to
  `python3 -m bench_v18.runner`.

## Per-arm flag set (verbatim, per merged #1603 methodology)

Common to both arms:

```text
pi -p --mode json --provider zai --model glm-5.3-flash --no-extensions --no-skills --no-context-files --session-dir <abs>/sessions/<run-id>
```

Native arm adds `--tools read,bash` and runs with
`PI_CODING_AGENT_DIR=<abs>/agent-native` containing an `mcp.json` with zero
MCP servers. MCP arm adds
`--exclude-tools edit_overwrite,edit_replace,exec_command` and runs with
`PI_CODING_AGENT_DIR=<abs>/agent-mcp` whose `mcp.json` registers exactly
one stdio server (aptu-coder). Shadow dirs are freshly created and empty
per run; all paths are absolute.

## Budget enforcement

Per-session kill at USD 0.25, stage caps of USD 0.10 / 0.10 / 0.60 / 2.00
for the four stages, and a hard cumulative USD 5 ceiling that halts the
ladder. Metering is fail-closed: absent or malformed `usage.cost` is a
kill plus a defect record, never a silent zero.

## Unit tests

`python3 -m pytest scripts/tests/` covers the cost-kill (over budget and
missing usage), the cumulative ceiling halt, the blinding leak check,
fabricated-anchor categorization, fixture determinism and the 50k LOC
floor, and oracle spot-checks against the committed fixture. All runner
tests are synthetic; no live pi session is launched by the test suite or
by CI. A minimal non-required `pytest-bench` job in `.github/workflows/ci.yml`
runs the suite on `ubuntu-24.04-arm`.
