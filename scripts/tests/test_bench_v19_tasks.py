"""Offline tests for the bench_v19 task generator and discriminative filter."""

from __future__ import annotations

from bench_v19.generate_tasks import (
    F1_GAP_THRESHOLD,
    apply_discriminative_filter,
    filter_pilot_tasks,
    generate_tasks,
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
