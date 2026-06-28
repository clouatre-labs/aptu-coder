#!/usr/bin/env python3
"""
session-stats.py -- Goose-coder handoff analytics for aptu-coder.

Scans .worktrees/*/.handoff/ directories, loads plan/build/validation JSON
files, and produces summary statistics: verdict distribution, retry rate,
PR success rate, complexity breakdown, and trends over time.

Zero external dependencies -- stdlib only.

Usage:
  python scripts/session-stats.py --worktrees-dir .worktrees          # text
  python scripts/session-stats.py --worktrees-dir .worktrees --json   # JSON
  python scripts/session-stats.py --worktrees-dir .worktrees --csv    # CSV
"""

import argparse
import csv
import glob
import json
import os
import re
import sys
from collections import Counter, defaultdict
from io import StringIO


def load_sessions(worktrees_dir):
    """Discover sessions via glob for .worktrees/*/.handoff/ and load JSON."""
    pattern = os.path.join(worktrees_dir, "*", ".handoff")
    handoff_dirs = sorted(glob.glob(pattern))
    sessions = []

    for hdir in handoff_dirs:
        worktree_dir = os.path.dirname(hdir)
        base = os.path.basename(worktree_dir)
        # Skip non-session worktrees like -a, -b
        if base.startswith("-"):
            continue

        plan = {}
        build = {}
        validation = {}

        # Load 02-plan.json
        plan_path = os.path.join(hdir, "02-plan.json")
        try:
            with open(plan_path, "r") as f:
                plan = json.load(f)
        except (FileNotFoundError, json.JSONDecodeError):
            pass

        # Load 03-build.json
        build_path = os.path.join(hdir, "03-build.json")
        try:
            with open(build_path, "r") as f:
                build = json.load(f)
        except (FileNotFoundError, json.JSONDecodeError):
            pass

        # Load 04-validation.json
        validation_path = os.path.join(hdir, "04-validation.json")
        try:
            with open(validation_path, "r") as f:
                validation = json.load(f)
        except (FileNotFoundError, json.JSONDecodeError):
            pass

        # Derive session_id: validation > plan > dirname
        session_id = (validation.get("session_id")
                      or plan.get("session_id")
                      or base)

        # Determine completeness
        has_validation = bool(validation)
        has_verdict = bool(validation.get("verdict")) if has_validation else False
        incomplete = not has_validation or not has_verdict

        # Determine verdict
        verdict = validation.get("verdict", "incomplete") if not incomplete else "incomplete"

        # Determine retry
        retry = False
        ri = validation.get("retry_instructions") if has_validation else None
        if ri is not None:
            if isinstance(ri, str) and len(ri) > 0:
                retry = True
            elif isinstance(ri, list) and len(ri) > 0:
                retry = True

        # PR URL
        pr_url = validation.get("pr_url") if has_validation else None

        # Complexity from plan
        complexity = plan.get("complexity", "unknown")

        session = {
            "session_id": session_id,
            "worktree_dir": base,
            "incomplete": incomplete,
            "verdict": verdict,
            "retry": retry,
            "pr_url": pr_url,
            "complexity": complexity,
            "has_plan": bool(plan),
            "has_build": bool(build),
            "has_validation": has_validation,
        }
        sessions.append(session)

    return sessions


def compute_verdict_dist(sessions):
    """Count verdict distribution: PASS, FAIL, PASS WITH NOTES, incomplete."""
    counts = Counter()
    for s in sessions:
        counts[s["verdict"]] += 1
    return dict(counts)


def compute_retry_rate(sessions):
    """Compute retry rate across sessions."""
    total = len(sessions)
    retry_count = sum(1 for s in sessions if s["retry"])
    return {
        "total": total,
        "retry_count": retry_count,
        "retry_rate": round(retry_count / total * 100, 2) if total > 0 else 0.0,
    }


def compute_pr_success(sessions):
    """Count PR success rate among completed sessions."""
    completed = [s for s in sessions if not s["incomplete"]]
    total = len(sessions)
    completed_count = len(completed)
    with_pr = sum(1 for s in completed if s["pr_url"])
    pr_rate = round(with_pr / total * 100, 2) if total > 0 else 0.0
    return {
        "total": total,
        "completed": completed_count,
        "with_pr": with_pr,
        "pr_success_rate": pr_rate,
    }


def compute_complexity_breakdown(sessions):
    """Count complexity levels from plan data."""
    counts = Counter()
    for s in sessions:
        c = s["complexity"]
        if c in ("simple", "medium", "complex"):
            counts[c] += 1
        else:
            counts["other"] += 1
    return dict(counts)


def compute_trend(sessions):
    """Bucket sessions by YYYYMMDD date prefix from session_id."""
    bucket = defaultdict(lambda: {"total": 0, "completed": 0, "passed": 0,
                                   "failed": 0, "incomplete": 0, "retry": 0})
    for s in sessions:
        m = re.match(r"^(\d{8})", s["session_id"])
        if not m:
            continue
        day = m.group(1)
        b = bucket[day]
        b["total"] += 1
        if s["incomplete"]:
            b["incomplete"] += 1
        else:
            b["completed"] += 1
            if s["verdict"] == "PASS":
                b["passed"] += 1
            elif s["verdict"] == "FAIL":
                b["failed"] += 1
        if s["retry"]:
            b["retry"] += 1

    result = []
    for day in sorted(bucket.keys()):
        b = bucket[day]
        pct = round(b["passed"] / b["total"] * 100, 2) if b["total"] > 0 else 0.0
        result.append({
            "day": day,
            "total": b["total"],
            "completed": b["completed"],
            "passed": b["passed"],
            "failed": b["failed"],
            "incomplete": b["incomplete"],
            "retry": b["retry"],
            "pass_rate_pct": pct,
        })
    return result


