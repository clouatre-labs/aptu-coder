"""Runner tests: fail-closed cost kills and cumulative ceiling halt.

These are synthetic unit tests; no live pi session is ever launched.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from bench_v18 import runner


def _write_jsonl(path: Path, entries: list[dict]) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "".join(json.dumps(e) + "\n" for e in entries), encoding="utf-8"
    )
    return path


def test_over_budget_session_is_killed_and_recorded(tmp_path):
    jsonl = _write_jsonl(tmp_path / "session.jsonl", [
        {"usage": {"cost": 0.30}},
    ])
    state = runner.LadderState()
    result = runner.meter_and_close(
        state, "pilot", "t1", "native", "abc", jsonl, None
    )
    assert result.killed
    assert result.defect == "per-session-cap-exceeded:0.3000"
    assert len(state.defects) == 1


def test_missing_usage_cost_fails_closed(tmp_path):
    # Malformed (string) cost must also kill.
    jsonl = _write_jsonl(tmp_path / "session.jsonl", [
        {"usage": {"cost": "not-a-number"}},
    ])
    state = runner.LadderState()
    result = runner.meter_and_close(
        state, "pilot", "t1", "mcp", "abc", jsonl, None
    )
    assert result.killed
    assert result.defect == "missing-or-malformed-usage-cost"
    # Absent JSONL entirely: fail closed as well.
    state2 = runner.LadderState()
    result2 = runner.meter_and_close(
        state2, "pilot", "t2", "native", "abc", tmp_path / "nope.jsonl", None
    )
    assert result2.killed
    assert result2.defect == "missing-or-malformed-usage-cost"


def test_cumulative_ceiling_halts_ladder(tmp_path):
    state = runner.LadderState()
    for i in range(3):
        jsonl = _write_jsonl(tmp_path / f"s{i}.jsonl", [
            {"usage": {"cost": 1.90}},
        ])
        runner.meter_and_close(state, "sealed", f"t{i}", "native", "x", jsonl, None)
    assert state.total_spent_usd > runner.TOTAL_CEILING_USD
    assert state.halted
    assert state.halt_reason == "total-ceiling-exceeded"


def test_stage_budget_cap_halts_before_entry(tmp_path):
    state = runner.LadderState()
    state.total_spent_usd = 0.11
    assert not runner.run_stage_budget_ok(state, "smoke")
    assert state.halted
    assert state.halt_reason == "stage-budget-cap-exceeded:smoke"
    # Under the pilot cap (0.60) the ladder proceeds.
    state2 = runner.LadderState()
    state2.total_spent_usd = 0.11
    assert runner.run_stage_budget_ok(state2, "pilot")


def test_session_dir_traversal_rejected(tmp_path):
    run_root = tmp_path / "run"
    run_root.mkdir()
    bad = run_root / ".." / "escape"
    try:
        runner.validate_session_dir(bad, run_root)
        raise AssertionError("expected ValueError")
    except ValueError as exc:
        assert "traversal" in str(exc)
    outside = tmp_path / "elsewhere"
    try:
        runner.validate_session_dir(outside, run_root)
        raise AssertionError("expected ValueError")
    except ValueError as exc:
        assert "outside" in str(exc)
    ok = runner.validate_session_dir(run_root / "sessions" / "r1", run_root)
    assert ok.is_absolute()


def test_synthetic_cost_kill_end_to_end(tmp_path):
    """Demo of the kill path with a synthetic child command, no pi."""
    run_root = tmp_path / "run"
    run_root.mkdir()
    session_dir = run_root / "sessions" / "rid" / "t" / "native"
    writer = (
        "import pathlib,sys;"
        "p=pathlib.Path(sys.argv[1]);p.parent.mkdir(parents=True,exist_ok=True);"
        'p.write_text(\'{"usage":{"cost":0.5}}\\n\')'
    )
    cmd = [sys.executable, "-c", writer, str(session_dir / "session.jsonl")]
    state = runner.LadderState()
    result = runner.run_session(
        state, "smoke", "t", "native", "rid", session_dir, cmd,
        {}, run_root,
    )
    assert result.killed
    assert result.cost_usd == 0.5
    assert state.defects[0]["reason"].startswith("per-session-cap-exceeded")


def test_run_ids_are_opaque_and_random():
    a, b = runner.new_run_id(), runner.new_run_id()
    assert a != b and len(a) == 32
