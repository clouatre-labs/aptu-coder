#!/usr/bin/env python3
"""
mcp-metrics.py -- Analyze aptu-coder JSONL metrics files.

Zero-dependency Python CLI (stdlib only). Reads daily-rotated JSONL files
from $XDG_DATA_HOME/aptu-coder/ and produces tool efficiency, cache health,
session pattern, and trend analysis.

Example commands:
  python scripts/mcp-metrics.py
  python scripts/mcp-metrics.py --from 2026-05-01 --to 2026-05-24
  python scripts/mcp-metrics.py --dir /path/to/metrics/
  python scripts/mcp-metrics.py --format json
  python scripts/mcp-metrics.py --format csv
  python scripts/mcp-metrics.py --tool exec_command
  python scripts/mcp-metrics.py --trend
"""

import argparse
import csv
import json
import os
import statistics
import sys
from collections import defaultdict
from datetime import date, datetime
from glob import glob
from io import StringIO


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def parse_date_arg(value):
    try:
        return datetime.strptime(value, "%Y-%m-%d").date()
    except ValueError:
        raise argparse.ArgumentTypeError(
            "Invalid date '{}': expected YYYY-MM-DD".format(value)
        )


def default_metrics_dir():
    xdg = os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share"))
    return os.path.join(xdg, "aptu-coder")


def file_date(path):
    """Extract date from metrics-YYYY-MM-DD.jsonl filename, or None."""
    basename = os.path.basename(path)
    if basename.startswith("metrics-") and basename.endswith(".jsonl"):
        date_part = basename[len("metrics-"):-len(".jsonl")]
        try:
            return datetime.strptime(date_part, "%Y-%m-%d").date()
        except ValueError:
            pass
    return None


def load_records(metrics_dir, from_date=None, to_date=None, tool_filter=None):
    pattern = os.path.join(metrics_dir, "metrics-*.jsonl")
    files = sorted(glob(pattern))
    records = []
    skipped = 0
    for path in files:
        fdate = file_date(path)
        if fdate is None:
            continue
        if from_date and fdate < from_date:
            continue
        if to_date and fdate > to_date:
            continue
        try:
            with open(path, encoding="utf-8") as f:
                for lineno, line in enumerate(f, 1):
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        rec = json.loads(line)
                    except json.JSONDecodeError as exc:
                        print(
                            "WARNING: {}: line {}: {}: skipping".format(path, lineno, exc),
                            file=sys.stderr,
                        )
                        skipped += 1
                        continue
                    if tool_filter and rec.get("tool") != tool_filter:
                        continue
                    records.append(rec)
        except OSError as exc:
            print("WARNING: cannot read {}: {}".format(path, exc), file=sys.stderr)
    return records


def quantile(values, q):
    """Return the q-th quantile (0..1) of a sorted list."""
    if not values:
        return 0
    n = len(values)
    if n == 1:
        return values[0]
    idx = q * (n - 1)
    lo = int(idx)
    hi = lo + 1
    if hi >= n:
        return values[-1]
    frac = idx - lo
    return values[lo] + frac * (values[hi] - values[lo])


def pct(numerator, denominator):
    if denominator == 0:
        return 0.0
    return 100.0 * numerator / denominator


# ---------------------------------------------------------------------------
# Analysis
# ---------------------------------------------------------------------------

def compute_tool_efficiency(records):
    by_tool = defaultdict(list)
    for rec in records:
        tool = rec.get("tool", "unknown")
        by_tool[tool].append(rec)

    rows = []
    for tool, recs in sorted(by_tool.items()):
        calls = len(recs)
        chars = sorted(rec.get("output_chars", 0) for rec in recs)
        durations = sorted(rec.get("duration_ms", 0) for rec in recs)
        truncated = [r for r in recs if r.get("output_truncated") is True]
        breach = [r for r in recs if r.get("chars_threshold_breach") is True]
        rows.append({
            "tool": tool,
            "calls": calls,
            "chars_p50": int(quantile(chars, 0.5)),
            "chars_p95": int(quantile(chars, 0.95)),
            "chars_max": chars[-1] if chars else 0,
            "dur_p50": int(quantile(durations, 0.5)),
            "dur_p95": int(quantile(durations, 0.95)),
            "truncated_rate": pct(len(truncated), calls),
            "breach_rate": pct(len(breach), calls),
        })
    return rows


