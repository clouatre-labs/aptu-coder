# v18 Benchmark Methodology (MCP vs native analysis tools)

Status: **DRAFT — scoping document. Not executed.** Supersedes the cancelled v17 re-execution (see [../v17/methodology.md](../v17/methodology.md) "Superseded", once PR #1562 merges; until then the v17 record lives on branch `bench/v17-results`).

## Why v18 exists

Two prior executions failed for harness reasons, not for tool reasons:

- v17 (discarded): the MCP server's working directory was never the target repo; no MCP run read the target, and all MCP-arm scores measured prior knowledge. See the v17 postmortem.
- v12: MCP-arm validity is unverifiable (no transcripts archived), and the headline used best-subset aggregation.

A methodology review (2026-09-15) additionally found that the v17 design could not prevent or detect contamination from model prior knowledge of the famous target repo (Django contrib.auth): a model scored 9/9 with zero target reads. v18 removes that failure mode by construction.

## Research question

When an agent performs code-analysis questions against a repository it has never seen, does access to the aptu-coder MCP tools (`analyze_directory`, `analyze_file`, `analyze_symbol`, `analyze_module`) reduce token cost (primary) without reducing answer accuracy (gate), relative to the same harness's native file-inspection tools?

## Design overview

- **Harness:** `pi` (local install; version pinned in the environment manifest). Headless mode: `pi -p --mode json`, session JSONL as the archival transcript format. Flags applied to both arms equally: `--no-context-files` (disables AGENTS.md/CLAUDE.md discovery — removes ambient-context contamination), `--no-extensions`, `--no-skills`. Harness launched with cwd = fixture repo root (the v17 cwd lesson transfers: the MCP server inherits the launcher's cwd).
- **Arms (2), paired per task:**
  - **Native:** pi built-in tools only (`read`, `bash` etc.), via `--tools` allowlist; no MCP.
  - **MCP:** aptu-coder as MCP server via `pi-mcp-adapter` (`--mcp-config`), aptu-coder tools allowlisted; built-ins denied via `--exclude-tools`. Exact tool names captured by the wiring smoke (see Ladder), not assumed.
- **Model (one):** glm-5.3-flash via Z.AI (verified list price 2026-09: USD 0.075 per 1M input, USD 0.25 per 1M output; roughly 13-20x cheaper than Claude Haiku 4.5 at USD 1/5). A single model keeps the paired design within the hard budget; cross-model generalization is explicitly out of scope (see "What this cannot tell us"). Fallback if glm-5.3-flash is unavailable at run time: glm-4.7-flash (free tier) or claude-haiku-4-5 with N reduced per the pilot arithmetic. The model ID and exact per-token rates on the run date are recorded in the manifest.
- **Unit of analysis:** the task pair. Each task runs once per arm (single model). N tasks, not N repeats of one task.
- **Target:** an unfamiliar or synthetic repository, generated once and committed as a frozen fixture with a `_source` provenance field (practice adopted from the param-description-experiments fixtures). Requirements: multi-language where aptu-coder supports it, 50k+ LOC, names/structure novel so prior knowledge cannot substitute for reading.

## Task suite and scoring

- **N = 40 small analysis questions** (fixed before the sealed run; chosen to fit the budget while preserving minimal sign-test power — see Cost estimate).
- **Ground truth from an oracle independent of aptu-coder** (e.g., ripgrep/tree-sitter tooling, or hand-verified answers committed with the fixture). The oracle must not share code with the treatment.
- **Deterministic scorer**, unit-tested (practice from `param-description-experiments/recipe/scorer.py`). Per-question categorical outcome, not prose rubrics:
  - `correct` / `incorrect` / `omitted` (no answer) / `fabricated-anchor` (cites file:line that does not resolve in the fixture) / `no-target-read` (session transcript shows zero successful reads of fixture paths — the treatment audit as a scorer category, both arms).
- **Metrics:** accuracy (primary quality), input/output/cache tokens, cost, turns, wall time — all from the session JSONL.

## Blinding (seal-and-reveal)

Run IDs are opaque randomized values carrying no arm/model/task-family information. The arm assignment is written to a sealed `label-map.json` at harness run time and not consulted until after scoring completes. `scores.json` contains no arm or model field; the analysis step is the first to join labels. (Practice adopted from both in-house experiment repos.)

## Run ladder (nothing touches the sealed run until cheaper stages pass)

1. **Wiring smoke (no or 1 live call):** verify aptu-coder MCP reachable from the harness cwd via a pre-flight `analyze_directory` on a fixture path (the v17 pre-flight gate, retained); capture exact MCP tool names pi exposes for the allowlists; verify usage/token telemetry lands in the session JSONL.
2. **Smoke:** one task pair (2 live sessions). Budget guard: if combined spend exceeds USD 0.10, abort and diagnose before proceeding.
3. **Pilot:** 10 task pairs (20 sessions), explicitly outside the sealed dataset; results inspected for harness defects only. Per-session spend guard: a session whose metered cost exceeds USD 0.25 is killed and the defect rule applies.
4. **Full sealed run:** N = 40 task pairs (80 sessions), frozen config (no runner/prompt/flag changes once begun; any defect aborts and the full run re-executes from scratch — the v17 defect rule, retained). Same per-session USD 0.25 spend guard; a full-abort re-execution must still fit the remaining budget.

## Pre-registered analysis and decision gate

- Analysis: per-task paired deltas; accuracy via exact sign test over non-ties; token/cost via per-task medians plus Wilcoxon signed-rank (outlier-robust); rank-biserial effect size; bootstrap CIs.
- **GO gate (pre-registered):** statistically significant paired token/cost reduction in the MCP arm with no accuracy regression. Anything else is NO-GO / wash and reported as such.
- Failure runs (no output, budget exhausted, `no-target-read`) are reported as a per-arm failure rate and excluded symmetrically: a task is dropped from both arms or neither.

## Provenance

Environment manifest recorded before the sealed run: pi version, pi-mcp-adapter version, aptu-coder release (with `aptu-coder --version` verified at run time against the manifest), model IDs, fixture commit hash. All raw session JSONLs, the sealed label-map (with timestamps), scores, and analysis outputs are committed under `docs/benchmarks/v18/results/`.

## Cost estimate and hard budget

**Hard ceiling: USD 5 total for the entire ladder (smoke + pilot + sealed run), target under USD 3.** The v18 design is re-scoped around this ceiling: one model (glm-5.3-flash), one arm pair, N = 40.

Arithmetic (verified list prices, Z.AI, 2026-09; re-verify on the run date and record in the manifest):

- Rates: input USD 0.075/1M, output USD 0.25/1M.
- Assumed mean session size for a small paired analysis task: 100k input (native-arm reading is input-heavy; this is deliberately conservative) + 4k output = USD 0.0085/session.
- Ladder totals:
  - Wiring smoke + smoke: 2 sessions = USD 0.02 (stage budget cap: USD 0.10).
  - Pilot: 20 sessions = USD 0.17 (stage budget cap: USD 0.60).
  - Sealed run: 80 sessions = USD 0.68 (stage budget cap: USD 2.00).
  - One full-abort re-execution of the sealed run: +USD 0.68 (held in reserve).
- Planned total: about USD 0.90; ceiling with full contingency and 3x cost overrun per session: still under USD 5.
- If the fallback model is claude-haiku-4-5 (USD 1/5 per 1M), the same conservative session profile costs about USD 0.075/session; N must then drop to 20 pairs (40 sessions ≈ USD 3.00 + pilot/smoke ≈ USD 3.60) and the power note below tightens accordingly. Decide at pilot time, before the sealed run.

Enforcement, not just estimation: the runner meters cost per session from the JSONL usage fields, kills any session above USD 0.25, and halts the ladder when a stage cap is hit. Exceeding USD 5 total is a defect that stops execution.

The expensive artifact (fixture + oracle + harness) is built once and reused at zero API cost.

## Statistical power at this budget

With N = 40 pairs (80 sessions): the exact sign test over non-tied accuracy pairs has roughly 80% power to detect a win probability of about 0.70 (two-sided, alpha 0.05); Wilcoxon signed-rank on token deltas is more sensitive and can detect moderate paired shifts. Small effects (win probability under about 0.65, or token deltas dominated by a few outlier tasks) will likely be missed or reported with wide bootstrap CIs. If defect-driven exclusions drop usable pairs below 30, the run is underpowered for anything but large effects and is reported as such rather than as a null result.

## What this cannot tell us

- Long-horizon, single-session exploration of a large monorepo (tasks are small and paired).
- The edit/exec MCP tools (`edit_overwrite`, `edit_replace`, `exec_command`) — only the four analysis tools are compared.
- Behavior in Claude Code specifically; results are pi-harness results. An optional small Claude Code confirmation arm may be added if the published claim names Claude Code.
- Performance on famous repositories, where prior knowledge substitutes for tooling — deliberately excluded.
- Cross-model or cross-provider generalization: one cheap model (glm-5.3-flash class) only. Whether the MCP/native comparison holds for frontier or mid-tier models is untested at this budget.
- Small effects: at N = 40 pairs the design is powered for large-to-moderate effects only (see Statistical power above); a null result is absence of detected difference at low power, not evidence of parity.
- Equivalence: a null accuracy result is absence of detected harm, not proof of parity.
