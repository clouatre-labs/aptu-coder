# v17: OpenFAST AeroDyn Integration Audit -- MCP Full (exec_command, isolation fixed)

## Overview

v17 re-runs the MCP+exec_command conditions from v16 with the tool-isolation regression fixed.
v16 dropped `--allowedTools` from the CLI and relied on agent frontmatter `tools:` as the
enforcement mechanism. That mechanism does not work when `--dangerously-skip-permissions` is
active: the permission layer -- and the allowlist filter it enforces -- is bypassed entirely.
Every v16 run used native Bash/Grep/Read exclusively, making v16 results invalid as MCP measurements.

v17 restores the v13 enforcement pattern: `--allowedTools` is passed as an explicit CLI flag,
which is enforced before the permission layer. The `--agent` invocation style is dropped in favour
of `--model` + `--system-prompt` + `--allowedTools`, matching the proven v13 runner.

Models are updated to the latest available at time of execution:
- Condition A: `claude-sonnet-4-6` (unchanged from v13/v16)
- Condition C: `claude-haiku-4-5-20251001` (explicit version pin; v16 used the `claude-haiku-4-5`
  alias which resolves to the same model but is less reproducible across CLI versions)

All other methodology parameters (task, repo, commit, rubric, N, output schema) are identical
to v13 so the native baseline (conditions B and D) can be reused directly.

## v16 Post-mortem

**Root cause:** `--dangerously-skip-permissions` bypasses all permission checks, including the
`--allowedTools` filter. v16 moved the allowlist into agent frontmatter (`tools:`), which is
parsed by the CLI but not enforced once permissions are skipped. The model received the full
native tool set and ignored the frontmatter instruction on every run.

**Evidence:** Session JSONL analysis of all 6 v16 runs shows 14-25 native tool calls (Bash, Grep,
Read) per run and exactly 1 MCP call (`analyze_module`) per run -- almost certainly the
ToolSearch discovery call, not intentional analysis. Zero `analyze_directory`, `analyze_file`,
or `analyze_symbol` calls appear in any v16 session.

**Fix:** Restore `--allowedTools "$ALLOWED_TOOLS"` as an explicit CLI flag in the runner script,
built from the condition's tool list at dispatch time. This is the v13 pattern and is confirmed
to work. The `--agent` invocation style is retired for benchmark use.

**Canary check:** The runner's isolation validator reads the session JSONL and fails loudly if
any native tool appears. Run pilots first and confirm ISOLATION PASS before proceeding to scored
runs.

## Background

v13 established MCP advantage on Fortran scientific HPC code (OpenFAST, 344 files):
- Sonnet MCP: 472k tokens avg, $1.65 avg vs 877k tokens, $2.85 native (46% fewer tokens, 42% cheaper)
- Haiku MCP: 687k tokens avg, $0.72 avg vs 2162k tokens, $2.21 native (68% fewer tokens, 68% cheaper)

v16 intended to measure whether adding `exec_command` to the MCP tool set changes those numbers.
v16 results are invalid (see post-mortem above) and are not used as a baseline here.

v17 answers the same question as v16 with a valid experimental design.

## Repository

Identical to v13:

- **Repository:** `OpenFAST/openfast`
- **Commit:** `2895884d2be01862173c88d70f86b358d2f1a50a` (pinned for reproducibility)
- **Language:** Fortran 90/95/03
- **Size:** 344 `.f90`/`.F90` source files, ~342 MB total (including test data)

## Design

### Factorial structure

v17 runs only the two MCP conditions. Native conditions (B and D) are reused from v13 as the
baseline; the task, repo, commit, and models are identical.

| Condition | Model | Tool Set | Description |
|---|---|---|---|
| A | claude-sonnet-4-6 | MCP (full) | analyze_directory, analyze_file, analyze_symbol, analyze_module, exec_command |
| C | claude-haiku-4-5-20251001 | MCP (full) | Same tools as A |
| B (v13) | claude-sonnet-4-6 | native | Reused from v13 -- Glob, Grep, Read, Bash |
| D (v13) | claude-haiku-4-5 | native | Reused from v13 -- Glob, Grep, Read, Bash |

### Sample design

- **N = 2 scored runs per condition** (4 total scored)
- **N = 1 pilot run per condition** (2 total pilots)
- **Total new runs: 6** (conditions A and C only)

### Native baseline (reused from v13)

