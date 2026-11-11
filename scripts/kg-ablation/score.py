#!/usr/bin/env python3
"""Aggregate adjudication sheets into fixture-by-arm tables.

Emits counts of catch / false positive / false negative / false
reassurance per (fixture, arm) cell plus the pre-registered decision-rule
inputs. Verdict application (SUPPORTED / NOT-SUPPORTED) is done by the
report author per the design's decision rule; this tool supplies the
tables and the unique-catch comparison.
"""

from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path

OUTCOMES = ("catch", "false_positive", "false_negative",
            "false_reassurance")


def tables(sheets_dir: Path) -> dict:
    counts: dict[tuple[str, str], dict[str, int]] = defaultdict(
        lambda: {k: 0 for k in OUTCOMES})
    for sheet_path in sorted(sheets_dir.glob("*.json")):
        sheet = json.loads(sheet_path.read_text(encoding="utf-8"))
        cell = counts[(sheet["fixture"], sheet["arm"])]
        for outcome in OUTCOMES:
            if sheet.get(outcome):
                cell[outcome] += 1
    table = {
        f"{fixture}|{arm}": cell
        for (fixture, arm), cell in sorted(counts.items())
    }

    catch_by_arm: dict[str, set[str]] = defaultdict(set)
    for (fixture, arm), cell in counts.items():
        if cell["catch"] > 0:
            catch_by_arm[arm].add(fixture)

    unique = {
        arm: sorted(catch_by_arm[arm] - catch_by_arm["no-graph"])
        for arm in ("optional-graph", "forced-graph")
    }
    return {
        "fixture_by_arm": table,
        "catches_by_arm": {k: sorted(v) for k, v in catch_by_arm.items()},
        "unique_catches_vs_no_graph": unique,
        "false_reassurance_total": sum(
            c["false_reassurance"] for c in counts.values()),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--sheets-dir", type=Path, required=True)
    args = ap.parse_args()
    print(json.dumps(tables(args.sheets_dir), indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
