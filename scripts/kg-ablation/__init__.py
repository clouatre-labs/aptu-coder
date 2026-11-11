"""KG pull-ablation benchmark harness (issue #1809).

Standalone stdlib-only package implementing the pre-registered design in
docs/audit/2026-08-29-kg-pull-ablation-benchmark-design.md. Does not import
any frozen bench_v18/bench_v19 module.
"""

from .config import (
    ARMS,
    FIXTURES,
    RUNS_PER_CELL,
    SPEND_CAP_USD,
    PROVIDER,
    MODEL,
    GRAPH_URI_PREFIX,
    HIDDEN_TOOL,
    session_matrix,
)

__all__ = [
    "ARMS",
    "FIXTURES",
    "RUNS_PER_CELL",
    "SPEND_CAP_USD",
    "PROVIDER",
    "MODEL",
    "GRAPH_URI_PREFIX",
    "HIDDEN_TOOL",
    "session_matrix",
]