| Run | Input tokens | Cost | Score |
|---|---|---|---|
| B-scored-1 | 582,097 | $1.97 | 9/9 |
| B-scored-2 | 1,170,929 | $3.74 | 8/9 |
| D-scored-1 | 2,188,829 | $2.23 | 7/9 |
| D-scored-2 | 2,135,038 | $2.18 | 7/9 |

Sonnet native median: 876,513 input tokens, $2.85 cost
Haiku native median: 2,161,934 input tokens, $2.21 cost

## Task

Identical to v13. See [v17/prompts/task.md](prompts/task.md).

## Execution

All runs are executed via `scripts/bench-v17-run.sh`.

**Key differences from v16 runner:**

- `--model`, `--system-prompt`, and `--allowedTools` are all explicit CLI flags (v13 pattern)
- `--agent` is not used; agent frontmatter `tools:` is not the enforcement mechanism
- `ALLOWED_TOOLS` is built at condition-dispatch time and passed verbatim to `--allowedTools`
- Haiku model pinned to `claude-haiku-4-5-20251001` for reproducibility

**Tool isolation:**

MCP conditions (A, C) enforce:

```
--allowedTools "mcp__aptu-coder__analyze_directory,mcp__aptu-coder__analyze_file,mcp__aptu-coder__analyze_symbol,mcp__aptu-coder__analyze_module,mcp__aptu-coder__exec_command"
```

The session JSONL validator checks for native tool use after each run and exits non-zero on
ISOLATION FAIL. Pilot run isolation must be confirmed before scored runs proceed.

## Conditions

### Condition A -- Sonnet + MCP full

**Model:** `claude-sonnet-4-6`

**Allowed tools:**
- `mcp__aptu-coder__analyze_directory`
- `mcp__aptu-coder__analyze_file`
- `mcp__aptu-coder__analyze_symbol`
- `mcp__aptu-coder__analyze_module`
- `mcp__aptu-coder__exec_command`

**Forbidden:** Glob, Grep, Read, Bash, Write, and any other native Claude Code tools

### Condition C -- Haiku + MCP full

**Model:** `claude-haiku-4-5-20251001`

**Allowed tools:** identical to Condition A

**Forbidden:** identical to Condition A

### Recommended call sequence (both conditions)

1. `analyze_directory` on `<repo>/modules/aerodyn/src` (max_depth=2, summary=true) -- orient (1 call)
2. `exec_command`: `grep -n "subroutine AD_CalcOutput\|subroutine AD_UpdateStates" AeroDyn.f90` -- line numbers
3. `analyze_symbol` on `<repo>/modules/aerodyn/src`, symbol=AD_CalcOutput, follow_depth=2 -- callee tree
4. `analyze_directory` on `<repo>/modules/nwtc-library/src` (max_depth=1, summary=true) -- NWTC types
5. `analyze_module` on each NWTC file needed; escalate to `analyze_file` only if TYPE definitions absent
6. `analyze_module` on `<repo>/modules/openfast-library/src/FAST_Subs.f90` -- glue code index

`exec_command` is permitted for targeted single-result lookups only (line numbers, symbol grep).
It must not be used to explore directory trees or dump file content.

## Rubric

Identical to v13. Three dimensions, each scored 0-3 (max total = 9).

See [v13/methodology.md](../v13/methodology.md) for full rubric text and calibration anchors.

## Analysis

- Compare v17 MCP (A, C) token counts and costs against v13 MCP (A, C) and v13 native (B, D)
- Report absolute and percentage change in input tokens and cost vs v13 MCP baseline
- Report savings vs native baseline (reused from v13)
- No statistical inference (n too small); report descriptive statistics only
- Record rubric scores; flag any regression vs v13 MCP scores
- v16 results are excluded from all comparisons (invalid experimental design)

## Run Order

See [v17/run-order.txt](run-order.txt).

## File References

- Methodology: This file
- Task description: [v17/prompts/task.md](prompts/task.md) (identical to v13)
- Condition A (Sonnet + MCP full): [v17/prompts/condition-a-mcp-sonnet.md](prompts/condition-a-mcp-sonnet.md)
- Condition C (Haiku + MCP full): [v17/prompts/condition-c-mcp-haiku.md](prompts/condition-c-mcp-haiku.md)
- Runner script: [scripts/bench-v17-run.sh](../../scripts/bench-v17-run.sh)
- Scores template: [v17/scores-template.json](scores-template.json)
- OpenFAST commit: 2895884d2be01862173c88d70f86b358d2f1a50a
- v13 native baseline: [v13/scores-template.json](../v13/scores-template.json)
- v16 post-mortem: See v16 regression in this document and [v16/methodology.md](../v16/methodology.md)
