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
