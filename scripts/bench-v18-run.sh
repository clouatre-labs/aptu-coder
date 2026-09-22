#!/usr/bin/env bash
# Thin entry point for the v18 benchmark ladder. Delegates to bench_v18.runner.
# Live ladder execution is maintainer-only post-merge; see
# docs/benchmarks/v18/harness.md.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PYTHONPATH="$SCRIPT_DIR${PYTHONPATH:+:$PYTHONPATH}"
exec python3 -m bench_v18.runner "$@"
