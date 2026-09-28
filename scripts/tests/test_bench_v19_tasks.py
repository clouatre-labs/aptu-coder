"""Offline tests for the bench_v19 task generator and discriminative filter."""

from __future__ import annotations

from bench_v19.generate_tasks import (
    F1_GAP_THRESHOLD,
    FANIN_SEALED_HOP_DEPTH,
    SEALED_N_TASKS,
    apply_discriminative_filter,
    filter_pilot_tasks,
    generate_fanin_tasks,
    generate_tasks,
    generate_track_c_tasks,
    select_fanin_symbols,
)
from bench_v19.runner_v19 import activation_gate

ORACLE = [
    {
        "id": "q-a-hop1-sym",
        "track": "A",
        "symbol": "sym",
        "hop_depth": 1,
        "expected_files": ["a.py", "b.py"],
    },
    {
        "id": "q-a-hop2-sym",
        "track": "A",
        "symbol": "sym",
        "hop_depth": 2,
        "expected_files": ["a.py", "b.py", "c.py"],
    },
    {
        "id": "q-a-hop3-sym",
        "track": "A",
        "symbol": "sym",
        "hop_depth": 3,
        "expected_files": ["a.py", "b.py", "c.py", "d.py"],
    },
    {
        "id": "q-c-loc-sym",
        "track": "C",
        "symbol": "sym",
        "hop_depth": 1,
        "expected_files": ["a.py"],
    },
]


def test_generate_tasks_deterministic_and_stratified():
    first = generate_tasks(ORACLE)
    second = generate_tasks(ORACLE)
    assert first == second
    hop_tasks = [t for t in first if t["track"] == "A"]
    assert [t["hop_depth"] for t in hop_tasks] == [1, 2, 3]
    assert first == sorted(first, key=lambda t: t["id"])
    lookup = [t for t in first if t["track"] == "C"]
    assert len(lookup) == 1


def test_filter_f1_gap_threshold_and_one_entity_exclusion():
    assert F1_GAP_THRESHOLD == 0.2
    multi = {"track": "A", "expected_files": ["a.py", "b.py", "c.py"]}
    # gap >= 0.2 in either direction -> keep
    assert apply_discriminative_filter(multi, 0.9, 0.5, divergent=False)
    assert apply_discriminative_filter(multi, 0.5, 0.9, divergent=False)
    # gap < 0.2 -> drop
    assert not apply_discriminative_filter(multi, 0.9, 0.8, divergent=False)
    # exactly 1 expected entity: excluded from the F1-gap rule
    single = {"track": "A", "expected_files": ["a.py"]}
    assert apply_discriminative_filter(single, 0.9, 0.9, divergent=True)
    assert not apply_discriminative_filter(single, 0.9, 0.1, divergent=False)
    # Track C decided purely by categorical divergence
    track_c = {"track": "C", "expected_files": ["a.py"]}
    assert apply_discriminative_filter(track_c, 0.9, 0.9, divergent=True)
    assert not apply_discriminative_filter(track_c, 0.1, 0.9, divergent=False)


def test_filter_pilot_tasks_returns_matching_survivors():
    tasks = generate_tasks(ORACLE)
    results = [
        {
            "task_id": f"task-{t['id']}",
            "f1_native": 0.9,
            "f1_mcp": 0.5,
            "divergent": False,
        }
        for t in tasks
    ]
    survivors = filter_pilot_tasks(tasks, results)
    assert survivors == sorted(survivors, key=lambda t: t["id"])
    # The 1-entity Track C cell survives only via divergence; it was not.
    assert all(t["track"] == "A" for t in survivors)


def test_track_c_generation_from_definition_index():
    index = {
        "sym": ["lib.py"],
        "other": ["pkg/mod.rs"],
        "dup": ["a.py", "b.py"],
        "ghost": [],
    }
    tasks = generate_track_c_tasks(index)
    assert [t["id"] for t in tasks] == ["task-c-lookup-other", "task-c-lookup-sym"]
    by_id = {t["id"]: t for t in tasks}
    assert by_id["task-c-lookup-sym"]["expected_files"] == ["lib.py"]
    assert by_id["task-c-lookup-sym"]["track"] == "C"
    # Track C stays hop-1 lookup (mcp-gateway tax-control gate).
    assert all(t["hop_depth"] == 1 for t in tasks)
    # Single-answer invariant: ambiguous (multi-file) and empty symbols
    # are skipped.
    assert "task-c-lookup-dup" not in by_id
    assert "task-c-lookup-ghost" not in by_id
    assert all(len(t["expected_files"]) == 1 for t in tasks)


