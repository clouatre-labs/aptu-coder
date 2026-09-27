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

from pathlib import Path

from bench_v19 import oracle

F1_GAP_THRESHOLD = 0.2

# Fan-in tier (A1/F2) selection thresholds.
FANIN_MIN_HOP1_CALLER_FILES = 40
FANIN_MIN_HOP3_CALLER_FILES = 60
FANIN_MAX_SYMBOLS = 10
FANIN_HOP_DEPTH = 3


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
                        f"hop(s) of indirection. Report file paths only. "
                        f"Report each calling file as a file:line anchor "
                        f"(path:line)."
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


def select_fanin_symbols(
    snapshot_root: Path,
    max_symbols: int = FANIN_MAX_SYMBOLS,
    min_hop1: int = FANIN_MIN_HOP1_CALLER_FILES,
    min_hop3: int = FANIN_MIN_HOP3_CALLER_FILES,
) -> tuple[list[dict], list[dict]]:
    """Fan-in tier (v19 amendment A1/F2) symbol selection.

    Finds unambiguous Python function definitions with LARGE hop-1
    fan-in (>= ``min_hop1`` distinct caller files -- the historical
    3-20 filter is deliberately relaxed and the rg double-extraction
    agreement is NOT an inclusion criterion for this tier), then keeps
    up to ``max_symbols`` of them by descending hop-1 caller-file
    count, provided their hop-3 transitive caller set has at least
    ``min_hop3`` files (the context-flooding premise).

    Each selection record carries the F2 justification datum
    ``rg_output_bytes``: the byte size of ``rg -n '<symbol>' -g '*.py'``
    over the snapshot. Returns (selections, exclusions) with both
    fail-closed recorded (ambiguity and sub-threshold hop-3 sets).
    """
    index = oracle.build_function_definition_index(snapshot_root)
    edges = oracle.build_call_edges(snapshot_root, index)
    hop1_callers: dict[str, set[str]] = {}
    for caller, callee, _line in edges:
        hop1_callers.setdefault(callee, set()).add(caller)

    selections: list[dict] = []
    exclusions: list[dict] = []
    candidates = sorted(
        (
            (len(files), symbol)
            for symbol, files in hop1_callers.items()
            if len(files) >= min_hop1
        ),
        reverse=True,
    )
    for hop1_count, symbol in candidates:
        if len(selections) >= max_symbols:
            break
        reason = oracle._ambiguity_reason(symbol, index)
        if reason is not None:
            exclusions.append(
                {"symbol": symbol, "hop1_caller_files": hop1_count, "reason": reason}
            )
            continue
        if len(index.get(symbol, [])) != 1:
            # not a function definition in this snapshot (e.g. a builtin
            # callee) -- not eligible for a lookup/definition tier
            exclusions.append(
                {
                    "symbol": symbol,
                    "hop1_caller_files": hop1_count,
                    "reason": (
                        "callee is not a single unambiguous function "
                        "definition in the snapshot"
                    ),
                }
            )
            continue
        hop3 = oracle.callers_oracle_from(index, edges, symbol, FANIN_HOP_DEPTH)
        if len(hop3) < min_hop3:
            exclusions.append(
                {
                    "symbol": symbol,
                    "hop1_caller_files": hop1_count,
                    "reason": (
                        f"hop-3 caller file set ({len(hop3)}) below the "
                        f"F2 context-flooding threshold ({min_hop3})"
                    ),
                }
            )
            continue
        selections.append(
            {
                "symbol": symbol,
                "hop1_caller_files": hop1_count,
                "hop3_caller_files": len(hop3),
                "hop3_expected_files": hop3,
                "rg_output_bytes": oracle.rg_output_bytes(snapshot_root, symbol),
                "defining_files": index[symbol],
            }
        )
    return selections, exclusions


def generate_fanin_tasks(selections: list[dict]) -> list[dict]:
    """Fan-in tier tasks (tier "fanin", hop_depth 3) from selections.

    Prompt shape matches the amended Track A callers wording
    (v19 carry-over ratification: file:line anchors requested), with
    hop depth fixed at 3 per F2. rg agreement is intentionally NOT
    consulted (A1/F2 dropped it for this tier).
    """
    tasks: list[dict] = []
    for sel in selections:
        symbol = sel["symbol"]
        tasks.append(
            {
                "id": f"task-fanin-{symbol}",
                "tier": "fanin",
                "track": "A",
                "prompt": (
                    f"List every file in this repository that calls "
                    f"{symbol} within {FANIN_HOP_DEPTH} hop(s) of "
                    f"indirection. Report file paths only. Cite each file "
                    f"with file:line anchors for the calling lines."
                ),
                "hop_depth": FANIN_HOP_DEPTH,
                "expected_files": sel["hop3_expected_files"],
                "rg_output_bytes": sel["rg_output_bytes"],
                "hop1_caller_files": sel["hop1_caller_files"],
                "hop3_caller_files": sel["hop3_caller_files"],
            }
        )
    return sorted(tasks, key=lambda t: t["id"])


def generate_fanin_track_c_tasks(selections: list[dict]) -> list[dict]:
    """Track C lookups for the fan-in symbols (single-answer invariant).

    Every fan-in selection is an unambiguous function definition, so the
    oracle's lookup invariant (exactly one defining file) holds for the
    full selection; Track C is emitted only up to 15 tasks per F2.
    """
    tasks: list[dict] = []
    for sel in selections[:15]:
        symbol = sel["symbol"]
        tasks.append(
            {
                "id": f"task-fanin-c-lookup-{symbol}",
                "tier": "fanin",
                "track": "C",
                "prompt": (
                    f"Locate the definition of {symbol} and report its file path."
                ),
                "hop_depth": 1,
                "expected_files": sel["defining_files"],
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
