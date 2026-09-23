"""v19 crossover benchmark scaffold (pilot-first).

Shadowed registry and constants for the v19 benchmark. This package never
reassigns bench_v18 module-level globals; all v18 reuse goes through
imports (see runner.py).
"""

__all__ = [
    "HOP_DEPTHS",
    "TRACKS",
    "V19_ARMS",
    "generate_tasks",
    "oracle",
    "runner",
    "score",
    "snapshots",
]

# Arms for v19. The mcp-gateway arm (directTools: false) is the tax-control
# arm per PR #1621; it is restricted to hop-1 Track C cells (see runner.py).
V19_ARMS = ("native", "mcp", "mcp-gateway")

# Hop-depth sweep applies to the Track A callers template only.
HOP_DEPTHS = (1, 2, 3)

TRACKS = ("A", "C")
