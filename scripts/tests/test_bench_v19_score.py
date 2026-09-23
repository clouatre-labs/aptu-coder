"""Offline tests for the bench_v19 F1 scorer verdicts."""

from __future__ import annotations

import pytest
from bench_v19.score import expected_digest, f1_score, score_answer


@pytest.mark.parametrize(
    ("answer", "expected", "fabricated", "verdict"),
    [
        # recall in (0, 0.9) -> partial
        ({"a.py", "b.py", "c.py", "d.py", "e.py"}, {"a.py", "z.py"}, 0, "partial"),
        # recall >= 0.9, zero fabricated anchors -> correct
        ({"a.py", "b.py", "c.py", "d.py", "e.py"}, {"a.py"}, 0, "correct"),
        # any fabricated anchor -> fabricated-anchor regardless of recall
        ({"a.py"}, {"a.py", "b.py", "c.py"}, 1, "fabricated-anchor"),
        # empty answer set -> defined no-anchor verdict, not an exception
        (set(), {"a.py"}, 0, "no-anchor"),
        (set(), set(), 0, "no-anchor"),
    ],
)
def test_score_answer_verdicts(answer, expected, fabricated, verdict):
    result = score_answer(answer, expected, fabricated_anchors=fabricated)
    assert result["verdict"] == verdict


def test_score_answer_fabricated_overrides_correct_recall():
    result = score_answer({"a.py"}, {"a.py"}, fabricated_anchors=1)
    assert result["verdict"] == "fabricated-anchor"


def test_f1_metrics_exact_set_match():
    metrics = f1_score({"a.py", "b.py"}, {"a.py", "b.py"})
    assert metrics == {"precision": 1.0, "recall": 1.0, "f1": 1.0}


def test_partial_recall_in_open_interval():
    metrics = score_answer(
        {"a.py", "b.py", "c.py", "d.py", "e.py"},
        {"a.py", "z.py"},
        0,
    )
    assert 0.0 < metrics["recall"] < 0.9
    assert metrics["verdict"] == "partial"


def test_expected_digest_reused_from_v18():
    assert expected_digest("x") == expected_digest("x")
    assert expected_digest("x") != expected_digest("y")
