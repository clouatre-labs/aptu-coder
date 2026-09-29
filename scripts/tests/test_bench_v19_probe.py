"""Offline tests for the bench_v19 cap-sensitivity probe driver."""

from __future__ import annotations

import hashlib
import json

import pytest
from bench_v18 import runner as v18
from bench_v19 import pilot, probe
from bench_v19 import runner_v19 as rv19

TASKS = [
    {
        "id": "chain",
        "track": "A",
        "hop_depth": 2,
        "prompt": "p",
        "expected_files": ["a.py"],
    },
    {
        "id": "timezone",
        "track": "C",
        "hop_depth": 2,
        "prompt": "p",
        "expected_files": ["b.py"],
    },
]
SNAPSHOT = "/unused"
CELLS = [
    ("chain", "mcp"),
    ("timezone", "mcp"),
    ("chain", "native"),
    ("timezone", "native"),
]
# runner_v19 sets 300 at import (env-driven carry-over); the frozen v18
# file default stays 120 and is never edited.
_BASELINE_WAIT = v18.SESSION_WAIT_TIMEOUT_S


def _fake_run_one(results):
    """Run-one stand-in: pops scripted results; records each launch."""
    launches: list[dict] = []

    def run_one(state, task, arm, tier, snapshot, run_root):
        # Mirror the inherited pilot gate before any launch.
        if not v18.run_stage_budget_ok(state, pilot.STAGE):
            return {"skipped": f"stage-cap:{state.halt_reason}"}
        if state.total_spent_usd >= pilot.STAGE_CAP:
            return {"skipped": "cumulative-cap-reached"}
        launches.append(
            {
                "task": task["id"],
                "arm": arm,
                "turn_cap": rv19.TIERS[tier]["turn_cap"],
                "wait": v18.SESSION_WAIT_TIMEOUT_S,
            }
        )
        info = results.pop(0)
        state.total_spent_usd += info.get("cost_usd", 0.01)
        return info

    return run_one, launches


def _ok_info(task, cap=None, defect=None):
    return {
        "run_id": f"r-{task}",
        "killed": defect is not None,
        "defect": defect,
        "cost_usd": 0.01,
        "stopReason": "stop",
    }


# ---- expand_grid: deterministic, every combination exactly once ----


def test_expand_grid_deterministic_covers_every_combination_once():
    g1 = probe.expand_grid(CELLS)
    g2 = probe.expand_grid(CELLS)
    assert g1 == g2
    keys = {(p["task"], p["arm"], p["turn_cap"], p["wait_timeout_s"]) for p in g1}
    assert len(keys) == len(g1)
    assert len(g1) == len(CELLS) * len(probe.TURN_CAPS) * len(probe.WAIT_TIMEOUTS_S)
    for task, arm in CELLS:
        for cap in probe.TURN_CAPS:
            for wait in probe.WAIT_TIMEOUTS_S:
                assert {
                    "task": task,
                    "arm": arm,
                    "turn_cap": cap,
                    "wait_timeout_s": wait,
                } in g1


# ---- restore invariants: TIERS and v18 wait timeout byte-equal ----


@pytest.fixture(autouse=True)
def _protect_constants():
    saved_caps = {t: rv19.TIERS[t]["turn_cap"] for t in rv19.TIERS}
    saved_wait = v18.SESSION_WAIT_TIMEOUT_S
    yield
    for t, cap in saved_caps.items():
        rv19.TIERS[t]["turn_cap"] = cap
    v18.SESSION_WAIT_TIMEOUT_S = saved_wait


def test_restore_invariant_after_full_grid(tmp_path, monkeypatch):
    run_one, launches = _fake_run_one(
        [_ok_info(t) for (t, _a) in CELLS for _ in range(9)]
    )
    before_wait = _BASELINE_WAIT
    before_caps = dict(rv19.TIERS)
    report = probe.run_grid(TASKS, SNAPSHOT, tmp_path, cells=CELLS, run_one=run_one)
    assert not report["aborted"]
    assert len(launches) == 36
    assert v18.SESSION_WAIT_TIMEOUT_S == before_wait
    assert rv19.TIERS == before_caps
    # Per-grid-point override actually applied at launch time.
    assert {l["turn_cap"] for l in launches} == {25, 40, 60}
    assert {l["wait"] for l in launches} == {300, 900, 1800}


def test_restore_invariant_after_mid_grid_abort(tmp_path):
    results = [_ok_info("chain")] + [{"skipped": "cumulative-cap-reached"}]
    run_one, _launches = _fake_run_one(results)
    before_caps = dict(rv19.TIERS)
    report = probe.run_grid(TASKS, SNAPSHOT, tmp_path, cells=CELLS, run_one=run_one)
    assert report["aborted"]
    assert report["abort_reason"] == "cumulative-cap-reached"
    assert rv19.TIERS == before_caps
    assert v18.SESSION_WAIT_TIMEOUT_S == _BASELINE_WAIT


