"""Offline tests for v19 error-session defect detection (issue #1696)."""

from __future__ import annotations

import pytest
from bench_v18 import runner as v18
from bench_v19 import pilot


def _info(**over):
    base = {
        "run_id": "r1", "arm": "mcp", "task": "task-x", "tier": "fanin",
        "killed": False, "defect": None, "cost_usd": 0.01,
        "aptu_tool_calls": 0,
        "tokens": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "stopReason": "stop", "final_text": "",
    }
    base.update(over)
    return base


def test_error_stop_records_defect():
    state = v18.LadderState()
    pilot.record_error_session_defect(state, _info(stopReason="error"))
    assert state.defects == [{
        "stage": "pilot", "task": "task-x", "arm": "mcp",
        "reason": "provider-error-stop",
    }]


def test_normal_stop_records_no_defect():
    state = v18.LadderState()
    pilot.record_error_session_defect(state, _info(stopReason="stop"))
    assert state.defects == []


def test_killed_session_excluded_from_provider_error_defect():
    state = v18.LadderState()
    pilot.record_error_session_defect(
        state, _info(stopReason="error", killed=True,
                     defect="wait-timeout-killed:900s"))
    assert state.defects == []


def test_detection_idempotent_per_cell():
    state = v18.LadderState()
    pilot.record_error_session_defect(state, _info(stopReason="error"))
    pilot.record_error_session_defect(state, _info(stopReason="error"))
    assert len(state.defects) == 1


def test_other_arm_and_task_get_own_entries():
    state = v18.LadderState()
    pilot.record_error_session_defect(state, _info(stopReason="error"))
    pilot.record_error_session_defect(
        state, _info(stopReason="error", arm="native"))
    pilot.record_error_session_defect(
        state, _info(stopReason="error", task="task-y"))
    assert len(state.defects) == 3
    assert all(d["reason"] == "provider-error-stop" for d in state.defects)


def test_summary_shape_matches_killed_defect_pattern():
    state = v18.LadderState()
    pilot.record_error_session_defect(state, _info(stopReason="error"))
    assert state.defects[0].keys() == {"stage", "task", "arm", "reason"}


@pytest.mark.parametrize("stage", ["pilot", "sealed"])
def test_stage_is_parameterizable(stage):
    state = v18.LadderState()
    pilot.record_error_session_defect(state, _info(stopReason="error"),
                                      stage=stage)
    assert state.defects[0]["stage"] == stage
