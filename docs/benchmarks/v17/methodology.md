# v17 Benchmark Methodology

## Provenance

v17 is a faithful re-run of the v12 Django auth migration benchmark (docs/benchmarks/v12/), corrected for six months of aptu-coder drift.

- Task: Django contrib.auth migration analysis on django/django at commit 6b90f8a8d6994dc62cd91dde911fe56ec3389494.
- Design: 2x2 -- {sonnet-4-6, haiku-4-5} x {MCP tools, native tools}.
- Conditions A/C use MCP tools (analyze_directory, analyze_file, analyze_symbol, analyze_module) via mcp-aptu-coder-only.json; conditions B/D use native tools (Bash, Glob, Grep, Read, Write, ToolSearch) with an empty MCP config and --strict-mcp-config.
- Execution order: the seed-42 order in [run-order.txt](run-order.txt) (12 total = 4 pilot + 8 scored).

Verbatim artifacts (task.md, run-order.txt, scores-template.json, mcp-aptu-coder-only.json) are byte-identical to their v12 sources.

## Corrections applied relative to v12

1. Tool prefix rename: every stale pre-rename MCP tool namespace in prompts replaced with `mcp__aptu-coder__` (ALLOWED TOOLS, FORBIDDEN TOOLS, recipe items, guidance line 19).
2. Pagination cleanup: the explicit page-size hint removed from the analyze_directory recipe call and from the pagination guidance sentence; `cursor` remains a supported parameter and page sizes are server-owned.
3. Isolation flags: claude invoked with `--settings '{"disableAllHooks":true}'` and `--setting-sources "project,local"` to prevent project/user hooks from running during benchmark runs. CLAUDE_CONFIG_DIR isolation is NOT used (it breaks keychain auth).
4. Django commit pinning: the runner checks out and verifies 6b90f8a8d6994dc62cd91dde911fe56ec3389494 before every run (inherited from the v13 runner pattern).
5. Budget cap: BENCH_MAX_BUDGET_USD is forwarded as --max-budget-usd.
6. Transcript archival: the per-run session JSONL is copied from ~/.claude/projects/<slug>/ into docs/benchmarks/v17/results/runs/ and validated for tool isolation after each run.
7. Session-dir slug: the runner now slugifies every non-alphanumeric character in the project path (including `.`), fixing transcript archival that previously always failed to locate the session directory.
8. Working directory: the CLI (and therefore the stdio aptu-coder MCP server) is launched with cwd = Django checkout. In the discarded execution the server inherited the runner-repo cwd, so its path validation rejected every target path (`path is outside the working directory`) and no MCP run ever read the target repo (see [postmortem.md](postmortem.md), H2). Applied to all conditions equally.
9. Prompt caching: `DISABLE_PROMPT_CACHING=1` (a v9 fix for a Bedrock-specific cache-write asymmetry) removed. With 5-19 turns per run, within-run cache reuse is substantial; disabling caching re-billed the full static context (~35-40k tokens/turn for MCP sessions) at full price every turn, disproportionately penalizing the MCP arm. Cost is taken from CLI `total_cost_usd`, which accounts for cached-token pricing; cache_read/cache_creation token counts are recorded in telemetry.
10. Pre-flight MCP gate: for MCP conditions, the runner verifies a minimal `analyze_directory("django/contrib/auth")` call succeeds from the exact harness working directory before starting the run, and aborts on failure. This would have caught defect 8 in both v17 and (if it had existed) v12 before any spend.

## Isolation policy

### Observed voluntary tool leakage

During the discarded first execution (see "Rejected execution"), the models voluntarily invoked tools outside their condition allowlists:

- ToolSearch in MCP conditions (a built-in that post-dates v12).
- `mcp__aptu-coder__exec_command` in an MCP condition -- a shell escape outside the v12 four-tool MCP allowlist, which the original validator missed because it checked only native tool names.
- WebFetch in a native condition.

These are voluntary model choices, not configuration failures; relying on prompts alone or on post-hoc review to prevent them is insufficient.

### Allowlist enforcement (in the runner)

The runner enforces both a positive allowlist (`--allowedTools`) and an explicit blocklist (`--disallowedTools`):

- MCP conditions (A, C): disallow ToolSearch, mcp__aptu-coder__exec_command, mcp__aptu-coder__edit_overwrite, mcp__aptu-coder__edit_replace, WebFetch, WebSearch, Task, TodoWrite.
- Native conditions (B, D): disallow WebFetch, WebSearch, Task, TodoWrite.

