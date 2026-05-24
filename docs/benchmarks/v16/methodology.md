# v16: OpenFAST AeroDyn Integration Audit -- Full MCP (exec_command added)

## Overview

v16 re-runs the MCP conditions from v13 (OpenFAST AeroDyn integration audit on Fortran) with one
change: `exec_command` is now available alongside the four `analyze_*` tools. This isolates the
effect of shell access on an already-MCP-capable agent. All other methodology parameters are
identical to v13 so the native baseline (conditions B and D) can be reused directly.

The target repository, task, rubric, models, N, runner, and commit pin are unchanged from v13.
Only the MCP allowed-tools list changes (two conditions, A and C).

## Background

v13 established MCP advantage on Fortran scientific HPC code (OpenFAST, 344 files):
- Sonnet 4.6 MCP: 472k tokens, $1.65 vs 877k tokens, $2.85 native (46% fewer tokens, 42% cheaper)
- Haiku 4.5 MCP: 687k tokens, $0.72 vs 2162k tokens, $2.21 native (68% fewer tokens, 68% cheaper)

Since v13, `exec_command` was added to aptu-coder. With it, the agent can perform targeted shell
queries (e.g., `grep -n "subroutine AD_CalcOutput"`, `wc -l`) without any native Claude Code tools.
v16 tests whether adding shell access to an already-MCP-capable agent further reduces token usage.

## Repository

Identical to v13:

- **Repository:** `OpenFAST/openfast`
- **Commit:** `2895884d2be01862173c88d70f86b358d2f1a50a` (pinned for reproducibility)
- **Language:** Fortran 90/95/03
- **Size:** 344 `.f90`/`.F90` source files, ~342 MB total (including test data)

## Design

### Factorial structure

v16 runs only the two MCP conditions. Native conditions (B and D) are reused from v13 as the
baseline; the task, repo, commit, and models are identical.

| Condition | Model | Tool Set | Description |
|---|---|---|---|
| A | claude-sonnet-4-6 | MCP (full) | analyze_directory, analyze_file, analyze_symbol, analyze_module, exec_command |
| C | claude-haiku-4-5 | MCP (full) | Same tools as A |
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

Identical to v13. See [v13/prompts/task.md](../v13/prompts/task.md) (symlinked as v16/prompts/task.md).

## Execution

All runs are executed via `scripts/bench-v16-run.sh`. Same runner pattern as v13 with the following
differences:

- `MCP_TOOLS` includes `mcp__aptu-coder__exec_command`
- `RUNS_DIR` and `PROMPTS_DIR` point to `v16`
- No native conditions (B/D) -- script only accepts A and C

**Tool isolation:**
- MCP conditions (A, C): `--mcp-config docs/benchmarks/v16/mcp-aptu-coder-full.json --strict-mcp-config --allowedTools "mcp__aptu-coder__analyze_directory,mcp__aptu-coder__analyze_file,mcp__aptu-coder__analyze_symbol,mcp__aptu-coder__analyze_module,mcp__aptu-coder__exec_command"`

## Conditions

### MCP conditions (A, C)

**Allowed tools:**
- `analyze_directory`
- `analyze_file`
- `analyze_symbol`
- `analyze_module`
- `exec_command`

**Forbidden:**
- Glob, Grep, Read, Bash, and any other Claude Code native tools

**Recommended call sequence:**
1. `analyze_directory(path="<repo>/modules/aerodyn/src", max_depth=2, summary=true)` -- orient (1 call)
2. `analyze_file` on `AeroDyn.f90` -- find `AD_CalcOutput` and `AD_UpdateStates`; or use `exec_command` with `grep -n "subroutine AD_" AeroDyn.f90` for a single-line result
3. `analyze_symbol(path="<repo>/modules/aerodyn/src", symbol="AD_CalcOutput", follow_depth=2)` -- trace callees
4. `analyze_directory(path="<repo>/modules/nwtc-library/src", max_depth=1, summary=true)` -- NWTC types
5. `analyze_file` on 1-2 NWTC type/utility files
6. `analyze_file` on `modules/openfast-library/src/FAST_Subs.f90` -- glue code entry point
7. `exec_command` for targeted line-number lookups or cross-checks as needed; non-zero exit
   codes are passed through to the agent as-is (exit_code field in the tool result) -- the
   agent is expected to fall back to an `analyze_*` call if a shell command fails

## Rubric

Identical to v13. Three dimensions, each scored 0-3 (max total = 9).

See [v13/methodology.md](../v13/methodology.md) for full rubric text and calibration anchors.

## Analysis

- Compare v16 MCP (A, C) token counts and costs against v13 MCP (A, C) and v13 native (B, D)
- Report absolute and percentage change in input tokens and cost vs v13 MCP
- Report savings vs native baseline (reused from v13)
- No statistical inference (n too small); report descriptive statistics only
- Record rubric scores; flag any regression vs v13 MCP scores

## Run Order

See [v16/run-order.txt](run-order.txt).

## File References

- Methodology: This file
- Task description: [v16/prompts/task.md](prompts/task.md) (identical to v13)
- Condition A (Sonnet+MCP full): [v16/prompts/condition-a-mcp-sonnet.md](prompts/condition-a-mcp-sonnet.md)
- Condition C (Haiku+MCP full): [v16/prompts/condition-c-mcp-haiku.md](prompts/condition-c-mcp-haiku.md)
- Scores template: [v16/scores-template.json](scores-template.json)
- OpenFAST commit: 2895884d2be01862173c88d70f86b358d2f1a50a
- v13 native baseline: [v13/scores-template.json](../v13/scores-template.json)
