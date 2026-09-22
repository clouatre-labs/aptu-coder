"""Blinding tests: scores.json must carry no arm/model/run-label fields."""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from bench_v18 import blinding


def test_scores_json_has_no_arm_model_keys(tmp_path):
    scores = [
        {
            "task_id": "q-0001",
            "category": "correct",
            "arm": "native",
            "model": "glm-5.3-flash",
            "label": "left",
        },
    ]
    out = blinding.write_scores(tmp_path, scores)
    blob = json.loads(out.read_text(encoding="utf-8"))
    for rec in blob:
        for forbidden in ("arm", "model", "label", "run_id", "run-label"):
            assert forbidden not in {k.lower() for k in rec}
    assert blob[0]["category"] == "correct"


def test_sealed_label_map_written_at_run_time(tmp_path):
    run_id = blinding.new_run_id()
    path = blinding.seal_label_map(
        tmp_path, run_id, {"q-0001": "native", "q-0002": "mcp"}
    )
    data = json.loads(path.read_text(encoding="utf-8"))
    assert data["run_id"] == run_id
    assert set(data["arms"].values()) == {"native", "mcp"}


def test_run_ids_uuid_based_not_derived():
    a = blinding.new_run_id()
    b = blinding.new_run_id()
    assert a != b
    assert a == a.lower() and len(a) == 32


def test_leaky_scores_are_sanitized(tmp_path):
    sanitized = blinding.sanitize_record(
        {"task_id": "q", "run_id": "leak", "arm": "native",
         "model": "glm", "category": "correct"}
    )
    assert sanitized == {"task_id": "q", "category": "correct"}
    out = blinding.write_scores(
        tmp_path, [{"task_id": "q", "run_id": "leak", "arm": "native"}]
    )
    rec = json.loads(out.read_text(encoding="utf-8"))[0]
    assert set(rec) == {"task_id"}