def test_restore_invariant_on_exception(tmp_path):
    def boom(*a, **k):
        raise RuntimeError("mid-session")

    with pytest.raises(RuntimeError):
        probe.run_grid(TASKS, SNAPSHOT, tmp_path, cells=CELLS, run_one=boom)
    assert v18.SESSION_WAIT_TIMEOUT_S == _BASELINE_WAIT


def test_poll_s_must_not_exceed_smallest_wait_timeout(tmp_path):
    with pytest.raises(ValueError, match="poll_s"):
        probe.run_grid(
            TASKS, SNAPSHOT, tmp_path, cells=CELLS, run_one=lambda *a: {}, poll_s=300.01
        )


# ---- skipped result aborts the grid, no binding row ----


def test_skipped_result_aborts_without_binding_row(tmp_path):
    results = [
        _ok_info("chain"),
        _ok_info("chain"),
        _ok_info("chain"),
        {"skipped": "stage-cap:per-session ceiling"},
    ]
    run_one, launches = _fake_run_one(results)
    report = probe.run_grid(TASKS, SNAPSHOT, tmp_path, cells=CELLS, run_one=run_one)
    assert report["aborted"] and len(launches) == 4
    assert all(r["task"] == "chain" for r in report["rows"])


# ---- budget gate fires before session launch ----


def test_budget_gate_blocks_launch_when_spent(tmp_path):
    results = [_ok_info("chain")]
    run_one, launches = _fake_run_one(results)
    state = v18.LadderState()
    state.total_spent_usd = pilot.STAGE_CAP
    report = probe.run_grid(
        TASKS, SNAPSHOT, tmp_path, state=state, cells=CELLS, run_one=run_one
    )
    assert report["aborted"] and launches == []
    assert report["rows"] == []


# ---- report artifact shape and cap-bound flags ----


def test_report_rows_shape_and_flags(tmp_path):
    results = []
    for _ in ("mcp", "native"):
        for cap in probe.TURN_CAPS:
            for wait in probe.WAIT_TIMEOUTS_S:
                if _ == "mcp":
                    results.append(_ok_info("chain", defect=f"turn-cap-exceeded:{cap}"))
                else:
                    results.append(
                        _ok_info("chain")
                        if wait < 900
                        else {
                            "run_id": "r-tw",
                            "killed": True,
                            "defect": f"wait-timeout-killed:{wait}s",
                            "cost_usd": 0.01,
                            "stopReason": None,
                        }
                    )
    run_one, _ = _fake_run_one(results)
    # Shrink cells to one task/arm pair for scripting simplicity.
    report = probe.run_grid(
        TASKS,
        SNAPSHOT,
        tmp_path,
        cells=[("chain", "mcp"), ("chain", "native")],
        run_one=run_one,
    )
    probe.write_artifacts(report, tmp_path)
    got = json.loads((tmp_path / "cap-sensitivity.json").read_text(encoding="utf-8"))
    assert len(got["rows"]) == 18
    for row in got["rows"]:
        assert {
            "task",
            "arm",
            "turn_cap",
            "wait_timeout_s",
            "run_id",
            "stopReason",
            "killed",
            "defect",
            "turns_at_termination",
            "wall_clock_s_at_termination",
            "turn_cap_bound",
            "wait_deadline_bound",
            "cost_usd",
        } <= set(row)
    by_cap = {r["turn_cap"]: r for r in got["rows"] if r["arm"] == "mcp"}
    for cap, row in by_cap.items():
        assert row["turn_cap_bound"] is True
        assert row["defect"].startswith("turn-cap-exceeded")
    tw = {r["wait_timeout_s"]: r for r in got["rows"] if r["arm"] == "native"}
    assert tw[900]["wait_deadline_bound"] is True
    assert tw[1800]["wait_deadline_bound"] is True
    assert tw[900]["wall_clock_s_at_termination"] == 900.0
    assert tw[300]["wait_deadline_bound"] is False
    # Manifest declares the full grid and budget up front.
    manifest = json.loads((tmp_path / "manifest.json").read_text(encoding="utf-8"))
    assert manifest["turn_caps"] == probe.TURN_CAPS
    assert manifest["wait_timeouts_s"] == probe.WAIT_TIMEOUTS_S
    assert manifest["budget"]["stage_cap_usd"] == pilot.STAGE_CAP
    assert manifest["budget"]["declared_up_front"] is True


# ---- frozen-constant invariance after a probe run ----


def test_frozen_v18_runner_file_unchanged_after_probe_run(tmp_path):
    frozen = probe.REPO / "scripts" / "bench_v18" / "runner.py"
    before = hashlib.sha256(frozen.read_bytes()).hexdigest()
    run_one, _ = _fake_run_one([_ok_info(t) for (t, _a) in CELLS for _ in range(9)])
    probe.run_grid(TASKS, SNAPSHOT, tmp_path, cells=CELLS, run_one=run_one)
    assert hashlib.sha256(frozen.read_bytes()).hexdigest() == before
    assert v18.SESSION_WAIT_TIMEOUT_S == _BASELINE_WAIT
    assert v18.PER_SESSION_KILL_USD == 0.25
    assert v18.STAGE_BUDGET_CAPS_USD["pilot"] == pilot.STAGE_CAP
