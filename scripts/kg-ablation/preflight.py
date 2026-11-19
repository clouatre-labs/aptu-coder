#!/usr/bin/env python3
"""Preflight gate for the v20 ablation fixture packs.

Run before any session: for each pack under fixtures/ this verifies
- compile-invisibility: `cargo check` and `cargo test` pass (defective
  packs included),
- ground-truth.json schema validity,
- DEFECT_SYMBOLS coverage: config.py carries every pack's named-defect
  symbols,
- grep-hostility: distinctive-literal defect-locating queries must fail
  to localize the defect (rg-based, with a stdlib fallback for hosts
  without ripgrep).

Fail-closed: missing cargo or any failed check exits nonzero.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

from config import DEFECT_SYMBOLS, FIXTURES_DIR

GREP_HOSTILITY_MAX_RATE = 0.0


def validate_ground_truth(gt: dict) -> list[str]:
    """Schema check for a ground-truth.json document; empty = valid."""
    errors = []
    if not isinstance(gt, dict):
        return ["ground truth must be a JSON object"]
    if not gt.get("pack"):
        errors.append("missing required field: pack")
    if not gt.get("description"):
        errors.append("missing required field: description")
    if "defect_symbols" not in gt:
        errors.append("missing required field: defect_symbols")
    elif not isinstance(gt["defect_symbols"], list) or \
            not all(isinstance(s, str) for s in gt["defect_symbols"]):
        errors.append("defect_symbols must be a list of strings")
    refs = gt.get("references")
    if refs is not None:
        for i, ref in enumerate(refs):
            if not isinstance(ref, dict) or \
                    not ref.get("caller") or not ref.get("callee"):
                errors.append(
                    f"references[{i}] needs caller and callee strings")
    targets = gt.get("pattern_targets")
    if targets is not None:
        for i, t in enumerate(targets):
            if not isinstance(t, dict) or not t.get("method") \
                    or not t.get("callsite"):
                errors.append(
                    f"pattern_targets[{i}] needs method and callsite")
    return errors


def grep_hostility_score(pack_dir: Path, queries: list[str]) -> float:
    """Fraction of queries that uniquely localize (exactly one hit).

    0.0 = hostile (no query localizes anything); 1.0 = every query
    pins the defect with a single text hit. Uses ripgrep when present,
    falling back to a line scan so the scorer stays testable anywhere.
    """
    if not queries:
        return 0.0
    localized = 0
    for query in queries:
        hits = _query_hits(pack_dir, query)
        if len(hits) == 1:
            localized += 1
    return localized / len(queries)


def _query_hits(pack_dir: Path, query: str) -> list[str]:
    if shutil.which("rg"):
        proc = subprocess.run(
            ["rg", "--no-heading", "-F", query, "."],
            cwd=str(pack_dir), capture_output=True, text=True)
        return [line for line in proc.stdout.splitlines() if line.strip()]
    hits = []
    for path in sorted(pack_dir.rglob("*")):
        if not path.is_file() or path.name == "ground-truth.json":
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for line in text.splitlines():
            if query in line:
                hits.append(f"{path}: {line.strip()}")
    return hits


def run_cargo_checks(pack_dir: Path) -> list[str]:
    """cargo check + cargo test on a pack; empty = compile-invisible."""
    errors = []
    for cmd in (["cargo", "check"], ["cargo", "test"]):
        proc = subprocess.run(cmd, cwd=str(pack_dir),
                              capture_output=True, text=True)
        if proc.returncode != 0:
            errors.append(
                f"{cmd[-1]} failed in {pack_dir.name}: "
                f"{proc.stdout.strip()[-400:] or proc.stderr.strip()[-400:]}")
    return errors


def check_pack(pack_dir: Path) -> dict:
    """All preflight checks for one pack; result['ok'] is the verdict."""
    result: dict = {"pack": pack_dir.name, "ok": True, "errors": [],
                    "grep_hostility_rate": None}
    gt_path = pack_dir / "ground-truth.json"
    if not gt_path.is_file():
        result["ok"] = False
        result["errors"].append("missing ground-truth.json")
        return result
    try:
        gt = json.loads(gt_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        result["ok"] = False
        result["errors"].append(f"ground-truth.json not valid JSON: {exc}")
        return result
    errors = validate_ground_truth(gt)
    if errors:
        result["ok"] = False
        result["errors"].extend(errors)
        return result
    symbols = gt.get("defect_symbols", [])
    if symbols != DEFECT_SYMBOLS.get(pack_dir.name):
        result["ok"] = False
        result["errors"].append(
            f"DEFECT_SYMBOLS mismatch: ground truth {symbols} vs config "
            f"{DEFECT_SYMBOLS.get(pack_dir.name)}")
    errors = run_cargo_checks(pack_dir)
    if errors:
        result["ok"] = False
        result["errors"].extend(errors)
    rate = grep_hostility_score(pack_dir, symbols)
    result["grep_hostility_rate"] = round(rate, 3)
    if rate > GREP_HOSTILITY_MAX_RATE:
        result["ok"] = False
        result["errors"].append(
            f"grep-hostility fail: rate {rate:.3f} > "
            f"{GREP_HOSTILITY_MAX_RATE:.3f} (a distinctive-literal query "
            "localizes the defect)")
    return result


def preflight_all(fixtures_dir: Path) -> list[dict]:
    packs = sorted(p for p in fixtures_dir.iterdir() if p.is_dir())
    return [check_pack(p) for p in packs]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--fixtures-dir", type=Path, default=FIXTURES_DIR)
    args = ap.parse_args()
    if not shutil.which("cargo"):
        print(json.dumps({"fatal": "cargo not found on PATH"}), flush=True)
        return 2
    results = preflight_all(args.fixtures_dir)
    ok = all(r["ok"] for r in results) and len(results) >= 4
    print(json.dumps({"ok": ok, "packs": results}, indent=2))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