def compute_cache_health(records):
    cacheable = [r for r in records if r.get("cache_hit") is not None]
    hits = [r for r in cacheable if r.get("cache_hit") is True]
    hit_chars = sum(r.get("output_chars", 0) for r in hits)

    by_tool = defaultdict(lambda: {"total": 0, "hits": 0})
    for rec in cacheable:
        tool = rec.get("tool", "unknown")
        by_tool[tool]["total"] += 1
        if rec.get("cache_hit") is True:
            by_tool[tool]["hits"] += 1

    by_tier = defaultdict(int)
    for rec in hits:
        tier = rec.get("cache_tier") or "unknown"
        by_tier[tier] += 1

    write_failures = [r for r in records if r.get("cache_write_failure") is True]

    per_tool = []
    for tool, counts in sorted(by_tool.items()):
        per_tool.append({
            "tool": tool,
            "cacheable_calls": counts["total"],
            "hit_rate": pct(counts["hits"], counts["total"]),
        })

    per_tier = [{"tier": t, "hits": c} for t, c in sorted(by_tier.items())]

    return {
        "overall_hit_rate": pct(len(hits), len(cacheable)),
        "cacheable_calls": len(cacheable),
        "total_hits": len(hits),
        "estimated_token_savings_chars": hit_chars,
        "write_failure_count": len(write_failures),
        "write_failure_rate": pct(len(write_failures), len(records)),
        "per_tool": per_tool,
        "per_tier": per_tier,
    }


def compute_session_patterns(records):
    by_session = defaultdict(lambda: {"calls": 0, "chars": 0, "errors": 0})
    for rec in records:
        sid = rec.get("session_id") or "unknown"
        by_session[sid]["calls"] += 1
        by_session[sid]["chars"] += rec.get("output_chars", 0)
        if rec.get("result") == "error":
            by_session[sid]["errors"] += 1

    top_by_calls = sorted(by_session.items(), key=lambda x: x[1]["calls"], reverse=True)[:10]
    top_by_chars = sorted(by_session.items(), key=lambda x: x[1]["chars"], reverse=True)[:10]

    error_by_tool = defaultdict(lambda: {"calls": 0, "errors": 0})
    for rec in records:
        tool = rec.get("tool", "unknown")
        error_by_tool[tool]["calls"] += 1
        if rec.get("result") == "error":
            error_by_tool[tool]["errors"] += 1

    error_type_dist = defaultdict(int)
    for rec in records:
        if rec.get("result") == "error":
            et = rec.get("error_type") or "unknown"
            error_type_dist[et] += 1

    return {
        "top_sessions_by_calls": [
            {"session_id": sid, "calls": d["calls"], "chars": d["chars"], "errors": d["errors"]}
            for sid, d in top_by_calls
        ],
        "top_sessions_by_chars": [
            {"session_id": sid, "calls": d["calls"], "chars": d["chars"], "errors": d["errors"]}
            for sid, d in top_by_chars
        ],
        "error_rate_by_tool": [
            {
                "tool": tool,
                "calls": counts["calls"],
                "errors": counts["errors"],
                "error_rate": pct(counts["errors"], counts["calls"]),
            }
            for tool, counts in sorted(error_by_tool.items())
        ],
        "error_type_distribution": [
            {"error_type": et, "count": c}
            for et, c in sorted(error_type_dist.items(), key=lambda x: x[1], reverse=True)
        ],
    }


def compute_trend(records):
    by_day = defaultdict(list)
    for rec in records:
        ts = rec.get("ts")
        if ts is None:
            continue
        try:
            day = datetime.utcfromtimestamp(ts / 1000.0).strftime("%Y-%m-%d")
        except (ValueError, OSError, OverflowError):
            continue
        by_day[day].append(rec)

    rows = []
    for day in sorted(by_day.keys()):
        recs = by_day[day]
        calls = len(recs)
        errors = sum(1 for r in recs if r.get("result") == "error")
        cacheable = [r for r in recs if r.get("cache_hit") is not None]
        hits = sum(1 for r in cacheable if r.get("cache_hit") is True)
        chars = sorted(r.get("output_chars", 0) for r in recs)
        rows.append({
            "day": day,
            "calls": calls,
            "errors": errors,
            "error_rate": pct(errors, calls),
            "cache_hit_rate": pct(hits, len(cacheable)),
            "output_chars_p95": int(quantile(chars, 0.95)),
        })
    return rows


# ---------------------------------------------------------------------------
# Formatters
# ---------------------------------------------------------------------------

