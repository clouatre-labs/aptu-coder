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


# ---- A6a: deterministic anchor verification (snapshot-based) ----

from bench_v19.score import ANCHOR_WINDOW_LINES, verify_anchors  # noqa: E402


def _make_snapshot(tmp_path, files: dict[str, str]):
    for rel, content in files.items():
        p = tmp_path / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content, encoding="utf-8")
    return tmp_path


def test_verify_anchors_real_call_site(tmp_path):
    snap = _make_snapshot(tmp_path, {
        "a/b.py": "def foo():\n    pass\n\nx = bar(1)\n",
    })
    verified, fabricated = verify_anchors(
        "see a/b.py:4", "bar", snap
    )
    assert (verified, fabricated) == (1, 0)


def test_verify_anchors_line_out_of_range(tmp_path):
    snap = _make_snapshot(tmp_path, {"a/b.py": "bar()\n"})
    verified, fabricated = verify_anchors("see a/b.py:99", "bar", snap)
    assert (verified, fabricated) == (0, 1)


def test_verify_anchors_missing_file(tmp_path):
    verified, fabricated = verify_anchors("see no/such.py:1", "bar", tmp_path)
    assert (verified, fabricated) == (0, 1)


def test_verify_anchors_symbol_not_near_line(tmp_path):
    snap = _make_snapshot(tmp_path, {"a/b.py": "unrelated()\n"})
    verified, fabricated = verify_anchors("see a/b.py:1", "bar", snap)
    assert (verified, fabricated) == (0, 1)


def test_verify_anchors_symbol_within_window(tmp_path):
    # Call spans lines: identifier on the line after the cited line.
    body = "result = (\n    bar(1, 2)\n)\n"
    snap = _make_snapshot(tmp_path, {"a/b.py": body})
    verified, fabricated = verify_anchors("see a/b.py:1", "bar", snap)
    assert verified == 1 and fabricated == 0
    assert ANCHOR_WINDOW_LINES >= 1


def test_verify_anchors_distinct_and_dedup(tmp_path):
    snap = _make_snapshot(tmp_path, {
        "a/b.py": "bar()\nx\ny\nz\nw\n",
    })
    text = "a/b.py:1 and a/b.py:1 and a/b.py:5"
    verified, fabricated = verify_anchors(text, "bar", snap)
    # :5 is outside the +/-2 window of the bar() call -> fabricated;
    # the duplicated :1 anchor is deduplicated to one verified hit.
    assert (verified, fabricated) == (1, 1)


def test_verify_anchors_symbol_substring_of_longer_identifier(tmp_path):
    # A9a edge case: the symbol occurs within the window only as a
    # substring of a longer identifier; a word-boundary match is
    # required, so the anchor is fabricated.
    snap = _make_snapshot(tmp_path, {"a/b.py": "bar_catalog(1)\n"})
    verified, fabricated = verify_anchors("see a/b.py:1", "bar", snap)
    assert (verified, fabricated) == (0, 1)


def test_verify_anchors_no_anchors_yields_zeroes(tmp_path):
    verified, fabricated = verify_anchors("no anchors here", "bar", tmp_path)
    assert (verified, fabricated) == (0, 0)
