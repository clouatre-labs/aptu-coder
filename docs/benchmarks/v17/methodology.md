# v17 Benchmark Methodology

## Provenance

v17 is a faithful re-run of the v12 Django auth migration benchmark (docs/benchmarks/v12/), corrected for six months of aptu-coder drift.

- Task: Django contrib.auth migration analysis on django/django at commit 6b90f8a8d6994dc62cd91dde911fe56ec3389494.
- Design: 2x2 -- {sonnet-4-6, haiku-4-5} x {MCP tools, native tools}.
- Runs: 12 total = 4 pilot + 8 scored, executed in the seed-42 order in run-order.txt.
- Conditions A/C use MCP tools (analyze_directory, analyze_file, analyze_symbol, analyze_module) via mcp-aptu-coder-only.json; conditions B/D use native tools (Bash, Glob, Grep, Read, Write, ToolSearch) with an empty MCP config and --strict-mcp-config.

## Corrections applied relative to v12

1. Tool prefix rename: every stale pre-rename MCP tool namespace in prompts replaced with `mcp__aptu-coder__` (ALLOWED TOOLS, FORBIDDEN TOOLS, recipe items, guidance line 19).
2. Pagination cleanup: the explicit page-size hint removed from the analyze_directory recipe call and from the pagination guidance sentence; `cursor` remains a supported parameter and page sizes are server-owned.
3. Isolation flags: claude invoked with `--settings '{"disableAllHooks":true}'` and `--setting-sources "project,local"` to prevent project/user hooks from running during benchmark runs. CLAUDE_CONFIG_DIR isolation is NOT used (it breaks keychain auth).
4. Django commit pinning: the runner checks out and verifies 6b90f8a8d6994dc62cd91dde911fe56ec3389494 before every run (inherited from the v13 runner pattern).
5. Budget cap: BENCH_MAX_BUDGET_USD is forwarded as --max-budget-usd.
6. Transcript archival: the per-run session JSONL is copied from ~/.claude/projects/<slug>/ into docs/benchmarks/v17/results/runs/ and validated for tool isolation after each run.

Verbatim artifacts (task.md, run-order.txt, scores-template.json, mcp-aptu-coder-only.json) are byte-identical to their v12 sources.

## Environment manifest

- claude CLI: 2.1.236
- aptu-coder: 0.34.1 (cargo workspace version)
- Isolation flags: --settings '{"disableAllHooks":true}', --setting-sources "project,local"
- rtk: present on the machine but its hook is disabled by disableAllHooks.

## Residual risk

The global CLAUDE.md still imports @RTK.md, so the model could voluntarily invoke `rtk` even with hooks disabled. Session transcripts must be checked post-hoc for non-allowlisted tool usage before scores are accepted.
