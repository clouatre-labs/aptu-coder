# Scripts

Benchmarking and testing utilities for aptu-coder.

## Benchmarking

Each `bench-*-run.sh` script runs one condition of an A/B evaluation wave (MCP vs native tool-calling, model comparisons) and writes results under its wave's results directory. Usage: `./bench-<wave>-run.sh <CONDITION_ID> <RUN_ID>` unless noted otherwise.

| Script | Conditions |
|--------|------------|
| `bench-v12-run.sh` | A-D |
| `bench-v13-run.sh` | A-D |
| `bench-v14-run.sh` | A-D |
| `bench-v15-run.sh` | E (MCP) or F (curl); usage: `<CONDITION_ID> <TARGET_ID> <RUN_ID> [--error-type <type>]` |
| `bench-v16-run.sh` | A or C (B/D reused from v13) |
| `bench-wave9-run.sh` | A-D |
| `bench-wave10-run.sh` | E or F |

Each script documents its own condition semantics and prompt substitution in a header comment; read that before running an unfamiliar wave.

## Observability & analytics

- `mcp-metrics.py` -- reads daily-rotated JSONL from `$XDG_DATA_HOME/aptu-coder/`, reports tool-call latency (p50/p95/p99), error rates, cache hit rates, and pagination/feature adoption. `python scripts/mcp-metrics.py --help` for options.
- `session-stats.py` -- scans `.worktrees/*/.handoff/` for plan/build/validation JSON and reports verdict distribution, retry rate, and PR success rate. `python scripts/session-stats.py --help` for options.

## Testing

- `cross-client-compat.py` -- validates that the MCP server behaves consistently across different client implementations.
- `tests/` -- pytest suite for `mcp-metrics.py` (`test_mcp_metrics.py`).

## One-off

- `rename-finalize.sh` -- idempotent finalization steps for the historical `code-analyze-mcp` -> `aptu-coder` project rename. Retained for reference; not part of routine workflows.