def fmt_text(tool_efficiency, cache_health, session_patterns, trend, show_trend):
    lines = []

    def header(title):
        lines.append("")
        lines.append("=" * 72)
        lines.append("  " + title)
        lines.append("=" * 72)

    def table(col_headers, col_widths, rows_data):
        fmt = "  " + "  ".join(
            ("{:" + (">" if i > 0 else "<") + str(w) + "}").format(h)
            for i, (h, w) in enumerate(zip(col_headers, col_widths))
        )
        lines.append(fmt)
        lines.append("  " + "-" * (sum(col_widths) + 2 * (len(col_widths) - 1)))
        for row in rows_data:
            line = "  " + "  ".join(
                ("{:" + (">" if i > 0 else "<") + str(w) + "}").format(str(v)[:w])
                for i, (v, w) in enumerate(zip(row, col_widths))
            )
            lines.append(line)

    # Tool Efficiency
    header("Tool Efficiency")
    table(
        ["tool", "calls", "chars_p50", "chars_p95", "chars_max", "dur_p50ms", "dur_p95ms", "trunc%", "breach%"],
        [25, 7, 10, 10, 10, 10, 10, 7, 7],
        [
            [
                r["tool"], r["calls"],
                r["chars_p50"], r["chars_p95"], r["chars_max"],
                r["dur_p50"], r["dur_p95"],
                "{:.1f}".format(r["truncated_rate"]),
                "{:.1f}".format(r["breach_rate"]),
            ]
            for r in tool_efficiency
        ],
    )

    # Cache Health
    header("Cache Health")
    ch = cache_health
    lines.append("  Overall hit rate : {:.1f}%  ({} hits / {} cacheable calls)".format(
        ch["overall_hit_rate"], ch["total_hits"], ch["cacheable_calls"]
    ))
    lines.append("  Write failure rate: {:.2f}%  ({} failures)".format(
        ch["write_failure_rate"], ch["write_failure_count"]
    ))
    lines.append("  Est. token savings: {:,} chars served from cache".format(
        ch["estimated_token_savings_chars"]
    ))
    if ch["per_tier"]:
        lines.append("")
        lines.append("  Cache tier breakdown:")
        for t in ch["per_tier"]:
            lines.append("    {}: {} hits".format(t["tier"], t["hits"]))
    if ch["per_tool"]:
        lines.append("")
        table(
            ["tool", "cacheable", "hit_rate%"],
            [25, 10, 10],
            [[r["tool"], r["cacheable_calls"], "{:.1f}".format(r["hit_rate"])] for r in ch["per_tool"]],
        )

    # Session Patterns
    header("Session Patterns")
    lines.append("  Top 10 sessions by call volume:")
    table(
        ["session_id", "calls", "total_chars", "errors"],
        [28, 7, 13, 7],
        [[r["session_id"][:28], r["calls"], r["chars"], r["errors"]]
         for r in session_patterns["top_sessions_by_calls"]],
    )
    lines.append("")
    lines.append("  Error rate by tool:")
    table(
        ["tool", "calls", "errors", "error_rate%"],
        [25, 7, 7, 12],
        [
            [r["tool"], r["calls"], r["errors"], "{:.1f}".format(r["error_rate"])]
            for r in session_patterns["error_rate_by_tool"]
        ],
    )
    if session_patterns["error_type_distribution"]:
        lines.append("")
        lines.append("  Error type distribution:")
        for item in session_patterns["error_type_distribution"]:
            lines.append("    {}: {}".format(item["error_type"], item["count"]))

    # Trend
    if show_trend:
        header("Daily Trend")
        table(
            ["day", "calls", "errors", "error%", "cache_hit%", "chars_p95"],
            [12, 7, 7, 8, 11, 10],
            [
                [
                    r["day"], r["calls"], r["errors"],
                    "{:.1f}".format(r["error_rate"]),
                    "{:.1f}".format(r["cache_hit_rate"]),
                    r["output_chars_p95"],
                ]
                for r in trend
            ],
        )

    lines.append("")
    return "\n".join(lines)


def fmt_json(tool_efficiency, cache_health, session_patterns, trend, show_trend):
    data = {
        "tool_efficiency": tool_efficiency,
        "cache_health": cache_health,
        "session_patterns": session_patterns,
    }
    if show_trend:
        data["trend"] = trend
    return json.dumps(data, indent=2)


