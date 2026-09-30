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

import re

VERDICTS = ("correct", "partial", "fabricated-anchor", "no-anchor")
CORRECT_RECALL_THRESHOLD = 0.9

# A6a: file:line anchors cited in a final answer. Verification is against
# the snapshot, never against the gold set.
ANCHOR_RE = re.compile(r"([\w./-]+/[\w./-]+):(\d+)")
# A call may span lines; the identifier may sit up to 2 lines away from
# the cited line (before or after).
ANCHOR_WINDOW_LINES = 2


def verify_anchors(text: str, symbol: str, snapshot_root) -> tuple[int, int]:
    """Deterministic anchor verification against the snapshot (A6a).

    An anchor ``path:line`` is fabricated iff the file does not exist in
    the snapshot, the line number is out of range, or the symbol
    identifier does not occur as a distinct identifier (word-boundary
    match, A9a) within ``ANCHOR_WINDOW_LINES`` of the cited line. A raw
    substring hit is not sufficient: a mention of the symbol inside a
    comment or as a substring of a longer identifier does not verify
    the anchor. Gold-set membership is never consulted here: a
    predicted file absent from the gold set is an ordinary false
    positive and affects only precision/F1, never the fabricated count.

    Returns ``(verified_count, fabricated_count)`` over distinct
    anchors. An answer that cites no file:line anchors at all yields
    ``(0, 0)``; verdicts then rest on recall alone.
    """
    verified = 0
    fabricated = 0
    seen: set[tuple[str, int]] = set()
    for match in ANCHOR_RE.finditer(text):
        path = match.group(1)
        lineno = int(match.group(2))
        key = (path, lineno)
        if key in seen:
            continue
        seen.add(key)
        file_path = snapshot_root / path
        try:
            lines = file_path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            fabricated += 1
            continue
        if not 1 <= lineno <= len(lines):
            fabricated += 1
            continue
        lo = max(0, lineno - 1 - ANCHOR_WINDOW_LINES)
        hi = lineno + ANCHOR_WINDOW_LINES
        # A9a: word-boundary identifier match, not a raw substring
        # check; substrings of longer identifiers or comment mentions
        # that do not form a distinct identifier do not verify.
        if re.search(rf"\b{re.escape(symbol)}\b", "\n".join(lines[lo:hi])) is None:
            fabricated += 1
            continue
        verified += 1
    return verified, fabricated


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
