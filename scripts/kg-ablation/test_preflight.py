"""Unit tests for the v20 preflight gate and the runner lock.

Run: python3 -m unittest discover -s scripts/kg-ablation
"""

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import preflight  # noqa: E402
import runner  # noqa: E402


class TestSingleInstanceLock(unittest.TestCase):
    """Second runner instance on the same lockfile must exit nonzero."""

    def test_second_lock_acquisition_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            first = runner.acquire_single_instance_lock(root)
            try:
                with self.assertRaises(SystemExit) as ctx:
                    runner.acquire_single_instance_lock(root)
                self.assertEqual(ctx.exception.code, 2)
            finally:
                first.close()

    def test_lock_released_after_close(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            first = runner.acquire_single_instance_lock(root)
            first.close()
            second = runner.acquire_single_instance_lock(root)
            second.close()


class TestValidateGroundTruth(unittest.TestCase):
    """Malformed ground-truth.json entries are rejected."""

    def test_rejects_missing_pack_and_defect_symbols(self):
        gt = {"description": "incomplete entry"}
        errors = preflight.validate_ground_truth(gt)
        self.assertTrue(any("pack" in e for e in errors))
        self.assertTrue(any("defect_symbols" in e for e in errors))

    def test_accepts_minimal_valid_entry(self):
        gt = {"pack": "clean-control",
              "description": "no defect",
              "defect_symbols": []}
        self.assertEqual(preflight.validate_ground_truth(gt), [])


class TestGrepHostilityScore(unittest.TestCase):
    """Distinctive-literal reachability is scored as hostile-fail."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        (Path(self.tmp) / "src").mkdir()
        (Path(self.tmp) / "src" / "lib.rs").write_text(
            "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n",
            encoding="utf-8")

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_unique_literal_scores_hostile_fail(self):
        # "beta" appears exactly once: a distinctive-literal rg query
        # localizes the target, so the pack scores 1.0 (hostile-fail).
        score = preflight.grep_hostility_score(Path(self.tmp), ["beta"])
        self.assertEqual(score, 1.0)

    def test_empty_queries_clean_control_passes(self):
        score = preflight.grep_hostility_score(Path(self.tmp), [])
        self.assertEqual(score, 0.0)


@unittest.skipUnless(shutil.which("cargo"), "cargo not on PATH")
class TestFixturePreflight(unittest.TestCase):
    """All four packs pass cargo check and cargo test."""

    def test_all_packs_preflight_clean(self):
        fixtures_dir = Path(__file__).resolve().parent / "fixtures"
        results = preflight.preflight_all(fixtures_dir)
        self.assertEqual(len(results), 4)
        for result in results:
            self.assertTrue(result["ok"],
                            f"{result['pack']}: {result['errors']}")


if __name__ == "__main__":
    unittest.main()