def test_ambiguous_symbols_excluded_from_both_tracks():
    from bench_v19.generate_tasks import collect_exclusions

    entries = [
        {
            "id": "q-a-hop1-dup",
            "track": "A",
            "symbol": "dup",
            "hop_depth": 1,
            "expected_files": [],
            "crosscheck_agrees": True,
            "excluded_reason": (
                "ambiguous symbol 'dup': defined in 2 files (a.py, b.py); "
                "excluded from task generation"
            ),
        },
        {
            "id": "q-c-loc-dup",
            "track": "C",
            "symbol": "dup",
            "hop_depth": 1,
            "expected_files": ["a.py", "b.py"],
            "excluded_reason": "ambiguous symbol 'dup'",
        },
        {
            "id": "q-c-loc-ok",
            "track": "C",
            "symbol": "ok",
            "hop_depth": 1,
            "expected_files": ["ok.py"],
            "excluded_reason": None,
        },
    ]
    tasks = generate_tasks(entries)
    assert [t["id"] for t in tasks] == ["task-q-c-loc-ok"]
    assert tasks[0]["hop_depth"] == 1
    exclusions = collect_exclusions(entries)
    assert [e["id"] for e in exclusions] == ["q-a-hop1-dup", "q-c-loc-dup"]
    assert all(e["reason"] for e in exclusions)


def test_track_c_tasks_produce_mcp_gateway_cells_in_runner_gate():
    from bench_v19 import runner as v19_runner

    index = {"sym": ["lib.py"], "dup": ["a.py", "b.py"]}
    tasks = generate_track_c_tasks(index)
    cells = v19_runner.ladder_cells(tasks)
    gateway = [c["task_id"] for c in cells if c["arm"] == "mcp-gateway"]
    # hop_depth 1 + Track C admit the lookup cell to the tax-control arm.
    assert gateway == ["task-c-lookup-sym"]


def test_track_c_exempt_from_f1_gap_rule():
    task = generate_track_c_tasks({"sym": ["lib.py"]})[0]
    # Single answer by construction: divergence only, never the F1 gap.
    assert apply_discriminative_filter(task, 0.9, 0.1, divergent=True)
    assert not apply_discriminative_filter(task, 0.9, 0.1, divergent=False)
    assert len(task["expected_files"]) == 1  # single-answer invariant


def test_track_a_hop_stratification_derives_from_call_edge_bfs(tmp_path):
    # a.py defines sym; b.py calls sym; c.py calls b.middle (defined in b.py).
    from bench_v19 import oracle

    (tmp_path / "a.py").write_text("def sym():\n    return 1\n")
    (tmp_path / "b.py").write_text("def middle():\n    return sym()\n")
    (tmp_path / "c.py").write_text("from b import middle\n\nmiddle()\n")
    entries = oracle.build_callers_oracle(tmp_path, "sym")
    assert all(e["crosscheck_agrees"] for e in entries)
    tasks = generate_tasks(entries)
    hop = {t["hop_depth"]: t["expected_files"] for t in tasks}
    assert hop[1] == ["b.py"]
    assert hop[2] == ["b.py", "c.py"]
    assert hop[3] == ["b.py", "c.py"]


def _fanin_fixture(tmp_path, n_callers: int, call: str = "sym") -> None:
    """Snapshot with one definition and n_callers distinct caller files."""
    (tmp_path / "lib.py").write_text(f"def {call}():\n    return 1\n")
    for i in range(n_callers):
        (tmp_path / f"caller_{i:03d}.py").write_text(
            f"from lib import {call}\n\n{call}()\n"
        )


def test_generate_fanin_tasks_default_hop_is_sealed_hop2():
    sel = [
        {
            "symbol": "sym",
            "hop1_caller_files": 50,
            "hop2_caller_files": 45,
            "hop2_expected_files": ["a.py", "b.py"],
            "hop3_caller_files": 60,
            "hop3_expected_files": ["a.py", "b.py", "c.py"],
            "rg_output_bytes": 9999,
            "defining_files": ["lib.py"],
        }
    ]
    tasks = generate_fanin_tasks(sel)
    assert len(tasks) == 1
    t = tasks[0]
    assert t["tier"] == "fanin" and t["track"] == "A"
    assert t["hop_depth"] == FANIN_SEALED_HOP_DEPTH == 2
    assert t["expected_files"] == ["a.py", "b.py"]
    assert "within 2 hop(s)" in t["prompt"]
    assert "file:line" in t["prompt"]
    # Stage-3 pilot reproduction: hop_depth=3 verbatim.
    p = generate_fanin_tasks(sel, hop_depth=3)[0]
    assert p["hop_depth"] == 3 and p["expected_files"] == [
        "a.py",
        "b.py",
        "c.py",
    ]


