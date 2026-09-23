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


def _session_dir_with_jsonl(parent: Path, entries: list[dict]) -> Path:
    """Session dir in pi's on-disk layout: <timestamp>_<uuid>.jsonl."""
    d = parent / "2026-01-01T00-00-00-000Z_0123abcd-0000-0000-0000-000000000000"
    _write_jsonl(d / "session-file.jsonl", entries)
    return d


def test_over_budget_session_is_killed_and_recorded(tmp_path):
    sess = _session_dir_with_jsonl(tmp_path / "sess", [
        {"usage": {"cost": {"total": 0.30}}},
    ])
    state = runner.LadderState()
    result = runner.meter_and_close(
        state, "pilot", "t1", "native", "abc", sess, None
    )
    assert result.killed
    assert result.defect == "per-session-cap-exceeded:0.3000"
    assert len(state.defects) == 1


def test_nested_message_usage_and_cost_dict_are_parsed(tmp_path):
    # Live pi layout: usage nested under message, cost as component dict.
    sess = _session_dir_with_jsonl(tmp_path / "sess", [
        {"message": {"role": "assistant",
                     "usage": {"cost": {"input": 0.1, "output": 0.05,
                                          "total": 0.15}}}},
    ])
    state = runner.LadderState()
    result = runner.meter_and_close(
        state, "pilot", "t", "native", "abc", sess, None
    )
    assert not result.killed
    assert result.cost_usd == 0.15


def test_missing_usage_cost_fails_closed(tmp_path):
    # Malformed (string) cost must also kill.
    sess = _session_dir_with_jsonl(tmp_path / "sess", [
        {"usage": {"cost": {"total": "not-a-number"}}},
    ])
    state = runner.LadderState()
    result = runner.meter_and_close(
        state, "pilot", "t1", "mcp", "abc", sess, None
    )
    assert result.killed
    assert result.defect == "missing-or-malformed-usage-cost"
    # Absent session dir entirely: fail closed as well.
    state2 = runner.LadderState()
    result2 = runner.meter_and_close(
        state2, "pilot", "t2", "native", "abc", tmp_path / "nope", None
    )
    assert result2.killed
    assert result2.defect == "missing-or-malformed-usage-cost"


def test_cumulative_ceiling_halts_ladder(tmp_path):
    state = runner.LadderState()
    for i in range(3):
        sess = _session_dir_with_jsonl(
            tmp_path / f"s{i}", [{"usage": {"cost": {"total": 1.90}}}])
        runner.meter_and_close(state, "sealed", f"t{i}", "native", "x", sess, None)
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
    sess_file = session_dir / "2026-01-01T00-00-00-000Z_0123abcd.jsonl"
    writer = (
        "import pathlib,sys;"
        "p=pathlib.Path(sys.argv[1]);p.parent.mkdir(parents=True,exist_ok=True);"
        'p.write_text(\'{"usage":{"cost":{"total":0.5}}}\\n\')'
    )
    cmd = [sys.executable, "-c", writer, str(sess_file)]
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


def test_safe_env_preserves_provider_key_and_strips_other_secrets(monkeypatch):
    monkeypatch.setenv("ZAI_API_KEY", "provider-key")
    monkeypatch.setenv("SOME_SERVICE_API_KEY", "other-secret")
    monkeypatch.setenv("SOME_SERVICE_TOKEN", "other-token")
    env = runner._safe_env()
    assert env["ZAI_API_KEY"] == "provider-key"
    assert "SOME_SERVICE_API_KEY" not in env
    assert "SOME_SERVICE_TOKEN" not in env


def test_invocation_pins_preregistered_model(tmp_path):
    cmd, _env = runner.build_invocation(
        "mcp", tmp_path, tmp_path / "sessions" / "x" / "t" / "mcp", "prompt",
    )
    assert "--provider" in cmd and "zai" in cmd
    assert "--model" in cmd and "glm-5.3-flash" in cmd
    i_p, i_m = cmd.index("--provider"), cmd.index("--model")
    assert cmd[i_p + 1] == "zai" and cmd[i_m + 1] == "glm-5.3-flash"


def test_shadow_dirs_carry_isolation_config(tmp_path):
    for arm in ("native", "mcp"):
        runner.build_invocation(
            arm, tmp_path,
            tmp_path / "sessions" / "x" / "t" / arm, "prompt",
        )
        agent_dir = tmp_path / f"agent-{arm}"
        mcp = json.loads((agent_dir / "mcp.json").read_text())
        settings = json.loads((agent_dir / "settings.json").read_text())
        if arm == "native":
            assert mcp == {"mcpServers": {}}
            assert settings == {"packages": []}
        else:
            server = mcp["mcpServers"]["aptu-coder"]
            assert server["type"] == "stdio"
            assert server["directTools"] is True
            assert settings == {"packages": ["npm:pi-mcp-adapter"]}


def test_gateway_and_search_arm_wiring(tmp_path):
    for arm, direct_tools in (("mcp-gateway", False),
                              ("mcp-search", "search")):
        cmd, _ = runner.build_invocation(
            arm, tmp_path,
            tmp_path / "sessions" / "x" / "t" / arm, "prompt",
        )
        agent_dir = tmp_path / f"agent-{arm}"
        mcp = json.loads((agent_dir / "mcp.json").read_text())
        settings = json.loads((agent_dir / "settings.json").read_text())
        server = mcp["mcpServers"]["aptu-coder"]
        assert server["type"] == "stdio"
        assert server["command"] == "aptu-coder"
        assert server["directTools"] == direct_tools
        assert settings == {"packages": ["npm:pi-mcp-adapter"]}
        # All MCP-mode arms must present identical read-only tool
        # availability so only directTools varies across arms.
        assert "--exclude-tools" in cmd
        assert "aptu-coder_edit_overwrite,aptu-coder_edit_replace," \
               "aptu-coder_exec_command" in cmd


def test_unknown_arm_raises_value_error():
    try:
        runner.build_invocation(
            "mcp-lite", Path("/tmp"), Path("/tmp/s/x/t/mcp-lite"), "p",
        )
        raise AssertionError("expected ValueError")
    except ValueError as exc:
        assert "unknown arm" in str(exc)


def test_new_arms_write_to_disjoint_shadow_dirs(tmp_path):
    run_root = tmp_path / "run"
    run_root.mkdir()
    for arm in ("mcp-gateway", "mcp-search"):
        session_dir = run_root / "sessions" / "rid" / "t" / arm
        _, env = runner.build_invocation(arm, run_root, session_dir, "p")
        assert env["PI_CODING_AGENT_DIR"] == \
            str((run_root / f"agent-{arm}").resolve())
    assert (run_root / "agent-mcp-gateway").is_dir()
    assert (run_root / "agent-mcp-search").is_dir()


def test_common_flags_do_not_disable_extensions():
    cmd, _ = runner.build_invocation(
        "mcp", Path("/tmp"), Path("/tmp/s/x/t/mcp"), "p",
    )
    assert "--no-extensions" not in cmd
    assert "--no-skills" in cmd and "--no-context-files" in cmd
