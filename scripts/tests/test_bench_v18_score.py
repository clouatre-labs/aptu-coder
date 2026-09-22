"""Scorer tests: fabricated-anchor categorization against a real fixture."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from bench_v18 import generate_fixture, score

SEED = 1604


def _fixture(tmp_path):
    root = tmp_path / "fixture"
    generate_fixture.generate(SEED, root)
    return root


def test_fabricated_anchor_categorized(tmp_path):
    fx = _fixture(tmp_path)
    transcript = "read fixture/zephyr_index_0000.py and analyzed it"
    answer = "the value is at docs/fake_file.py:42"
    got = score.categorize(answer, transcript, "expected", fx)
    assert got == "fabricated-anchor"


def test_line_out_of_range_is_fabricated(tmp_path):
    fx = _fixture(tmp_path)
    # A real fixture path but an impossible line number.
    rel = next(p for p in fx.rglob("*.py")).relative_to(fx).as_posix()
    answer = f"see {rel}:999999"
    transcript = f"read fixture/{rel} and analyzed it"
    assert score.categorize(answer, transcript, "expected", fx) == \
        "fabricated-anchor"


def test_correct_incorrect_omitted_no_target_read(tmp_path):
    fx = _fixture(tmp_path)
    import json
    manifest = json.loads((fx / "manifest.json").read_text(encoding="utf-8"))
    expected = manifest["answers"][0]["answer"]
    src_rel = manifest["answers"][0]["path"]
    good_tx = f"read fixture/{src_rel} and analyzed; found {expected}"
    assert score.categorize(expected, good_tx, expected, fx) == "correct"
    assert score.categorize("V18ANS-0000:wrong-0000", good_tx, expected, fx) \
        == "incorrect"
    assert score.categorize(None, good_tx, expected, fx) == "omitted"
    # Transcript with no fixture reads -> no-target-read even if answer ok.
    assert score.categorize(expected, "no reads here", expected, fx) == \
        "no-target-read"
