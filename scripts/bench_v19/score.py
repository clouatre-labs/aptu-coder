"""Standalone F1 set-recall scorer for the v19 benchmark.

Deliberately imports ONLY expected_digest and write_scores from
bench_v18.score; categorize/score_session/ANSWER_RE are v18-answer-format
coupled and must never leak into v19 scoring. Verdicts: correct (recall
>= 0.9 and zero fabricated anchors), partial (recall in (0, 0.9)),
fabricated-anchor (any fabricated anchor), no-anchor (empty answer set,
a defined verdict rather than an exception).
"""

from __future__ import annotations

from bench_v18.score import expected_digest, write_scores  # noqa: F401

VERDICTS = ("correct", "partial", "fabricated-anchor", "no-anchor")
CORRECT_RECALL_THRESHOLD = 0.9


def f1_score(answer_set: set[str], expected_set: set[str]) -> dict:
    """Precision/recall/F1 over sets of entities (file paths or symbols)."""
    if not answer_set or not expected_set:
        return {"precision": 0.0, "recall": 0.0, "f1": 0.0}
    overlap = len(answer_set & expected_set)
    precision = overlap / len(answer_set)
    recall = overlap / len(expected_set)
    f1 = 2 * precision * recall / (precision + recall) if precision + recall else 0.0
    return {"precision": precision, "recall": recall, "f1": f1}


def score_answer(
    answer_set: set[str],
    expected_set: set[str],
    fabricated_anchors: int,
) -> dict:
    """Verdict plus F1 metrics for one Track A answer.

    A defined verdict is always returned; an empty answer set yields
    "no-anchor", never an exception.
    """
    metrics = f1_score(answer_set, expected_set)
    if not answer_set:
        verdict = "no-anchor"
    elif fabricated_anchors > 0:
        verdict = "fabricated-anchor"
    elif metrics["recall"] >= CORRECT_RECALL_THRESHOLD:
        verdict = "correct"
    else:
        verdict = "partial"
    return {"verdict": verdict, **metrics}


def score_task(task: dict, answer_set: set[str], fabricated_anchors: int) -> dict:
    """Score one task against its oracle; result carries the expected digest."""
    result = score_answer(answer_set, set(task["expected_files"]), fabricated_anchors)
    result["task_id"] = task["id"]
    result["expected_sha"] = expected_digest("\n".join(sorted(task["expected_files"])))
    return result
