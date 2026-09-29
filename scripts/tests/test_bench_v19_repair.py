"""Offline tests for v19 A8 automated repair mode (issue #1696).

The end-to-end test runs the pure helpers against the ratified sealed
worked example (docs/benchmarks/v19/results/tasks-fanin-hop2/sealed/)
and must reproduce its published before-after and arm-means values.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest
from bench_v18 import runner as v18
from bench_v19 import repair

REPO = Path(__file__).resolve().parent.parent.parent
SEALED_DIR = (REPO / "docs/benchmarks/v19/results/tasks-fanin-hop2"
              / "sealed")


def _session(task, arm, stop="stop", killed=False, defect=None, f1=0.0,
             verdict="no-anchor", cost=0.01):
    return {
        "run_id": f"run-{task}-{arm}", "arm": arm, "task": task,
        "tier": "fanin", "killed": killed, "defect": defect,
        "cost_usd": cost, "aptu_tool_calls": 0,
        "tokens": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "stopReason": stop,
        "score": {"verdict": verdict, "precision": 0.0, "recall": 0.0,
                  "f1": f1, "task_id": task, "expected_sha": "x"},
    }


# ---- detection feeds selection: error-stop cells are the repair set ----

def test_select_exactly_provider_error_cells():
    summary = {"sessions": [
        _session("t1", "native", "stop"),
        _session("t1", "mcp", "error"),
        _session("t2", "native", "error"),
        _session("t2", "mcp", "toolUse"),
    ]}
    assert repair.select_error_cells(summary) == [
        {"task": "t1", "arm": "mcp"},
        {"task": "t2", "arm": "native"},
    ]


@pytest.mark.parametrize("over", [
    {"stop": "error", "killed": True,
     "defect": "wait-timeout-killed:900s"},
    {"stop": "error", "killed": True,
     "defect": "turn-cap-exceeded:41"},
    {"stop": "stop"},
    {"stop": "toolUse"},
    {"stop": None},
])
def test_killed_and_non_error_sessions_excluded(over):
    summary = {"sessions": [_session("t1", "mcp", **over)]}
    assert repair.select_error_cells(summary) == []


def test_sealed_worked_example_selects_error_cells():
    sealed = json.loads((SEALED_DIR / "summary.json").read_text())
    cells = repair.select_error_cells(sealed)
    # Three cells ended stopReason "error" with no kill defect.
    # timezone/mcp also ended "error" but carries a
    # wait-timeout-killed defect, so it is excluded from repair
    # selection per the issue acceptance criteria (the hand-run A8
    # repair predated this rule and included it; its repair row still
    # renders in before-after via the before-session match).
    assert cells == [
        {"task": "task-fanin-chain", "arm": "mcp"},
        {"task": "task-fanin-dec", "arm": "mcp"},
        {"task": "task-fanin-include", "arm": "native"},
    ]


# ---- filtered task file: frozen set untouched ----

def _write_task_file(tmp_path, ids):
    tasks = [{"id": i, "track": "A", "hop_depth": 2, "prompt": "p",
              "expected_files": ["a.rs"], "symbol": "s"} for i in ids]
    p = tmp_path / "frozen-tasks.json"
    p.write_text(json.dumps(tasks), encoding="utf-8")
    return p


def test_filtered_task_file_written_outside_frozen_set(tmp_path):
    frozen = _write_task_file(tmp_path, ["t1", "t2", "t3"])
    cells = [{"task": "t2", "arm": "mcp"}, {"task": "t3", "arm": "native"}]
    out = repair.write_filtered_task_file([frozen], cells,
                                          tmp_path / "repair-tmp")
    assert out == tmp_path / "repair-tmp" / "tasks-repair-filtered.json"
    assert out.parent != frozen.parent
    got = json.loads(out.read_text(encoding="utf-8"))
    assert [t["id"] for t in got] == ["t2", "t3"]


def test_frozen_task_file_byte_identical_after_repair_write(tmp_path):
    frozen = _write_task_file(tmp_path, ["t1", "t2"])
    before = hashlib.sha256(frozen.read_bytes()).hexdigest()
    cells = [{"task": "t1", "arm": "native"}]
    repair.write_filtered_task_file([frozen], cells, tmp_path / "tmp2")
    assert hashlib.sha256(frozen.read_bytes()).hexdigest() == before


def test_frozen_task_file_byte_identical_after_full_repair_flow(tmp_path):
    frozen = _write_task_file(tmp_path, ["t1", "t2"])
    before = hashlib.sha256(frozen.read_bytes()).hexdigest()
    summary = {"sessions": [
        _session("t1", "native", "stop", f1=0.5, verdict="partial"),
        _session("t1", "mcp", "error"),
        _session("t2", "native", "error"),
        _session("t2", "mcp", "stop", f1=0.1),
    ]}
    repair.write_filtered_task_file(
        [frozen], repair.select_error_cells(summary), tmp_path / "tmp3")
    assert hashlib.sha256(frozen.read_bytes()).hexdigest() == before


# ---- A8 replacement rule ----

def test_replacement_only_where_repair_completed():
    before = {"sessions": [
        _session("t1", "mcp", "error"),
        _session("t2", "mcp", "error"),
        _session("t3", "mcp", "error"),
    ]}
    repairs = [
        _session("t1", "mcp", "stop", f1=0.8, verdict="partial"),
        _session("t2", "mcp", "toolUse", killed=True,
                 defect="turn-cap-exceeded:41"),
        _session("t3", "mcp", "error"),
    ]
    rows = repair.before_after(before, repairs)
    assert [(r["task"], r["replaced"]) for r in rows] == [
        ("t1", True), ("t2", False), ("t3", False)]
    adjusted = repair.apply_replacement(before, repairs)
    by_cell = {(s["task"], s["arm"]): s for s in adjusted}
    assert by_cell[("t1", "mcp")]["stopReason"] == "stop"
    assert by_cell[("t2", "mcp")]["stopReason"] == "error"
    assert by_cell[("t3", "mcp")]["stopReason"] == "error"


def test_repair_completed_but_killed_stays_as_sealed():
    before = {"sessions": [_session("t1", "mcp", "error")]}
    repairs = [_session("t1", "mcp", "stop", killed=True,
                        defect="wait-timeout-killed:900s", f1=0.9)]
    rows = repair.before_after(before, repairs)
    assert rows[0]["replaced"] is False
    assert repair.apply_replacement(before, repairs)[0]["stopReason"] == "error"


def test_repair_adjusted_arm_means():
    before = {"sessions": [
        _session("t1", "native", "error"),
        _session("t1", "mcp", "stop", f1=0.4, verdict="partial"),
    ]}
    repairs = [_session("t1", "native", "stop", f1=0.8, verdict="partial")]
    adjusted = repair.apply_replacement(before, repairs)
    means = repair.arm_means(adjusted)
    assert means == {"mcp_mean_f1": 0.4, "mcp_completed": "1/1",
                     "native_mean_f1": 0.8, "native_completed": "1/1"}
    assert repair.arm_means(before["sessions"])["native_mean_f1"] == 0.0


# ---- end-to-end against the ratified sealed worked example ----

def test_worked_example_reproduces_published_values():
    sealed = json.loads((SEALED_DIR / "summary.json").read_text())
    repairs = json.loads(
        (SEALED_DIR / "repairs/summary.json").read_text())
    published_ba = json.loads(
        (SEALED_DIR / "repairs/before-after.json").read_text())
    published_means = json.loads(
        (SEALED_DIR / "repairs/arm-means.json").read_text())

    rows = repair.before_after(sealed, repairs["sessions"])
    for got, want in zip(rows, published_ba):
        for key in ("task", "arm", "before_f1", "before_stop",
                    "before_verdict", "after_f1", "after_stop",
                    "after_defect", "after_verdict", "replaced"):
            assert got[key] == want[key], (got, key)

    adjusted = repair.apply_replacement(sealed, repairs["sessions"])
    assert repair.arm_means(sealed["sessions"]) == published_means[
        "as_sealed"]
    assert repair.arm_means(adjusted) == published_means["repair_adjusted"]


def test_empty_selection_is_noop(tmp_path):
    assert repair.select_error_cells({"sessions": []}) == []
    assert repair.select_error_cells({"sessions": [
        _session("t1", "mcp", "stop")]}) == []
