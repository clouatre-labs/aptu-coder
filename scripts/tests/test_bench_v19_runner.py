"""Offline tests for the bench_v19 runner adapter."""

from __future__ import annotations

import bench_v18.runner as v18_runner
import pytest
from bench_v19 import runner as v19_runner

ARMS_AND_CELLS = [
    ("native", "A", 1, True),
    ("native", "C", 1, True),
    ("mcp", "A", 3, True),
    ("mcp", "C", 2, True),
    # mcp-gateway tax-control arm: hop-1 Track C cells only (PR #1621).
    ("mcp-gateway", "C", 1, True),
    ("mcp-gateway", "C", 2, False),
    ("mcp-gateway", "A", 1, False),
]


def test_import_does_not_mutate_bench_v18_constants():
    assert v18_runner.ARMS == ("native", "mcp", "mcp-gateway", "mcp-search")
    assert v18_runner.PER_SESSION_KILL_USD == 0.25
    assert v18_runner.TOTAL_CEILING_USD == 5.0
    assert v18_runner.MODEL == "glm-5.3-flash"
    assert v19_runner.V19_ARMS == ("native", "mcp", "mcp-gateway")


@pytest.mark.parametrize(("arm", "track", "hop", "expected"), ARMS_AND_CELLS)
def test_arm_allowed_for_cell(arm, track, hop, expected):
    assert v19_runner.arm_allowed_for_cell(arm, track, hop) is expected


def test_unknown_arm_rejected():
    with pytest.raises(ValueError, match="unknown arm"):
        v19_runner.arm_allowed_for_cell("mcp-search", "C", 1)
    with pytest.raises(ValueError, match="unknown arm"):
        v19_runner.build_invocation(
            "mcp-search",
            None,
            None,
            "prompt",
        )


def test_build_invocation_matches_v18_for_native(tmp_path):
    session_dir = tmp_path / "s"
    cmd, env = v19_runner.build_invocation(
        "native",
        tmp_path,
        session_dir,
        "p",
    )
    expected_cmd, expected_env = v18_runner.build_invocation(
        "native",
        tmp_path,
        tmp_path / "s2",
        "p",
    )
    assert cmd[:2] == expected_cmd[:2] == ["pi", "-p"]
    assert env == expected_env


def test_ladder_cells_restrict_gateway_to_hop1_track_c():
    tasks = [
        {"id": "t-a1", "track": "A", "hop_depth": 1},
        {"id": "t-c1", "track": "C", "hop_depth": 1},
        {"id": "t-c2", "track": "C", "hop_depth": 2},
    ]
    cells = v19_runner.ladder_cells(tasks)
    gateway = [c["task_id"] for c in cells if c["arm"] == "mcp-gateway"]
    assert gateway == ["t-c1"]


# ---- A7b: wait-deadline kills are recorded as fail-closed defects ----

import json as _json  # noqa: E402

from bench_v19 import runner_v19 as rv19_mod  # noqa: E402


class _FakeProc:
    """Minimal Popen stand-in: never exits on its own."""

    def __init__(self):
        pass

    def wait(self, timeout=None):
        import subprocess
        raise subprocess.TimeoutExpired(cmd="fake", timeout=timeout)

    def kill(self):
        pass


def test_wait_timeout_kill_recorded_as_defect(tmp_path, monkeypatch):
    from bench_v18 import runner as v18

    session_dir = tmp_path / "sess"
    session_dir.mkdir()
    (session_dir / "s.jsonl").write_text("", encoding="utf-8")
    monkeypatch.setattr(v18, "SESSION_WAIT_TIMEOUT_S", 0)
    monkeypatch.setattr(v18, "session_jsonl_files", lambda d: [session_dir / "s.jsonl"])
    monkeypatch.setattr(
        v18, "validate_session_dir", lambda d, r: d
    )
    metered = v18.SessionResult("pilot", "t", "mcp", "r", False, None, 0.01)
    monkeypatch.setattr(v18, "meter_and_close", lambda *a, **k: metered)

    state = v18.LadderState()
    result = rv19_mod.run_session_with_turn_cap(
        state, "pilot", "task", "mcp", "run1",
        session_dir, ["true"], {}, tmp_path,
        turn_cap=40, poll_s=0.0,
    )
    assert result.killed is True
    assert result.defect.startswith("wait-timeout-killed:")
    assert any(d["reason"] == result.defect for d in state.defects)
