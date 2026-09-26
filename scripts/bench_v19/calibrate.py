"""Calibration harness for the v19 anchor-adjudication protocol (#1686).

Two phases, zero judge/API calls in this module:

--emit-requests  read a labels JSON file (a list of
                 {"anchor_record": {...}, "human_label": "<role>"}) and
                 write the adjudication request payloads plus the model
                 pin to an output file, for the maintainer to run
                 through the MCP judge tool offline.

--score          given the labels file and a results file (judge answers
                 in the same order), compute per-role and overall
                 agreement against the human labels and evaluate the
                 freeze gate.

Freeze gate (from #1686, constants here): overall agreement >= 0.9 and
no role below 0.75. Calibration itself does NOT run in CI.
"""

from __future__ import annotations

import argparse
import json

from bench_v19.adjudicate import (
    ANCHOR_ROLE_CRITERIA,
    JEV_MODEL,
    build_adjudication_request,
)

# Freeze gate: agreed in #1686; do not tune without a manifest amendment.
FREEZE_OVERALL_MIN = 0.9
FREEZE_PER_ROLE_MIN = 0.75


def emit_requests(labels: list, out_path: str) -> int:
    """Write adjudication requests for each labeled anchor to out_path."""
    payload = {
        "model": JEV_MODEL,
        "requests": [build_adjudication_request(item["anchor_record"]) for item in labels],
    }
    with open(out_path, "w", encoding="utf-8") as fh:
        json.dump(payload, fh, indent=2)
        fh.write("\n")
    return len(payload["requests"])


def compute_agreement(labels: list, judge_answers: list) -> dict:
    """Per-role and overall agreement of judge answers vs human labels.

    `judge_answers` is a list of {"anchor_role": "<role>"} dicts aligned
    index-wise with `labels`. Roles with zero labeled cases are reported
    with agreement 0.0 and count 0; the freeze gate (see freeze_gate)
    only evaluates roles that have at least one labeled case.
    """
    if len(labels) != len(judge_answers):
        raise ValueError(
            "labels ({}) and judge_answers ({}) length mismatch".format(
                len(labels), len(judge_answers)
            )
        )
    per_role = {role: {"agree": 0, "total": 0} for role in ANCHOR_ROLE_CRITERIA}
    agree_total = 0
    for item, answer in zip(labels, judge_answers):
        role = item["human_label"]
        counts = per_role[role]
        counts["total"] += 1
        if answer["anchor_role"] == role:
            counts["agree"] += 1
            agree_total += 1
    overall = agree_total / len(labels) if labels else 0.0
    role_agreement = {
        role: counts["agree"] / counts["total"] if counts["total"] else 0.0
        for role, counts in per_role.items()
    }
    return {
        "overall_agreement": overall,
        "per_role_agreement": role_agreement,
        "per_role_counts": {role: c["total"] for role, c in per_role.items()},
    }


def freeze_gate(agreement: dict) -> bool:
    """True iff overall >= 0.9 and every covered role >= 0.75."""
    if agreement["overall_agreement"] < FREEZE_OVERALL_MIN:
        return False
    return all(
        value >= FREEZE_PER_ROLE_MIN
        for role, value in agreement["per_role_agreement"].items()
        if agreement["per_role_counts"][role] > 0
    )


def score_results(labels: list, judge_answers: list) -> dict:
    """Agreement report plus the freeze-gate verdict."""
    report = compute_agreement(labels, judge_answers)
    report["freeze_gate_passed"] = freeze_gate(report)
    return report


def main() -> None:
    parser = argparse.ArgumentParser(
        description="v19 anchor-adjudication calibration harness (#1686)"
    )
    parser.add_argument(
        "labels", help="labels JSON file: list of {anchor_record, human_label}"
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument(
        "--emit-requests",
        metavar="OUT",
        help="write judge request payloads to OUT (run through the MCP judge tool manually)",
    )
    mode.add_argument(
        "--score",
        metavar="RESULTS",
        help="score a results file of judge answers (same order as labels) against labels",
    )
    args = parser.parse_args()

    with open(args.labels, "r", encoding="utf-8") as fh:
        labels = json.load(fh)

    if args.emit_requests is not None:
        count = emit_requests(labels, args.emit_requests)
        print("wrote {} requests (model {}) to {}".format(count, JEV_MODEL, args.emit_requests))
    else:
        with open(args.score, "r", encoding="utf-8") as fh:
            judge_answers = json.load(fh)
        report = score_results(labels, judge_answers)
        print(json.dumps(report, indent=2, sort_keys=True))
        if not report["freeze_gate_passed"]:
            raise SystemExit(1)


if __name__ == "__main__":
    main()