def fmt_csv(tool_efficiency, cache_health, session_patterns, trend, show_trend):
    buf = StringIO()
    w = csv.writer(buf)

    w.writerow(["## tool_efficiency"])
    w.writerow(["tool", "calls", "chars_p50", "chars_p95", "chars_max",
                "dur_p50", "dur_p95", "truncated_rate", "breach_rate"])
    for r in tool_efficiency:
        w.writerow([r["tool"], r["calls"], r["chars_p50"], r["chars_p95"],
                    r["chars_max"], r["dur_p50"], r["dur_p95"],
                    "{:.2f}".format(r["truncated_rate"]),
                    "{:.2f}".format(r["breach_rate"])])

    w.writerow([])
    w.writerow(["## cache_health"])
    ch = cache_health
    w.writerow(["overall_hit_rate", "cacheable_calls", "total_hits",
                "estimated_token_savings_chars", "write_failure_count", "write_failure_rate"])
    w.writerow([
        "{:.2f}".format(ch["overall_hit_rate"]),
        ch["cacheable_calls"], ch["total_hits"],
        ch["estimated_token_savings_chars"],
        ch["write_failure_count"],
        "{:.2f}".format(ch["write_failure_rate"]),
    ])

    w.writerow([])
    w.writerow(["## session_error_rate_by_tool"])
    w.writerow(["tool", "calls", "errors", "error_rate"])
    for r in session_patterns["error_rate_by_tool"]:
        w.writerow([r["tool"], r["calls"], r["errors"], "{:.2f}".format(r["error_rate"])])

    if show_trend and trend:
        w.writerow([])
        w.writerow(["## trend"])
        w.writerow(["day", "calls", "errors", "error_rate", "cache_hit_rate", "output_chars_p95"])
        for r in trend:
            w.writerow([
                r["day"], r["calls"], r["errors"],
                "{:.2f}".format(r["error_rate"]),
                "{:.2f}".format(r["cache_hit_rate"]),
                r["output_chars_p95"],
            ])

    return buf.getvalue()


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description="Analyze aptu-coder JSONL metrics files.",
        epilog=(
            "Examples:\n"
            "  python scripts/mcp-metrics.py\n"
            "  python scripts/mcp-metrics.py --from 2026-05-01 --to 2026-05-24\n"
            "  python scripts/mcp-metrics.py --dir /path/to/metrics/\n"
            "  python scripts/mcp-metrics.py --format json\n"
            "  python scripts/mcp-metrics.py --format csv\n"
            "  python scripts/mcp-metrics.py --tool exec_command\n"
            "  python scripts/mcp-metrics.py --trend\n"
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--from", dest="from_date", metavar="YYYY-MM-DD", type=parse_date_arg,
        help="Include files on or after this date.",
    )
    parser.add_argument(
        "--to", dest="to_date", metavar="YYYY-MM-DD", type=parse_date_arg,
        help="Include files on or before this date.",
    )
    parser.add_argument(
        "--dir", dest="metrics_dir", metavar="DIR",
        help="Directory containing metrics-*.jsonl files (overrides XDG path).",
    )
    parser.add_argument(
        "--format", dest="fmt", choices=["text", "json", "csv"], default="text",
        help="Output format: text (default), json, or csv.",
    )
    parser.add_argument(
        "--tool", dest="tool_filter", metavar="TOOL",
        help="Restrict analysis to a single tool name.",
    )
    parser.add_argument(
        "--trend", action="store_true",
        help="Show daily trend breakdown.",
    )
    args = parser.parse_args()

    metrics_dir = args.metrics_dir or default_metrics_dir()

    if not os.path.isdir(metrics_dir):
        print("No metrics data found.")
        sys.exit(0)

    records = load_records(
        metrics_dir,
        from_date=args.from_date,
        to_date=args.to_date,
        tool_filter=args.tool_filter,
    )

    if not records:
        print("No metrics data found.")
        sys.exit(0)

    tool_efficiency = compute_tool_efficiency(records)
    cache_health = compute_cache_health(records)
    session_patterns = compute_session_patterns(records)
    trend = compute_trend(records) if args.trend else []

    if args.fmt == "json":
        print(fmt_json(tool_efficiency, cache_health, session_patterns, trend, args.trend))
    elif args.fmt == "csv":
        print(fmt_csv(tool_efficiency, cache_health, session_patterns, trend, args.trend), end="")
    else:
        print(fmt_text(tool_efficiency, cache_health, session_patterns, trend, args.trend))


if __name__ == "__main__":
    main()
