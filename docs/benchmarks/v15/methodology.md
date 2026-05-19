# v15: remote_file / remote_tree MCP Tools vs curl Benchmark

## Overview

This benchmark compares the performance and usability of aptu-coder's MCP remote tools (`remote_file`, `remote_tree`) against curl CLI for fetching data from GitLab without cloning.

**Design:**
- Two conditions: E (MCP remote tools), F (curl CLI control)
- Five targets: T1 (full file), T1b (sliced file), T2 (small tree), T3 (medium tree), T4 (depth=2 stress)
- 5 scored runs per condition per target, interleaved
- 1 pilot run per condition per target group to validate methodology before scoring
- Error sub-tasks (C5 rubric): missing token and 404 not found, one per condition

**References:**
- Issue #934: benchmark remote_file/remote_tree vs curl
- Issue #856: T4 depth=2 stress test on gitlab-org/gitlab

## Background

The MCP protocol allows Claude and other agents to invoke tools via a standardized interface. aptu-coder exposes `remote_file` and `remote_tree` as MCP tools for fetching GitLab data without cloning. This benchmark measures whether MCP tools provide a usability or performance advantage over direct curl CLI invocations.

Key asymmetry: GitLab's Files API returns a JSON envelope with base64-encoded content. The MCP tool decodes server-side before returning plain text to the agent. curl returns the JSON envelope; the agent must decode it. This is measured by rubric criterion C3 (decode_required).

## Conditions

| Condition | Tool | Auth | Notes |
|-----------|------|------|-------|
| E (MCP) | `mcp__aptu-coder__remote_file`, `mcp__aptu-coder__remote_tree` | `GITLAB_TOKEN` env var | Tools use token automatically; agent sees plain text |
| F (curl) | `curl` CLI via Bash | `PRIVATE-TOKEN` header | Agent must construct curl commands; must base64-decode JSON envelope |

Both conditions use the same GitLab Files API endpoint (`/api/v4/projects/{encoded}/repository/files/{encoded}` for files; `/api/v4/projects/{encoded}/repository/tree` for trees).

## Test Targets

| ID | Operation | Repo | Path | Depth | Notes |
|----|-----------|------|------|-------|-------|
| T1 | File fetch (full) | gnome/gtk | gtk/gtkwidget.c | N/A | ~350 KB C source; tests baseline file fetch |
| T1b | File fetch (sliced) | gnome/gtk | gtk/gtkwidget.c (lines 1-50) | N/A | Same network cost as T1; agent sees 50 lines vs ~350 KB; tests token efficiency |
| T2 | Tree listing (small) | gnome/gtk | / (root) | 1 | ~14 entries; tests baseline tree fetch |
| T3 | Tree listing (medium) | gnome/gtk | gtk/ | 1 | ~101 entries; tests larger tree handling |
| T4 | Tree listing (depth=2 stress) | gitlab-org/gitlab | / (root) | 2 | 132 root entries + 45 subdirs; reproduces #856; expected to exceed 2000ms median |

## Rubric

Five criteria (C1-C5) measure correctness, latency, and usability:

| Criterion | Source | Field | Description | Win Condition |
|-----------|--------|-------|-------------|---------------|
| C1 | Agent | `content_correct` | Content begins with expected first line | >=4/5 runs true |
| C2 | Harness | `latency_ms` | Median latency across 5 runs | <=2000 ms |
| C3 | Agent | `decode_required` | false = no base64 decode required | >=4/5 runs false (MCP wins) |
| C4 | Architectural note | N/A | MCP requires GITLAB_TOKEN unconditionally; not scored per-run | Informational |
| C5 | Agent | `error_graceful` | Clear actionable error on missing token AND on 404 | >=4/5 runs true |

**Scoring:** A target passes if >=4/5 criteria are met (C2 uses median; C1/C3/C5 use run counts). C4 is architectural context only.

## Run Protocol

### Pilot Runs
Before scoring, run one pilot per condition per target group to validate methodology:
1. E-T1-pilot (MCP on T1)
2. F-T1-pilot (curl on T1)
3. E-T1b-pilot, F-T1b-pilot
4. E-T2-pilot, F-T2-pilot
5. E-T3-pilot, F-T3-pilot
6. E-T4-pilot, F-T4-pilot

Review pilot outputs for correctness before proceeding to scored runs.

### Scored Runs
After pilots pass, run 5 scored runs per condition per target, interleaved (E, F, F, E, E, F, F, E, E, F) to minimize network variance correlation.

Total scored runs: 5 targets × 2 conditions × 5 runs = 50 runs.

### Error Sub-Tasks
After all targets, run error sub-tasks (not counted in n=5):
- E-error-missing-token: MCP with GITLAB_TOKEN unset
- E-error-not-found: MCP with nonexistent path
- F-error-missing-token: curl with GITLAB_TOKEN unset
- F-error-not-found: curl with nonexistent path

## External Timing Harness

The harness measures wall-clock latency using bash `$EPOCHREALTIME` (bash 5+) or `gdate +%s%3N` (macOS with coreutils). Falls back to noting "timing unavailable" if neither is present.

For condition F (curl), the harness runs a pre-timed curl call to the GitLab API before invoking the agent, recording the baseline curl latency. The agent's curl invocations occur inside the LLM call and are not separately timed.

For condition E (MCP), no pre-timing is available; latency is measured as the goose wall-clock time (includes LLM overhead).

## Output Schema

Per-run agent output (JSON):

```json
{
  "run_id": "string",
  "condition": "E or F",
  "target_id": "T1, T1b, T2, T3, T4, or error",
  "content_correct": "boolean (C1)",
  "decode_required": "boolean (C3)",
  "first_line_seen": "string (truncated to 80 chars)",
  "content_chars": "integer (character count)",
  "entry_count": "integer (for tree targets)",
  "first_entry_seen": "string (for tree targets)",
  "tool_calls_total": "integer",
  "error_graceful": "boolean (C5, error sub-tasks only)",
  "error_message": "string (C5, error sub-tasks only, truncated to 200 chars)"
}
```

Per-run harness output (JSON):

```json
{
  "run_id": "string",
  "condition": "E or F",
  "target_id": "T1, T1b, T2, T3, T4, or error",
  "latency_ms": "integer (wall-clock harness measurement)",
  "raw_bytes": "integer (bytes returned by API)",
  "content_chars": "integer (character count as seen by agent)",
  "est_tokens": "integer (estimated as content_chars / 4)",
  "timestamp": "ISO 8601"
}
```

## Discard Logic

Any run exceeding 30 seconds wall-clock time is discarded and must be re-run. This is expected for T4 (depth=2 stress) and should be noted in results.

## Acceptance Criteria

A target passes the benchmark if:
1. C1 (content_correct): >=4/5 runs report true
2. C2 (latency_ok): median latency across 5 runs <=2000 ms
3. C3 (decode_not_required): >=4/5 runs report false (MCP wins; curl loses)
4. C5 (error_graceful): >=4/5 runs report true on both missing_token and not_found

**Verdict:** MCP wins if E passes >=3/4 criteria and F passes <=2/4 criteria (or vice versa for curl win).

## Notes

- Token estimation: `est_tokens = content_chars / 4` is an approximation; actual token count depends on tokenizer.
- Rate limiting: GITLAB_TOKEN may hit rate limits after 10+ runs per target; monitor for 429 responses.
- Network variance: Median aggregation across 5 runs mitigates transient network delays.