def fmt_text(verdict_dist, retry_rate, pr_success, complexity_breakdown, trend):
    """Produce human-readable text output with section headers."""
    lines = []

    def section(title):
        lines.append("")
        lines.append("## {}".format(title))

    def kv(key, value):
        lines.append("  {}: {}".format(key, value))

    section("Verdict Distribution")
    for v, c in sorted(verdict_dist.items()):
        kv(v, str(c))

    section("Retry Rate")
    kv("total", str(retry_rate["total"]))
    kv("retry_count", str(retry_rate["retry_count"]))
    kv("retry_rate", "{:.2f}%".format(retry_rate["retry_rate"]))

    section("PR Success Rate")
    kv("total", str(pr_success["total"]))
    kv("completed", str(pr_success["completed"]))
    kv("with_pr", str(pr_success["with_pr"]))
    kv("pr_success_rate", "{:.2f}%".format(pr_success["pr_success_rate"]))

    section("Complexity Breakdown")
    for c, n in sorted(complexity_breakdown.items()):
        kv(c, str(n))

    if trend:
        section("Trend (by day)")
        lines.append("  {:<10s} {:>6s} {:>6s} {:>6s} {:>6s} {:>10s} {:>8s}".format(
            "day", "total", "pass", "fail", "incomp", "retry", "pass%"))
        for t in trend:
            lines.append("  {:<10s} {:>6d} {:>6d} {:>6d} {:>6d} {:>10d} {:>7.2f}%".format(
                t["day"], t["total"], t["passed"], t["failed"],
                t["incomplete"], t["retry"], t["pass_rate_pct"]))

    return "\n".join(lines) + "\n"


def fmt_json(verdict_dist, retry_rate, pr_success, complexity_breakdown, trend):
    """Produce JSON output following mcp-metrics.py conventions."""
    data = {
        "verdict_distribution": verdict_dist,
        "retry_rate": retry_rate,
        "pr_success": pr_success,
        "complexity_breakdown": complexity_breakdown,
    }
    if trend:
        data["trend"] = trend
    return json.dumps(data, indent=2) + "\n"


def fmt_csv(verdict_dist, retry_rate, pr_success, complexity_breakdown, trend):
    """Produce CSV output with ## section headers, matching mcp-metrics.py."""
    buf = StringIO()
    w = csv.writer(buf)

    w.writerow(["## verdict_distribution"])
    w.writerow(["verdict", "count"])
    for v, c in sorted(verdict_dist.items()):
        w.writerow([v, c])

    w.writerow([])
    w.writerow(["## retry_rate"])
    w.writerow(["total", "retry_count", "retry_rate_pct"])
    w.writerow([retry_rate["total"], retry_rate["retry_count"],
                "{:.2f}".format(retry_rate["retry_rate"])])

    w.writerow([])
    w.writerow(["## pr_success"])
    w.writerow(["total", "completed", "with_pr", "pr_success_rate_pct"])
    w.writerow([pr_success["total"], pr_success["completed"],
                pr_success["with_pr"],
                "{:.2f}".format(pr_success["pr_success_rate"])])

    w.writerow([])
    w.writerow(["## complexity_breakdown"])
    w.writerow(["complexity", "count"])
    for c, n in sorted(complexity_breakdown.items()):
        w.writerow([c, n])

    if trend:
        w.writerow([])
        w.writerow(["## trend"])
        w.writerow(["day", "total", "completed", "passed", "failed",
                    "incomplete", "retry", "pass_rate_pct"])
        for t in trend:
            w.writerow([t["day"], t["total"], t["completed"], t["passed"],
                        t["failed"], t["incomplete"], t["retry"],
                        "{:.2f}".format(t["pass_rate_pct"])])

    return buf.getvalue()


def main():
    parser = argparse.ArgumentParser(
        description="Goose-coder handoff analytics for aptu-coder.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--worktrees-dir", required=True,
                        help="Path to .worktrees directory.")
    parser.add_argument("--format", dest="fmt",
                        choices=["text", "json", "csv"], default="text",
                        help="Output format (default: text).")
    parser.add_argument("--json", action="store_true",
                        help="Shorthand for --format json.")
    parser.add_argument("--csv", action="store_true",
                        help="Shorthand for --format csv.")
    args = parser.parse_args()

    # Handle shorthand flags
    if args.json:
        fmt = "json"
    elif args.csv:
        fmt = "csv"
    else:
        fmt = args.fmt

    worktrees_dir = os.path.abspath(args.worktrees_dir)
    if not os.path.isdir(worktrees_dir):
        print("Directory not found: {}".format(worktrees_dir), file=sys.stderr)
        sys.exit(1)

    sessions = load_sessions(worktrees_dir)
    if not sessions:
        print("No sessions found in {}.".format(worktrees_dir))
        sys.exit(0)

    verdict_dist = compute_verdict_dist(sessions)
    retry_rate = compute_retry_rate(sessions)
    pr_success = compute_pr_success(sessions)
    complexity_breakdown = compute_complexity_breakdown(sessions)
    trend = compute_trend(sessions)

    if fmt == "json":
        print(fmt_json(verdict_dist, retry_rate, pr_success,
                       complexity_breakdown, trend), end="")
    elif fmt == "csv":
        print(fmt_csv(verdict_dist, retry_rate, pr_success,
                      complexity_breakdown, trend), end="")
    else:
        print(fmt_text(verdict_dist, retry_rate, pr_success,
                       complexity_breakdown, trend), end="")


if __name__ == "__main__":
    main()