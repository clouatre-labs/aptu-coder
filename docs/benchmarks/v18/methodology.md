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
- **Models:** claude-sonnet and claude-haiku (or the closest available at execution time; recorded in the manifest).
- **Unit of analysis:** the task pair. Each task runs once per arm per model. N tasks, not N repeats of one task.
- **Target:** an unfamiliar or synthetic repository, generated once and committed as a frozen fixture with a `_source` provenance field (practice adopted from the param-description-experiments fixtures). Requirements: multi-language where aptu-coder supports it, 50k+ LOC, names/structure novel so prior knowledge cannot substitute for reading.

## Task suite and scoring

- **N = 40-60 small analysis questions** with machine-verifiable answers (e.g., "list all callers of symbol X", "which files import module Y", "what does function F return"). N is fixed before the sealed run.
- **Ground truth from an oracle independent of aptu-coder** (e.g., ripgrep/tree-sitter tooling, or hand-verified answers committed with the fixture). The oracle must not share code with the treatment.
- **Deterministic scorer**, unit-tested (practice from `param-description-experiments/recipe/scorer.py`). Per-question categorical outcome, not prose rubrics:
  - `correct` / `incorrect` / `omitted` (no answer) / `fabricated-anchor` (cites file:line that does not resolve in the fixture) / `no-target-read` (session transcript shows zero successful reads of fixture paths — the treatment audit as a scorer category, both arms).
- **Metrics:** accuracy (primary quality), input/output/cache tokens, cost, turns, wall time — all from the session JSONL.

## Blinding (seal-and-reveal)

Run IDs are opaque randomized values carrying no arm/model/task-family information. The arm assignment is written to a sealed `label-map.json` at harness run time and not consulted until after scoring completes. `scores.json` contains no arm or model field; the analysis step is the first to join labels. (Practice adopted from both in-house experiment repos.)

## Run ladder (nothing touches the sealed run until cheaper stages pass)

1. **Wiring smoke (no or 1 live call):** verify aptu-coder MCP reachable from the harness cwd via a pre-flight `analyze_directory` on a fixture path (the v17 pre-flight gate, retained); capture exact MCP tool names pi exposes for the allowlists; verify usage/token telemetry lands in the session JSONL.
2. **Smoke:** one task pair, live.
3. **Pilot:** 10 task pairs, explicitly outside the sealed dataset; results inspected for harness defects only.
4. **Full sealed run:** N task pairs, frozen config (no runner/prompt/flag changes once begun; any defect aborts and the full run re-executes from scratch — the v17 defect rule, retained).

## Pre-registered analysis and decision gate

- Analysis: per-task paired deltas; accuracy via exact sign test over non-ties; token/cost via per-task medians plus Wilcoxon signed-rank (outlier-robust); rank-biserial effect size; bootstrap CIs.
- **GO gate (pre-registered):** statistically significant paired token/cost reduction in the MCP arm with no accuracy regression. Anything else is NO-GO / wash and reported as such.
- Failure runs (no output, budget exhausted, `no-target-read`) are reported as a per-arm failure rate and excluded symmetrically: a task is dropped from both arms or neither.

## Provenance

Environment manifest recorded before the sealed run: pi version, pi-mcp-adapter version, aptu-coder release (with `aptu-coder --version` verified at run time against the manifest), model IDs, fixture commit hash. All raw session JSONLs, the sealed label-map (with timestamps), scores, and analysis outputs are committed under `docs/benchmarks/v18/results/`.

## Cost estimate

Roughly USD 50-150 of API spend for the full ladder at two models (small analysis tasks; pi sessions are short). The expensive artifact (fixture + oracle + harness) is built once and reused; re-executions after a defect cost minutes.

## What this cannot tell us

- Long-horizon, single-session exploration of a large monorepo (tasks are small and paired).
- The edit/exec MCP tools (`edit_overwrite`, `edit_replace`, `exec_command`) — only the four analysis tools are compared.
- Behavior in Claude Code specifically; results are pi-harness results. An optional small Claude Code confirmation arm may be added if the published claim names Claude Code.
- Performance on famous repositories, where prior knowledge substitutes for tooling — deliberately excluded.
- Equivalence: a null accuracy result is absence of detected harm, not proof of parity.
