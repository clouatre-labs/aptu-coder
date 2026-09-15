# v17 Benchmark Analysis

## Summary

v17 is a faithful re-run of the v12 Django auth migration benchmark (same task, same prompts modulo drift corrections, same seed-42 run order, same pinned Django commit `6b90f8a8d6994dc62cd91dde911fe56ec3389494`), executed on 2026-09-14/15 with claude CLI 2.1.236 and aptu-coder 0.34.1. All 12 runs (4 pilot + 8 scored) completed with valid JSON output, archived session transcripts, and clean tool-isolation verification.

Headline question: did the MCP-vs-native gap from v12 hold, narrow, or flip after six months of drift on both sides?

**Answer: it narrowed at the Sonnet tier and flipped at the Haiku tier.**

## Scores

Median total scores per condition (v12 -> v17), scored runs only:

| Condition | Model | Tool set | n | v12 median | v17 median | v17 runs |
|-----------|-------|----------|---|-----------|-----------|----------|
| A | Sonnet | MCP | 2 | 9.0 | 9.0 | 9, 9 |
| B | Sonnet | Native | 2 | 8.5 | 9.0 | 9, 9 |
| C | Haiku | MCP | 2 | 8.0 | 3.5 | 1, 6 |
| D | Haiku | Native | 2 | 9.0 | 9.0 | 9, 9 |

Median by dimension (v17):

| Condition | Structural | Cross-module | Approach |
|-----------|-----------|--------------|----------|
| A | 3.0 | 3.0 | 3.0 |
| B | 3.0 | 3.0 | 3.0 |
| C | 1.0 | 1.0 | 1.5 |
| D | 3.0 | 3.0 | 3.0 |

## The MCP-vs-native gap

- **Sonnet (A vs B):** v12 had MCP ahead 9.0 vs 8.5; v17 has both at 9.0. The quality gap closed. The efficiency gap persists: MCP Sonnet still completes the task with a median of 3.5 tool calls vs 11 for native (a ~3x reduction), at similar cost (median USD 0.74 vs 0.72) and lower wall time (median 111s vs 130s). MCP remains the more token- and call-efficient interface, but no longer buys a quality edge at the Sonnet tier.
- **Haiku (C vs D):** the gap flipped. Native haiku stayed perfect (9, 9 -- identical to v12), while MCP haiku regressed hard (8.0 -> 3.5 median). C-scored-2 (score 6) missed base_user.py entirely; C-scored-1 (score 1) failed to reach the repository at all -- it concluded the Django checkout was outside the MCP working-directory sandbox, emitted an empty module map, and substituted its own run_id. This is model-path-choice variance, not contamination: C-scored-2 and C-pilot-1 reached the repo with an identical runner config, and the session transcripts show only allowlisted `mcp__aptu-coder__analyze_*` calls. The smaller model is evidently more brittle against the aptu-coder path sandbox when the target repo lives outside the agent's cwd.

## Efficiency (v17, scored runs)

| Run | Tool calls | Input tok | Cost USD | Wall s | Score/USD |
|-----|-----------|-----------|----------|--------|-----------|
| A-scored-1 | 3 | 163,683 | 0.566 | 86 | 15.9 |
| A-scored-2 | 4 | 268,283 | 0.921 | 138 | 9.8 |
| B-scored-1 | 11 | 208,237 | 0.705 | 151 | 12.8 |
| B-scored-2 | 11 | 211,742 | 0.733 | 109 | 12.3 |
| C-scored-1 | 5 | 347,858 | 0.367 | 37 | 2.7 |
| C-scored-2 | 5 | 290,287 | 0.306 | 28 | 19.6 |
| D-scored-1 | 5 | 502,792 | 0.534 | 76 | 16.9 |
| D-scored-2 | 7 | 388,505 | 0.413 | 93 | 21.8 |

Prompt caching was disabled (v12 parity), so input tokens dominate cost in all conditions.

## Isolation verification

Every archived session transcript passed a full tool census: MCP conditions used only `mcp__aptu-coder__analyze_*` tools; native conditions used only Bash/Glob/Grep/Read/Write/ToolSearch. All `rtk` string matches in transcripts are coincidental substrings inside base64 blobs; no `rtk` tool was invoked in any run. Django HEAD was verified at the pinned commit before and after all runs.

## Deviations from plan (logged per instructions)

1. **Runner fixes (4 commits on this branch, made between runs, never mid-run):**
   - Session-dir slug left `.` unslugified, so transcript archival always failed ("Session directory not found"). Fixed by slugifying all non-alphanumerics.
   - Models voluntarily invoked `ToolSearch` (MCP conditions), `mcp__aptu-coder__exec_command` (C-pilot-1), and `WebFetch` (D-scored-1) -- all outside the v12 allowlists. Fixed by adding `--disallowedTools` for the respective conditions and by tightening the validator to fail on ANY non-allowlisted tool rather than only cross-family tools.
2. **Re-runs due to the above:** B-scored-1 (archival bug), A-scored-1 (x2, ToolSearch), C-pilot-1 (exec_command), D-scored-1 (WebFetch). All contaminated/incomplete artifacts were deleted before the re-run. D-pilot-1 failed once with an empty-stderr transient claude exit and succeeded on manual retry.
3. **Budget cap:** `BENCH_MAX_BUDGET_USD=5.00` per run. A pre-flight probe revealed the CLI rejects caps below its own minimum, so a ping at 0.05 errored; the cap itself was never hit (max single-run cost: USD 0.92).
4. **Seed-42 run order** was followed exactly; only re-runs replaced failed executions of the same run ID in place.

## Threats to validity

- `--disallowedTools` hardening post-dates v12; it removes voluntary tool leakage as a failure mode, which slightly favors runs executed after the fix (all MCP runs after commit 2, all native runs after commit 4). Given every affected run was re-executed under the final flag set, all 12 archived runs reflect the hardened configuration except B-scored-1/B-scored-2/C-scored-1/C-scored-2 which predate the WebFetch fix (their transcripts show no such usage, so no material difference).
- n=2 per condition for scored runs; the C median is driven by one total-failure run. The C regression direction is nonetheless consistent with C-scored-2's structural misses.
