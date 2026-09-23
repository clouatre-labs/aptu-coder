"""Offline tests for the bench_v19 task generator and discriminative filter."""

from __future__ import annotations

from bench_v19.generate_tasks import (
    F1_GAP_THRESHOLD,
    apply_discriminative_filter,
    filter_pilot_tasks,
    generate_tasks,
    generate_track_c_tasks,
)

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
        "hop_depth": None,
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
    index = {"sym": ["lib.py"], "other": ["pkg/mod.rs"], "ghost": []}
    tasks = generate_track_c_tasks(index)
    assert [t["id"] for t in tasks] == ["task-c-lookup-other", "task-c-lookup-sym"]
    by_id = {t["id"]: t for t in tasks}
    assert by_id["task-c-lookup-sym"]["expected_files"] == ["lib.py"]
    assert by_id["task-c-lookup-sym"]["track"] == "C"
    assert by_id["task-c-lookup-sym"]["hop_depth"] is None
    # Empty definition lists emit no task (nothing to look up).
    assert "task-c-lookup-ghost" not in by_id


def test_track_c_exempt_from_f1_gap_rule():
    task = generate_track_c_tasks({"sym": ["lib.py"]})[0]
    # Single answer by construction: divergence only, never the F1 gap.
    assert apply_discriminative_filter(task, 0.9, 0.1, divergent=True)
    assert not apply_discriminative_filter(task, 0.9, 0.1, divergent=False)


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
