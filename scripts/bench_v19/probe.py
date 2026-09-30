#!/usr/bin/env python3
"""A7/A7b cap-sensitivity probe driver (measurement only, no cap change).

Re-runs a fixed cell set (chain/mcp, timezone/mcp, plus their completed
native controls) across a 3x3 grid of turn cap {25, 40, 60} x wait
deadline {300s, 900s, 1800s}. Per grid point the probe saves/sets/
restores ``rv19.TIERS`` turn caps and ``setattr``s
``v18.SESSION_WAIT_TIMEOUT_S`` in try/finally, then calls
``pilot.run_one`` verbatim so the fail-closed metering and the
stage-budget ceiling are inherited with zero pilot.py edits. The
shared bench_v19 config module owns the import-time env override on
SESSION_WAIT_TIMEOUT_S; it is never relied on for per-grid-point
control.

Outputs under a dedicated probe run root: manifest.json (declaring the
full grid and probe budget up front) and cap-sensitivity.json (one
binding row per cell x cap with stopReason, killed/defect, turns and
wall-clock at termination, and cap-bound flags). The entire grid aborts
on a run_one skipped result (stage-cap / cumulative-cap); a skipped
point is never recorded as a binding datapoint. No frozen v18 constant
is edited; the AMENDMENTS.md A9 entry ratifies caps as-is (no-change
ratification).
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO / "scripts"))

from bench_v18 import runner as v18

from bench_v19 import pilot
from bench_v19 import runner_v19 as rv19

STAGE = pilot.STAGE
STAGE_CAP = pilot.STAGE_CAP
APTU_CODER_VERSION = pilot.APTU_CODER_VERSION

TURN_CAPS = [25, 40, 60]
WAIT_TIMEOUTS_S = [300, 900, 1800]

# Fixed cell set: the two A7b-deadline-bound cells plus their completed
# native controls (both native counterparts completed in the sealed run).
CELLS = [
    ("chain", "mcp"),
    ("timezone", "mcp"),
    ("chain", "native"),
    ("timezone", "native"),
]


def expand_grid(cells: list[tuple[str, str]] | None = None) -> list[dict]:
    """Deterministic cell x cap x wait expansion, every combo once."""
    cells = CELLS if cells is None else cells
    return [
        {"task": task, "arm": arm, "turn_cap": cap, "wait_timeout_s": wait}
        for task, arm in cells
        for cap in TURN_CAPS
        for wait in WAIT_TIMEOUTS_S
    ]


def run_grid(
    tasks,
    snapshot: Path,
    run_root: Path,
    state=None,
    cells: list[tuple[str, str]] | None = None,
    run_one=pilot.run_one,
    poll_s: float = 2.0,
) -> dict:
    """Run the full grid; abort everything on a skipped result.

    Each grid point saves/sets/restores rv19.TIERS turn caps and
    v18.SESSION_WAIT_TIMEOUT_S in try/finally so a mid-grid exception
    or abort leaves both byte-equal to their originals.
    """
    if not 0 < poll_s <= min(WAIT_TIMEOUTS_S):
        raise ValueError("poll_s must be in (0, min grid wait timeout]")
    state = v18.LadderState() if state is None else state
    by_id = {t["id"]: t for t in tasks}
    rows: list[dict] = []
    skipped: str | None = None
    for point in expand_grid(cells):
        task = by_id.get(point["task"])
        if task is None:
            raise KeyError(f"cell task not in task set: {point['task']}")
        saved_caps = {t: rv19.TIERS[t]["turn_cap"] for t in rv19.TIERS}
        saved_wait = v18.SESSION_WAIT_TIMEOUT_S
        try:
            for t in rv19.TIERS:
                rv19.TIERS[t]["turn_cap"] = point["turn_cap"]
            v18.SESSION_WAIT_TIMEOUT_S = point["wait_timeout_s"]
            start = time.monotonic()
            info = run_one(state, task, point["arm"], "control", snapshot, run_root)
            wall = time.monotonic() - start
        finally:
            for t, cap in saved_caps.items():
                rv19.TIERS[t]["turn_cap"] = cap
            v18.SESSION_WAIT_TIMEOUT_S = saved_wait
        if info.get("skipped"):
            # Fail-closed: stage/cumulative cap hit -> abort the whole
            # grid; a skipped point is not a binding datapoint.
            skipped = info["skipped"]
            break
        defect = info.get("defect")
        turns = rv19.count_assistant_turns(
            run_root / "sessions" / info["run_id"] / task["id"] / point["arm"]
        )
        rows.append(
            {
                "task": point["task"],
                "arm": point["arm"],
                "turn_cap": point["turn_cap"],
                "wait_timeout_s": point["wait_timeout_s"],
                "run_id": info["run_id"],
                "stopReason": info.get("stopReason"),
                "killed": info.get("killed", False),
                "defect": defect,
                "turns_at_termination": turns,
                "wall_clock_s_at_termination": (
                    float(defect.split(":")[1].rstrip("s"))
                    if defect and defect.startswith("wait-timeout-killed:")
                    else round(wall, 3)
                ),
                "turn_cap_bound": bool(
                    (defect or "").startswith("turn-cap-exceeded")
                    or (turns >= point["turn_cap"])
                ),
                "wait_deadline_bound": bool(
                    (defect or "").startswith("wait-timeout-killed")
                ),
                "cost_usd": info.get("cost_usd"),
            }
        )
    return {
        "grid": expand_grid(cells),
        "rows": rows,
        "aborted": skipped is not None,
        "abort_reason": skipped,
        "total_spent_usd": round(state.total_spent_usd, 6),
        "halted": state.halted,
        "halt_reason": state.halt_reason,
        "defects": state.defects,
    }


def write_artifacts(report: dict, run_root: Path) -> None:
    """Write manifest.json (grid + budget up front) and the report."""
    manifest = {
        "driver": "bench_v19.probe",
        "stage": STAGE,
        "aptu-coder-version": APTU_CODER_VERSION,
        "repo-head": subprocess.run(
            ["git", "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            cwd=str(REPO),
            check=False,
        ).stdout.strip(),
        "provider": v18.PROVIDER,
        "model": v18.MODEL,
        "amendments": ["A7", "A7b", "A9"],
        "turn_caps": TURN_CAPS,
        "wait_timeouts_s": WAIT_TIMEOUTS_S,
        "cells": [list(c) for c in CELLS],
        "budget": {
            "stage_cap_usd": STAGE_CAP,
            "per_session_kill_usd": v18.PER_SESSION_KILL_USD,
            "ceiling_usd": v18.TOTAL_CEILING_USD,
            "declared_up_front": True,
        },
    }
    (run_root / "manifest.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )
    (run_root / "cap-sensitivity.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--tasks", type=Path, action="append", required=True)
    ap.add_argument("--snapshot", type=Path, required=True)
    ap.add_argument("--run-root", type=Path, default=Path("/tmp/v19-probe/run1"))
    args = ap.parse_args()
    snapshot = args.snapshot.resolve()
    run_root = args.run_root.resolve()
    sealed = REPO / "docs" / "benchmarks" / "v19" / "results"
    if sealed == run_root or sealed in run_root.parents:
        ap.error("run root must not overlap the sealed results directory")
    run_root.mkdir(parents=True, exist_ok=True)
    import os

    os.chdir(snapshot)
    tasks = pilot.load_tasks(args.tasks)
    state = v18.LadderState()
    report = run_grid(tasks, snapshot, run_root, state)
    write_artifacts(report, run_root)
    print(
        json.dumps(
            {
                "aborted": report["aborted"],
                "abort_reason": report["abort_reason"],
                "rows": len(report["rows"]),
                "total_spent_usd": report["total_spent_usd"],
            }
        )
    )


if __name__ == "__main__":
    main()