The transcript validator fails a run if ANY tool outside the condition allowlist appears in the session JSONL, apart from StructuredOutput (the CLI's response-formatting tool, not an agent tool).

### Post-hoc transcript census

After each run, the archived session JSONL is censused for tool usage. The census is verification of enforcement, not the enforcement mechanism itself: the `--allowedTools`/`--disallowedTools` flags define the action space; the census confirms the flags worked and records which allowlisted tools were actually used.

## Execution protocol

### Frozen configuration

Once the first scored run begins, the benchmark configuration is frozen: no runner changes, no prompt changes, no flag changes, no rubric changes. Any tool-flag change alters the model's action space and is therefore a confound, not an instrumentation fix.

### Defect handling

If a runner or validation defect is discovered mid-benchmark:

1. The entire benchmark is aborted -- all runs, pilots included, are discarded.
2. The fix lands on the runner.
3. ALL runs (pilots and scored) are re-executed from scratch under the fixed configuration.

Partial re-execution is not permitted: a dataset mixing runs executed under different flag sets is not a valid comparison.

### Run ID versioning

Re-executions never silently replace prior attempts. Run IDs are versioned with an execution-round suffix, e.g. `A-scored-1@r2` for the second execution of A-scored-1. Each version's report, telemetry, and session transcript are archived independently, so the provenance of every scored run is traceable to a single execution round in which the entire configuration was frozen. Failed runs are recorded and reported; they are never re-executed under the same run ID to obtain a passing replacement (survivorship bias).

## Scoring protocol

- Two independent readers score each run against the rubric before seeing each other's scores.
- Disagreements are resolved by discussion; the size of the delta and its resolution are recorded alongside the agreed score.
- Reader identities are recorded with the scores.
- Scoring happens only after all runs in the (frozen) execution are complete. No intermediate scoring, and no scoring while runs are still being executed or re-executed.

## Validity

### Sample size

The headline claim requires n>=3 scored runs per condition; n=5 is the target. The discarded execution and the v12 re-run both showed substantial within-condition variance, including total-access-failure runs, which n=2 cannot absorb.

### Pre-registered analysis

- Aggregation: median per condition.
- Primary metric: median total rubric score (sum of the three dimensions, 0-9) per condition, MCP vs native pooled across models; rank-biserial effect size, no p-values.
- These are stated before execution begins and are not chosen after seeing the results.

### Total-failure runs

A run in which the agent fails entirely (e.g., no usable output, budget exhausted, no tool access) is reported as a failure and counted in a per-condition failure rate. Failure runs are never silently folded into a median: a condition containing a total-failure run reports both the median over scored runs and the failure rate, so a 2/3 success condition cannot present as a higher median than a 3/3 success condition without the difference being visible.

## Rejected execution (2026-09-14/15)

A first execution of the v17 run order was discarded. During it, four runner defects were found and fixed (session-dir slug breaking transcript archival; missing --disallowedTools enforcement; a validator that checked only native tool names and missed mcp__aptu-coder__exec_command; missing WebFetch enforcement in native conditions). Because these fixes landed mid-benchmark, runs were executed under different flag sets; the resulting data is not a valid comparison and is not scored. No v17 results are retained. The full matrix (pilots and scored runs) will be re-executed from scratch under the fixed, frozen configuration in a separate execution before any scores are produced.

Post-hoc investigation of the discarded artifacts (see [postmortem.md](postmortem.md)) found a fifth, fatal defect: the MCP server's working directory was never the Django checkout, so no MCP run in any condition accessed the target repo, and all A/C scores measured prior knowledge rather than tool use. Corrections 8-10 above address it. The postmortem also documents that v12's headline aggregation used a best-subset MCP comparison and that v12's MCP arm validity is unverifiable (no transcripts archived).

## Environment manifest

- claude CLI: 2.1.236
- aptu-coder: 0.34.1 (cargo workspace version)
- Isolation flags: --settings '{"disableAllHooks":true}', --setting-sources "project,local"
- rtk: present on the machine but its hook is disabled by disableAllHooks.

## Residual risk

The global CLAUDE.md still imports @RTK.md, so the model could voluntarily invoke `rtk` even with hooks disabled. Session transcripts must be checked post-hoc for non-allowlisted tool usage before scores are accepted.
