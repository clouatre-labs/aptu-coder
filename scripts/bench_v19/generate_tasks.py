#!/usr/bin/env python3
"""Task generator for the v19 benchmark.

Track A: callers-template prompts stratified by hop depth 1/2/3 (the hop
sweep is restricted to the callers template). Track C: lookup control
prompts. Track C lookup tasks are generated directly from the
tree-sitter definition index and only when the symbol has exactly one
defining file (single-answer invariant, hop-1 lookup so the runner's
mcp-gateway tax-control gate admits these cells). Track C is exempt
from the F1-gap rule and from the rg agreement cross-check, which is
only defined for callers sets. Ambiguous symbols (defined in more than
one file) are excluded from both tracks fail-closed, with the
exclusion reason recorded (see ``collect_exclusions``). Includes the
discriminative-power filter (methodology "Task selection rule"):
Track A tasks are kept when the F1 gap between arms is >= 0.2, except
tasks whose oracle has exactly one ground-truth entity, which are
explicitly excluded from the F1-gap rule and kept by categorical
divergence instead.
"""

from __future__ import annotations

F1_GAP_THRESHOLD = 0.2


def generate_tasks(oracle_entries: list[dict]) -> list[dict]:
    """Deterministic task list from oracle entries.

    Track A callers entries yield one task per hop depth; Track A tasks
    whose rg-vs-tree-sitter cross-check disagreed are dropped (the
    methodology's double-extraction agreement filter), as are entries
    carrying an ``excluded_reason`` (ambiguous symbol, fail-closed).
    Track C lookup entries yield one task each. Output is sorted by
    task id.
    """
    tasks: list[dict] = []
    for entry in oracle_entries:
        if entry.get("excluded_reason") is not None:
            continue  # ambiguous symbol -> fail-closed exclusion
        if entry.get("track") == "A":
            if not entry.get("crosscheck_agrees", True):
                continue  # double-extraction disagreement -> drop
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
                    "hop_depth": entry["hop_depth"],
                    "expected_files": entry["expected_files"],
                }
            )
    return sorted(tasks, key=lambda t: t["id"])


def collect_exclusions(oracle_entries: list[dict]) -> list[dict]:
    """Recorded exclusion reasons for dropped oracle entries.

    Fail-closed record of every entry excluded from task generation
    because its symbol is ambiguous (defined in more than one file).
    """
    return [
        {
            "id": entry["id"],
            "symbol": entry["symbol"],
            "reason": entry["excluded_reason"],
        }
        for entry in oracle_entries
        if entry.get("excluded_reason") is not None
    ]


def generate_track_c_tasks(definition_index: dict[str, list[str]]) -> list[dict]:
    """Track C lookup tasks straight from the tree-sitter definition index.

    Only symbols with exactly one defining file emit a task: a lookup
    prompt must have a single answer, so multi-file (ambiguous) symbols
    are skipped fail-closed. Each task is a hop-1 lookup (Track C stays
    hop-1), which is what admits it to the mcp-gateway tax-control arm
    in the runner gate.
    """
    tasks: list[dict] = []
    for symbol in sorted(definition_index):
        files = sorted(definition_index[symbol])
        if len(files) != 1:
            continue  # single-answer invariant: skip ambiguous/empty symbols
        tasks.append(
            {
                "id": f"task-c-lookup-{symbol}",
                "track": "C",
                "prompt": (
                    f"Locate the definition of {symbol} and report its file path."
                ),
                "hop_depth": 1,
                "expected_files": files,
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
    are exempt from the F1-gap rule. Track C is exempt too: lookup
    tasks have a single answer by construction, so they are decided
    purely by categorical divergence (``divergent``).
    """
    if task["track"] == "C":
        return divergent  # single answer by construction; F1-gap exempt
    if len(task["expected_files"]) > 1:
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