def test_sealed_constants_recorded():
    # Pool-bound per A4: the window yields 8 viable symbols on Django.
    assert SEALED_N_TASKS == 8


def test_select_fanin_symbols_gates_on_hop2_set(tmp_path):
    from bench_v19 import oracle

    # Hop-2 set inside the A4a window (60 direct callers, max 60).
    _fanin_fixture(tmp_path, 60)
    selections, exclusions = select_fanin_symbols(
        tmp_path, max_symbols=1, min_hop1=40, min_hop3=40, min_sealed=40,
        min_rg_bytes=0,
    )
    assert len(selections) == 1 and not exclusions
    s = selections[0]
    assert s["hop2_caller_files"] == 60
    assert s["hop3_caller_files"] == 60
    assert set(s["hop2_expected_files"]) == {
        f"caller_{i:03d}.py" for i in range(60)
    }
    tasks = generate_fanin_tasks(selections)
    assert tasks[0]["expected_files"] == s["hop2_expected_files"]
    # Oracle sanity: BFS at sealed depth agrees with the recorded set.
    idx = oracle.build_function_definition_index(tmp_path)
    edges = oracle.build_call_edges(tmp_path, idx)
    direct = oracle.callers_oracle_from(idx, edges, "sym", 2)
    assert set(direct) == set(s["hop2_expected_files"])


def test_select_fanin_symbols_excludes_small_hop2_set(tmp_path):
    # Large hop-1 fan-in but hop-2 set below the sealed window:
    # fail-closed exclusion with a recorded reason.
    _fanin_fixture(tmp_path, 50)
    selections, exclusions = select_fanin_symbols(
        tmp_path, max_symbols=1, min_hop1=40, min_hop3=40, min_sealed=55,
        min_rg_bytes=0,
    )
    assert not selections
    assert len(exclusions) == 1
    assert "outside the" in exclusions[0]["reason"]
    assert "sealed window [55, 60]" in exclusions[0]["reason"]


def test_select_fanin_symbols_excludes_oversized_hop2_set(tmp_path):
    # A4a upper bound: 100 caller files is output-bounded (the re-pilot's
    # 78-848-file tasks never completed) -> excluded.
    _fanin_fixture(tmp_path, 100)
    selections, exclusions = select_fanin_symbols(
        tmp_path, max_symbols=1, min_hop1=40, min_hop3=40, min_rg_bytes=0,
    )
    assert not selections
    assert len(exclusions) == 1
    assert "outside the" in exclusions[0]["reason"]
    assert "[20, 60]" in exclusions[0]["reason"]


def test_select_fanin_symbols_excludes_subfloor_rg_output(tmp_path):
    # In-window hop-2 set but tiny raw rg output: not context-flooding.
    _fanin_fixture(tmp_path, 30)
    selections, exclusions = select_fanin_symbols(
        tmp_path, max_symbols=1, min_hop1=20, min_hop3=20, min_rg_bytes=10**9,
    )
    assert not selections
    assert len(exclusions) == 1
    assert "below the A4a" in exclusions[0]["reason"]


def test_activation_gate_pending_below_min_sample():
    # A4b: 3/4 = 0.75 must NOT halt the gate (sample too small to judge);
    # this is exactly the re-pilot halt that motivated the amendment.
    sessions = [
        {"killed": False, "aptu_tool_calls": 1},
        {"killed": False, "aptu_tool_calls": 1},
        {"killed": False, "aptu_tool_calls": 1},
        {"killed": False, "aptu_tool_calls": 0},
    ]
    gate = activation_gate(sessions)
    assert gate["pass"] is True and gate["pending"] is True
    assert gate["fraction"] is None and gate["scorable"] == 4
    # At full sample the same active fraction fails, as designed.
    full = sessions * 2 + [{"killed": False, "aptu_tool_calls": 0}]
    gate = activation_gate(full)
    assert gate["pass"] is False and "pending" not in gate
    # Killed sessions are excluded from the sample, not counted against.
    killed = sessions + [{"killed": True, "aptu_tool_calls": 0}] * 10
    gate = activation_gate(killed)
    assert gate["pending"] is True  # still only 4 scorable


def test_activation_gate_passes_at_threshold_with_full_sample():
    sessions = [{"killed": False, "aptu_tool_calls": 1}] * 8
    gate = activation_gate(sessions)
    assert gate["pass"] is True and gate["fraction"] == 1.0
    # 7/9 active = 0.778 < 0.8 -> fail at full sample.
    gate = activation_gate(
        [{"killed": False, "aptu_tool_calls": 1}] * 7
        + [{"killed": False, "aptu_tool_calls": 0}] * 2
    )
    assert gate["pass"] is False and gate["fraction"] == 7 / 9
