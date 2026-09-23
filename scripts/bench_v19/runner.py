#!/usr/bin/env python3
"""Thin runner adapter for the v19 benchmark.

Imports bench_v18 runner/blinding/stats via sys.path (never copy-paste)
and exposes a shadowed v19 arms registry. The mcp-gateway arm
(directTools: false, identical to the PR #1621 mcp-gateway spec) is the
tax-control arm restricted to hop-1 Track C cells. bench_v18 module-level
constants are never mutated; mutation observable by v18 tests would be a
defect.
"""

from __future__ import annotations

from bench_v18 import blinding as v18_blinding  # noqa: F401
from bench_v18 import runner as v18_runner
from bench_v18 import stats as v18_stats  # noqa: F401

from bench_v19 import HOP_DEPTHS, V19_ARMS  # noqa: F401

TRACKS = ("A", "C")


def build_invocation(
    arm: str, run_root, session_dir, prompt: str
) -> tuple[list[str], dict[str, str]]:
    """Delegate to the v18 invocation builder with v19 arm validation."""
    if arm not in V19_ARMS:
        raise ValueError(f"unknown arm: {arm}")
    return v18_runner.build_invocation(arm, run_root, session_dir, prompt)


def arm_allowed_for_cell(arm: str, track: str, hop_depth: int | None) -> bool:
    """Cell-placement rule for the v19 ladder.

    The mcp-gateway tax-control arm runs only on hop-1 Track C cells
    (exactly as PR #1621 defined); native and mcp run on every cell.
    """
    if arm not in V19_ARMS:
        raise ValueError(f"unknown arm: {arm}")
    if arm == "mcp-gateway":
        return track == "C" and hop_depth == 1
    return True


def ladder_cells(tasks: list[dict]) -> list[dict]:
    """All (task, arm) cells for the v19 ladder, deterministic order."""
    cells = []
    for task in sorted(tasks, key=lambda t: t["id"]):
        for arm in V19_ARMS:
            if arm_allowed_for_cell(arm, task["track"], task.get("hop_depth")):
                cells.append({"task_id": task["id"], "arm": arm})
    return cells
