#!/usr/bin/env python3
"""Deterministic categorical scorer for the v18 benchmark.

Maps (transcript, answer) pairs to one of: correct, incorrect, omitted,
fabricated-anchor, no-target-read. Fabricated anchors are resolved against
the fixture filesystem.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

CATEGORIES = (
    "correct", "incorrect", "omitted", "fabricated-anchor", "no-target-read",
)

CITE_RE = re.compile(
    r"([A-Za-z0-9_\-./]+\.(?:py|rs|ts)):(\d+)"
)
ANSWER_RE = re.compile(r"V18ANS-\d{4}:[A-Za-z]+-\d{4}")


def _fixture_paths(fixture_root: Path) -> set[str]:
    return {
        p.relative_to(fixture_root).as_posix()
        for p in fixture_root.rglob("*") if p.is_file()
    }


def _line_count(path: Path) -> int:
    return len(path.read_text(encoding="utf-8").splitlines())


def read_target_used(transcript: str, fixture_root: Path) -> bool:
    """True when the transcript shows a successful read of a fixture path."""
    for rel in _fixture_paths(fixture_root):
        if rel in transcript and ("read" in transcript or "analyze" in transcript):
            return True
    return False


def categorize(
    answer: str | None, transcript: str, expected: str, fixture_root: Path
) -> str:
    """Score one question. Deterministic and unit-tested."""
    if answer is None or not answer.strip():
        return "omitted"
    paths = _fixture_paths(fixture_root)
    for match in CITE_RE.finditer(answer):
        rel, lineno = match.group(1).lstrip("./"), int(match.group(2))
        if rel not in paths:
            return "fabricated-anchor"
        if lineno < 1 or lineno > _line_count(fixture_root / rel):
            return "fabricated-anchor"
    if not read_target_used(transcript, fixture_root):
        return "no-target-read"
    found = ANSWER_RE.findall(answer)
    if expected in found:
        return "correct"
    return "incorrect"


def score_session(
    question: dict, answer: str | None, transcript: str, fixture_root: Path
) -> dict:
    category = categorize(
        answer, transcript, question["expected"], fixture_root
    )
    return {
        "task_id": question["id"],
        "category": category,
        "expected_sha": expected_digest(question["expected"]),
    }


def expected_digest(expected: str) -> str:
    import hashlib

    return hashlib.sha256(expected.encode()).hexdigest()[:16]


def write_scores(path: Path, scored: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(scored, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
