#!/usr/bin/env python3
"""Task generator for the v19 benchmark.

Track A: callers-template prompts stratified by hop depth 1/2/3 (the hop
sweep is restricted to the callers template). Track C: lookup control
prompts. Includes the pilot-first discriminative-power filter
(methodology "Task selection rule"): Track A tasks are kept when the F1
gap between arms is >= 0.2, except tasks whose oracle has exactly one
ground-truth entity, which are explicitly excluded from the F1-gap rule
and kept by categorical divergence instead.
"""

from __future__ import annotations

F1_GAP_THRESHOLD = 0.2


def generate_tasks(oracle_entries: list[dict]) -> list[dict]:
    """Deterministic task list from oracle entries.

    Track A callers entries yield one task per hop depth; Track C lookup
    entries yield one task each. Output is sorted by task id.
    """
    tasks: list[dict] = []
    for entry in oracle_entries:
        if entry.get("track") == "A":
            tasks.append(
                {
                    "id": f"task-{entry['id']}",
                    "track": "A",
                    "prompt": (
                        f"List every file in this repository that calls "
                        f"{entry['symbol']} within {entry['hop_depth']} "
                        f"hop(s) of indirection. Report file paths only."
                    ),
                    "hop_depth": entry["hop_depth"],
                    "expected_files": entry["expected_files"],
                }
            )
        elif entry.get("track") == "C":
            tasks.append(
                {
                    "id": f"task-{entry['id']}",
                    "track": "C",
                    "prompt": (
                        f"Locate the definition of {entry['symbol']} and "
                        f"report its file path."
                    ),
                    "hop_depth": None,
                    "expected_files": entry["expected_files"],
                }
            )
    return sorted(tasks, key=lambda t: t["id"])


def apply_discriminative_filter(
    task: dict,
    f1_native: float,
    f1_mcp: float,
    divergent: bool,
) -> bool:
    """Keep/drop decision for one piloted task.

    Track A with more than one expected entity: keep when
    |f1_native - f1_mcp| >= 0.2. Tasks with exactly one expected entity
    are excluded from the F1-gap rule and decided purely by
    categorical divergence (``divergent``).
    """
    if task["track"] == "A" and len(task["expected_files"]) > 1:
        return abs(f1_native - f1_mcp) >= F1_GAP_THRESHOLD
    return divergent


def filter_pilot_tasks(tasks: list[dict], pilot_results: list[dict]) -> list[dict]:
    """Apply the filter to piloted tasks.

    pilot_results items: {task_id, f1_native, f1_mcp, divergent}.
    Returns survivors in deterministic (id) order.
    """
    by_id = {r["task_id"]: r for r in pilot_results}
    survivors = [
        task
        for task in tasks
        if (result := by_id.get(task["id"])) is not None
        and apply_discriminative_filter(
            task,
            result["f1_native"],
            result["f1_mcp"],
            result["divergent"],
        )
    ]
    return sorted(survivors, key=lambda t: t["id"])
