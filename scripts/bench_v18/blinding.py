#!/usr/bin/env python3
"""Blinding for the v18 benchmark: opaque run IDs and a sealed label map.

scores.json is emitted with no arm/model/run-label fields; the label map is
sealed at run time and joined only during analysis.
"""

from __future__ import annotations

import json
import uuid
from pathlib import Path

FORBIDDEN_KEYS = ("arm", "model", "run_id", "label", "labels", "run-label")


def new_run_id() -> str:
    """Opaque randomized run ID (uuid4); never derived from task/model."""
    return uuid.uuid4().hex


def seal_label_map(
    run_root: Path, run_id: str, arm_assignments: dict[str, str]
) -> Path:
    """Write the sealed label-map.json (arm per opaque run label)."""
    run_root.mkdir(parents=True, exist_ok=True)
    path = run_root / "label-map.json"
    path.write_text(
        json.dumps({"run_id": run_id, "arms": arm_assignments}, indent=2)
        + "\n",
        encoding="utf-8",
    )
    return path


def sanitize_record(record: dict) -> dict:
    """Strip any arm/model/run-label fields from a per-task score record."""
    out = {}
    for key, value in record.items():
        if key.lower() in FORBIDDEN_KEYS or "arm" in key.lower() \
                or "model" in key.lower():
            continue
        out[key] = value
    return out


def write_scores(run_root: Path, scores: list[dict]) -> Path:
    """Emit scores.json guaranteed free of arm/model/label fields."""
    sanitized = [sanitize_record(s) for s in scores]
    blob = json.dumps(sanitized, indent=2, sort_keys=True).lower()
    for word in FORBIDDEN_KEYS:
        if f'"{word}"' in blob:
            raise ValueError(f"blinding leak: forbidden key '{word}' in scores")
    path = run_root / "scores.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(sanitized, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return path
