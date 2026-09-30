"""Offline tests for v19 A9 verify-repair of fabricated-anchor sessions."""

from __future__ import annotations

import json

import pytest
from bench_v19 import verify_repair as vr
from bench_v19.score import verify_anchors


def _session(task, arm, stop="stop", killed=False, defect=None, f1=0.0,
             verdict="no-anchor", cost=0.01, symbol="bar"):
    return {
        "run_id": f"run-{task}-{arm}", "arm": arm, "task": task,
        "tier": "fanin", "killed": killed, "defect": defect,
        "symbol": symbol,
        "cost_usd": cost, "aptu_tool_calls": 0,
        "tokens": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "stopReason": stop,
        "score": {"verdict": verdict, "precision": 0.0, "recall": 0.0,
                  "f1": f1, "task_id": task, "expected_sha": "x"},
    }


# ---- selection: only completed fabricated-anchor cells ----

def test_select_exactly_fabricated_completed_cells():
    summary = {"sessions": [
        _session("t1", "native", verdict="fabricated-anchor"),
        _session("t1", "mcp", "error", verdict="fabricated-anchor"),
        _session("t2", "native", verdict="partial"),
        _session("t2", "mcp", verdict="fabricated-anchor"),
    ]}
    assert vr.select_fabricated_cells(summary) == [
        {"task": "t1", "arm": "native"},
        {"task": "t2", "arm": "mcp"},
    ]


@pytest.mark.parametrize("over", [
    {"stop": "stop", "killed": True, "defect": "wait-timeout-killed:900s"},
    {"stop": "stop", "defect": "provider-error-stop"},
    {"stop": "error", "verdict": "fabricated-anchor"},
    {"stop": "toolUse", "verdict": "fabricated-anchor"},
    {"stop": None, "verdict": "fabricated-anchor"},
    {"stop": "stop", "verdict": "partial"},
])
def test_error_killed_defect_nonfabricated_excluded(over):
    summary = {"sessions": [_session("t1", "mcp", **over)]}
    assert vr.select_fabricated_cells(summary) == []


def test_empty_selection_is_noop():
    assert vr.select_fabricated_cells({"sessions": []}) == []


# ---- filtered task file: frozen set untouched ----

def _write_task_file(tmp_path, ids):
    tasks = [{"id": i, "track": "A", "hop_depth": 2, "prompt": "p",
              "expected_files": ["a.rs"], "symbol": "s"} for i in ids]
    p = tmp_path / "frozen-tasks.json"
    p.write_text(json.dumps(tasks), encoding="utf-8")
    return p


def test_filtered_task_file_written_outside_frozen_set(tmp_path):
    frozen = _write_task_file(tmp_path, ["t1", "t2", "t3"])
    cells = [{"task": "t2", "arm": "mcp"}]
    out = vr.write_filtered_task_file([frozen], cells, tmp_path / "tmp")
    assert out == tmp_path / "tmp" / "tasks-verify-repair-filtered.json"
    assert out.parent != frozen.parent
    got = json.loads(out.read_text(encoding="utf-8"))
    assert [t["id"] for t in got] == ["t2"]


def test_frozen_task_file_byte_identical_after_repair_write(tmp_path):
    import hashlib
    frozen = _write_task_file(tmp_path, ["t1", "t2"])
    before = hashlib.sha256(frozen.read_bytes()).hexdigest()
    vr.write_filtered_task_file(
        [frozen],
        vr.select_fabricated_cells({"sessions": [
            _session("t2", "mcp", verdict="fabricated-anchor")]}),
        tmp_path / "tmp2")
    assert hashlib.sha256(frozen.read_bytes()).hexdigest() == before


# ---- before-after replacement flag ----

def _snap_with_fabricated(tmp_path):
    p = tmp_path / "a/b.py"
    p.parent.mkdir(parents=True)
    p.write_text("bar()\n", encoding="utf-8")
    return tmp_path


def test_replacement_only_where_repair_completed_clean(tmp_path):
    snap = _snap_with_fabricated(tmp_path)
    before = {"sessions": [
        _session("t1", "mcp", verdict="fabricated-anchor"),
        _session("t2", "mcp", verdict="fabricated-anchor"),
        _session("t3", "mcp", verdict="fabricated-anchor"),
    ]}
    # t1: completed and clean; t2: still fabricated; t3: did not complete.
    repairs = [
        _session("t1", "mcp", f1=0.8, verdict="partial"),
        _session("t2", "mcp", f1=0.8, verdict="fabricated-anchor"),
        _session("t3", "mcp", "error"),
    ]
    repairs[0]["final_text"] = "see a/b.py:1 for bar"
    repairs[1]["final_text"] = "see a/b.py:99 for bar"
    repairs[2]["final_text"] = ""
    rows = vr.before_after(before, repairs, snap)
    assert [(r["task"], r["replaced"]) for r in rows] == [
        ("t1", True), ("t2", False), ("t3", False)]
    adjusted = vr.apply_replacement(before, repairs, snap)
    by_cell = {(s["task"], s["arm"]): s for s in adjusted}
    assert by_cell[("t1", "mcp")]["score"]["verdict"] == "partial"
    assert by_cell[("t2", "mcp")]["score"]["verdict"] == "fabricated-anchor"
    assert by_cell[("t3", "mcp")]["score"]["verdict"] == "fabricated-anchor"


def test_repair_not_completed_keeps_original_as_sealed(tmp_path):
    snap = _snap_with_fabricated(tmp_path)
    before = {"sessions": [
        _session("t1", "mcp", verdict="fabricated-anchor", f1=0.2)]}
    repairs = [_session("t1", "mcp", "error", f1=0.9, verdict="partial")]
    repairs[0]["final_text"] = "see a/b.py:1"
    rows = vr.before_after(before, repairs, snap)
    assert rows[0]["replaced"] is False
    assert vr.apply_replacement(before, repairs, snap)[0]["score"]["f1"] == 0.2


def test_arm_means():
    sessions = [
        _session("t1", "mcp", f1=0.4, verdict="partial"),
        _session("t1", "native", f1=0.8, verdict="partial"),
    ]
    assert vr.arm_means(sessions) == {
        "mcp_mean_f1": 0.4, "mcp_completed": "1/1",
        "native_mean_f1": 0.8, "native_completed": "1/1"}


def test_fabricated_anchors_in_and_corrective_prefix(tmp_path):
    snap = _snap_with_fabricated(tmp_path)
    fab = vr.fabricated_anchors_in("see a/b.py:99", "bar", snap)
    assert fab == ["a/b.py:99"]
    note = vr.corrective_prefix(fab, "bar")
    assert "a/b.py:99" in note and "distinct identifier" in note
    assert verify_anchors("see a/b.py:1", "bar", snap) == (1, 0)
