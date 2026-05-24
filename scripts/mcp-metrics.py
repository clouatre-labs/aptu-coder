#!/usr/bin/env python3
"""
mcp-metrics.py -- MCP tool-call observability for aptu-coder.

Reads daily-rotated JSONL files from $XDG_DATA_HOME/aptu-coder/ and
produces four evidence-based sections aligned with OTel GenAI semantic
conventions (mcp.server.operation.duration, error.type) and industry-
standard AI agent observability practice (latency SLOs at p50/p95/p99,
tool success rate, cache latency savings, non-zero exit rate).

Zero external dependencies -- stdlib only.

Usage:
  python scripts/mcp-metrics.py                        # full summary
  python scripts/mcp-metrics.py --trend                # + daily breakdown
  python scripts/mcp-metrics.py --tool exec_command    # single tool
  python scripts/mcp-metrics.py --from 2026-05-01      # date filter
  python scripts/mcp-metrics.py --format json | jq .   # machine-readable
  python scripts/mcp-metrics.py --format csv           # spreadsheet export
  python scripts/mcp-metrics.py --all-tools            # include legacy tools
"""

import argparse
import csv
import json
import os
import sys
from collections import defaultdict
from datetime import datetime, timezone
from glob import glob
from io import StringIO


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

# The seven tools shipped in the current server.  Records from renamed or
# removed tools (analyze_raw, remote_file, etc.) are excluded by default
# to avoid polluting aggregate statistics with obsolete data.
CURRENT_TOOLS = {
    "analyze_directory",
    "analyze_file",
    "analyze_module",
    "analyze_symbol",
    "edit_overwrite",
    "edit_replace",
    "exec_command",
}


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
    """Return the date encoded in a metrics-YYYY-MM-DD.jsonl filename, or None."""
    basename = os.path.basename(path)
    if basename.startswith("metrics-") and basename.endswith(".jsonl"):
        try:
            return datetime.strptime(basename[8:-6], "%Y-%m-%d").date()
        except ValueError:
            pass
    return None


def load_records(metrics_dir, from_date=None, to_date=None,
                 tool_filter=None, all_tools=False):
    """Parse JSONL files; apply date, tool, and schema-version filters."""
    records = []
    skipped = 0
    for path in sorted(glob(os.path.join(metrics_dir, "metrics-*.jsonl"))):
        fdate = file_date(path)
        if fdate is None:
            continue
        if from_date and fdate < from_date:
            continue
        if to_date and fdate > to_date:
            continue
        try:
            with open(path, encoding="utf-8") as f:
                for lineno, raw in enumerate(f, 1):
                    raw = raw.strip()
                    if not raw:
                        continue
                    try:
                        rec = json.loads(raw)
                    except json.JSONDecodeError as exc:
                        print("WARNING: {}:{}: {}: skipping".format(
                            path, lineno, exc), file=sys.stderr)
                        skipped += 1
                        continue
                    tool = rec.get("tool")
                    if tool_filter and tool != tool_filter:
                        continue
                    if not all_tools and tool not in CURRENT_TOOLS:
                        continue
                    records.append(rec)
        except OSError as exc:
            print("WARNING: cannot read {}: {}".format(path, exc),
                  file=sys.stderr)
    return records


def quantile(sorted_values, q):
    """Linear-interpolation quantile on a pre-sorted list (q in [0, 1])."""
    n = len(sorted_values)
    if n == 0:
        return 0
    if n == 1:
        return sorted_values[0]
    idx = q * (n - 1)
    lo = int(idx)
    hi = min(lo + 1, n - 1)
    return sorted_values[lo] + (idx - lo) * (sorted_values[hi] - sorted_values[lo])


def pct(numerator, denominator):
    return 0.0 if denominator == 0 else 100.0 * numerator / denominator


def ms_to_human(ms):
    """Format milliseconds as a human-readable string (e.g. 2184987 -> '36m 24s')."""
    ms = int(ms)
    if ms < 1000:
        return "{}ms".format(ms)
    s = ms // 1000
    if s < 60:
        return "{}s".format(s)
    m, s = divmod(s, 60)
    if m < 60:
        return "{}m {}s".format(m, s)
    h, m = divmod(m, 60)
    return "{}h {}m".format(h, m)


# ---------------------------------------------------------------------------
# Section 1: Latency & Output Size
# ---------------------------------------------------------------------------
# OTel reference: mcp.server.operation.duration histogram
# Metrics: p50 / p95 / p99 / max duration; p50 / p95 / max output_chars
# Industry standard: p50 + p95 + p99 required for SLO definition
# (groundcover.com AI Agent Observability Guide, 2026; OTel GenAI metrics spec)
# p99 surfaces severe tail latency invisible at p95 -- exec_command p95=4.8s
# but p99=61.7s in the production corpus.

def compute_latency(records):
    by_tool = defaultdict(list)
    for rec in records:
        by_tool[rec.get("tool", "unknown")].append(rec)

    rows = []
    for tool, recs in sorted(by_tool.items()):
        dur = sorted(r.get("duration_ms", 0) for r in recs)
        chars = sorted(r.get("output_chars", 0) for r in recs)
        rows.append({
            "tool": tool,
            "calls": len(recs),
            "dur_p50":  int(quantile(dur, 0.50)),
            "dur_p95":  int(quantile(dur, 0.95)),
            "dur_p99":  int(quantile(dur, 0.99)),
            "dur_max":  dur[-1] if dur else 0,
            "chars_p50": int(quantile(chars, 0.50)),
            "chars_p95": int(quantile(chars, 0.95)),
            "chars_max": chars[-1] if chars else 0,
            "truncated_pct": pct(
                sum(1 for r in recs if r.get("output_truncated") is True),
                len(recs)),
        })
    return rows


# ---------------------------------------------------------------------------
# Section 2: Reliability (Tool Success Rate + exec non-zero exits + timeouts)
# ---------------------------------------------------------------------------
# OTel reference: error.type attribute on mcp.server.operation.duration
# Metrics: tool success rate (primary SLO signal per industry consensus),
#   exec_command non-zero exit rate (silent partial failures not counted as
#   MCP errors), timed_out rate, error_type distribution
# Sources: OTel MCP semconv; Sentry AI Agent Observability Guide (2026);
#   marktechpost.com Top 7 Best Practices (2025); atlan.com Complete Guide (2026)
#
# Note: exec_command with exit_code != 0 is NOT an MCP-layer error (result="ok")
# but IS an application-layer failure -- the agent ran a command that failed.
# This is a composite signal not visible in the basic error_rate view.

def compute_reliability(records):
    by_tool = defaultdict(lambda: {
        "calls": 0, "errors": 0,
        "exit_nonzero": 0, "timed_out": 0,
        "error_types": defaultdict(int),
    })
    for rec in records:
        tool = rec.get("tool", "unknown")
        t = by_tool[tool]
        t["calls"] += 1
        if rec.get("result") == "error":
            t["errors"] += 1
            et = rec.get("error_type") or "unknown"
            t["error_types"][et] += 1
        if rec.get("exit_code") not in (None, 0):
            t["exit_nonzero"] += 1
        if rec.get("timed_out") is True:
            t["timed_out"] += 1

    rows = []
    for tool, t in sorted(by_tool.items()):
        rows.append({
            "tool": tool,
            "calls": t["calls"],
            "success_rate": pct(t["calls"] - t["errors"], t["calls"]),
            "error_rate":   pct(t["errors"], t["calls"]),
            "errors":       t["errors"],
            "exit_nonzero": t["exit_nonzero"],
            "exit_nonzero_pct": pct(t["exit_nonzero"], t["calls"]),
            "timed_out":    t["timed_out"],
            "timed_out_pct": pct(t["timed_out"], t["calls"]),
            "error_types": dict(sorted(
                t["error_types"].items(), key=lambda x: -x[1])),
        })
    return rows


# ---------------------------------------------------------------------------
# Section 3: Cache Effectiveness
# ---------------------------------------------------------------------------
# OTel reference: gen_ai.client.operation.duration with cache attributes
# Metrics: hit rate per tool and tier; median latency saved per cache hit
#   (latency savings = median(miss_duration) - median(hit_duration));
#   estimated wall-clock time saved across all hits; chars served from cache
# Sources: Sentry AI agent observability guide frames cache hit rate in terms
#   of latency avoided, not just chars (2026).  Industry consensus cites
#   30-50% cost/latency reduction from caching (zylos.ai, 2026).
#
# Latency savings is a composite metric: it requires correlating cache_hit
# boolean with duration_ms, which a simple hit-rate count cannot show.
# analyze_directory saves 806ms/call at median in the production corpus.

def compute_cache(records):
    # Per-tool: separate hit and miss durations
    by_tool = defaultdict(lambda: {
        "hit_dur": [], "miss_dur": [],
        "hit_chars": 0, "hits": 0, "cacheable": 0,
    })
    by_tier = defaultdict(int)
    write_failures = 0

    for rec in records:
        tool = rec.get("tool", "unknown")
        t = by_tool[tool]
        ch = rec.get("cache_hit")
        if ch is None:
            continue
        t["cacheable"] += 1
        dur = rec.get("duration_ms", 0)
        if ch is True:
            t["hits"] += 1
            t["hit_dur"].append(dur)
            t["hit_chars"] += rec.get("output_chars", 0)
            tier = rec.get("cache_tier") or "unknown"
            by_tier[tier] += 1
        else:
            t["miss_dur"].append(dur)
        if rec.get("cache_write_failure") is True:
            write_failures += 1

    total_cacheable = sum(v["cacheable"] for v in by_tool.values())
    total_hits = sum(v["hits"] for v in by_tool.values())
    total_hit_chars = sum(v["hit_chars"] for v in by_tool.values())

    per_tool = []
    total_ms_saved = 0
    for tool, t in sorted(by_tool.items()):
        hit_med = quantile(sorted(t["hit_dur"]), 0.5) if t["hit_dur"] else None
        miss_med = quantile(sorted(t["miss_dur"]), 0.5) if t["miss_dur"] else None
        if hit_med is not None and miss_med is not None:
            ms_saved_per_hit = max(0.0, miss_med - hit_med)
        else:
            ms_saved_per_hit = None
        total_tool_saved = (ms_saved_per_hit * t["hits"]
                            if ms_saved_per_hit is not None else 0)
        total_ms_saved += total_tool_saved
        per_tool.append({
            "tool": tool,
            "cacheable": t["cacheable"],
            "hits": t["hits"],
            "hit_rate": pct(t["hits"], t["cacheable"]),
            "hit_dur_median": int(hit_med) if hit_med is not None else None,
            "miss_dur_median": int(miss_med) if miss_med is not None else None,
            "ms_saved_per_hit": int(ms_saved_per_hit) if ms_saved_per_hit is not None else None,
            "total_ms_saved": int(total_tool_saved),
            "hit_chars": t["hit_chars"],
        })

    return {
        "overall_hit_rate": pct(total_hits, total_cacheable),
        "total_cacheable": total_cacheable,
        "total_hits": total_hits,
        "total_hit_chars": total_hit_chars,
        "total_ms_saved": int(total_ms_saved),
        "write_failures": write_failures,
        "write_failure_rate": pct(write_failures, total_cacheable),
        "per_tier": [{"tier": t, "hits": c}
                     for t, c in sorted(by_tier.items())],
        "per_tool": per_tool,
    }


# ---------------------------------------------------------------------------
# Section 4: Outliers (Slowest calls + High-error sessions)
# ---------------------------------------------------------------------------
# OTel reference: individual span data backing aggregate histograms
# Metrics: top-N slowest individual calls (tool, duration, session, seq);
#   top sessions by error count; sessions with non-zero exec exits
# Sources: Langfuse, AgentOps, Sentry all surface top-N slowest spans as a
#   primary debugging primitive -- "dashboards show totals; traces show
#   decisions" (Sentry, 2026).  Without this, a 36-minute exec_command call
#   is invisible behind a 4.8s p95 aggregate.

def compute_outliers(records, top_n=10):
    slowest = []
    by_session = defaultdict(lambda: {
        "calls": 0, "errors": 0,
        "exit_nonzero": 0, "timed_out": 0,
        "total_dur_ms": 0, "total_chars": 0,
    })

    for rec in records:
        tool = rec.get("tool", "unknown")
        dur = rec.get("duration_ms", 0) or 0
        sid = rec.get("session_id") or "unknown"
        seq = rec.get("seq")

        slowest.append({
            "duration_ms": dur,
            "tool": tool,
            "session_id": sid,
            "seq": seq,
            "timed_out": rec.get("timed_out", False),
            "exit_code": rec.get("exit_code"),
        })

        s = by_session[sid]
        s["calls"] += 1
        s["total_dur_ms"] += dur
        s["total_chars"] += rec.get("output_chars", 0)
        if rec.get("result") == "error":
            s["errors"] += 1
        if rec.get("exit_code") not in (None, 0):
            s["exit_nonzero"] += 1
        if rec.get("timed_out") is True:
            s["timed_out"] += 1

    slowest.sort(key=lambda x: x["duration_ms"], reverse=True)
    top_slow = slowest[:top_n]

    # Sessions ranked by error count, then by non-zero exits
    sessions_list = [
        {"session_id": sid, **s}
        for sid, s in by_session.items()
    ]
    top_by_errors = sorted(
        [s for s in sessions_list if s["errors"] > 0],
        key=lambda x: (-x["errors"], -x["calls"]),
    )[:top_n]
    top_by_calls = sorted(
        sessions_list,
        key=lambda x: -x["calls"],
    )[:top_n]

    return {
        "slowest_calls": top_slow,
        "top_sessions_by_errors": top_by_errors,
        "top_sessions_by_calls": top_by_calls,
    }


# ---------------------------------------------------------------------------
# Section 5: Daily Trend
# ---------------------------------------------------------------------------
# Aligned with OTel time-series metric conventions.
# Includes: calls, success_rate, cache_hit_rate, exec_nonzero_pct,
#   dur_p95, dur_p99, output_chars_p95 per day.
# p99 included in trend to detect tail-latency regressions across releases.

def compute_trend(records):
    by_day = defaultdict(list)
    for rec in records:
        ts = rec.get("ts")
        if not ts:
            continue
        try:
            day = datetime.fromtimestamp(
                ts / 1000.0, timezone.utc).strftime("%Y-%m-%d")
        except (ValueError, OSError, OverflowError):
            continue
        by_day[day].append(rec)

    rows = []
    for day in sorted(by_day):
        recs = by_day[day]
        calls = len(recs)
        errors = sum(1 for r in recs if r.get("result") == "error")
        cacheable = [r for r in recs if r.get("cache_hit") is not None]
        hits = sum(1 for r in cacheable if r.get("cache_hit") is True)
        exit_nonzero = sum(
            1 for r in recs if r.get("exit_code") not in (None, 0))
        dur = sorted(r.get("duration_ms", 0) for r in recs)
        chars = sorted(r.get("output_chars", 0) for r in recs)
        rows.append({
            "day": day,
            "calls": calls,
            "success_rate": pct(calls - errors, calls),
            "error_rate": pct(errors, calls),
            "cache_hit_rate": pct(hits, len(cacheable)) if cacheable else None,
            "exec_nonzero_pct": pct(exit_nonzero, calls),
            "dur_p95": int(quantile(dur, 0.95)),
            "dur_p99": int(quantile(dur, 0.99)),
            "chars_p95": int(quantile(chars, 0.95)),
        })
    return rows


# ---------------------------------------------------------------------------
# Text formatter
# ---------------------------------------------------------------------------

def fmt_text(latency, reliability, cache, outliers, trend, show_trend):
    lines = []

    def section(title):
        lines.append("")
        lines.append("=" * 76)
        lines.append("  {}".format(title))
        lines.append("=" * 76)

    def table(headers, widths, rows_data):
        # Header row
        parts = []
        for i, (h, w) in enumerate(zip(headers, widths)):
            align = "<" if i == 0 else ">"
            parts.append(("{:" + align + str(w) + "}").format(h))
        lines.append("  " + "  ".join(parts))
        lines.append("  " + "-" * (sum(widths) + 2 * (len(widths) - 1)))
        for row in rows_data:
            parts = []
            for i, (v, w) in enumerate(zip(row, widths)):
                align = "<" if i == 0 else ">"
                parts.append(("{:" + align + str(w) + "}").format(str(v)[:w]))
            lines.append("  " + "  ".join(parts))

    # ------------------------------------------------------------------
    # 1. Latency & Output Size
    # ------------------------------------------------------------------
    section("1. Latency & Output Size  "
            "[OTel: mcp.server.operation.duration | SLO: p50/p95/p99]")
    table(
        ["tool", "calls", "p50ms", "p95ms", "p99ms", "max", "chars_p50", "chars_p95", "trunc%"],
        [22,      7,       6,       6,       6,       9,     9,           9,           7],
        [
            [r["tool"], r["calls"],
             r["dur_p50"], r["dur_p95"], r["dur_p99"],
             ms_to_human(r["dur_max"]),
             r["chars_p50"], r["chars_p95"],
             "{:.1f}".format(r["truncated_pct"])]
            for r in latency
        ],
    )

    # ------------------------------------------------------------------
    # 2. Reliability
    # ------------------------------------------------------------------
    section("2. Reliability  "
            "[OTel: error.type | Signals: success_rate, exit!=0, timed_out]")
    table(
        ["tool", "calls", "success%", "error%", "errors", "exit!=0", "exit!=0%", "timedout"],
        [22,      7,       9,          7,         7,        7,         9,           8],
        [
            [r["tool"], r["calls"],
             "{:.1f}".format(r["success_rate"]),
             "{:.1f}".format(r["error_rate"]),
             r["errors"],
             r["exit_nonzero"],
             "{:.1f}".format(r["exit_nonzero_pct"]),
             r["timed_out"]]
            for r in reliability
        ],
    )
    # Error type breakdown (non-zero only)
    all_etypes = defaultdict(int)
    for r in reliability:
        for et, cnt in r["error_types"].items():
            all_etypes[et] += cnt
    if all_etypes:
        lines.append("")
        lines.append("  Error type distribution:")
        for et, cnt in sorted(all_etypes.items(), key=lambda x: -x[1]):
            lines.append("    {:30s}  {:>5}".format(et, cnt))

    # ------------------------------------------------------------------
    # 3. Cache Effectiveness
    # ------------------------------------------------------------------
    section("3. Cache Effectiveness  "
            "[Composite: latency saved = miss_median - hit_median per call]")
    ch = cache
    lines.append(
        "  Overall hit rate : {:.1f}%  ({} hits / {} cacheable calls)".format(
            ch["overall_hit_rate"], ch["total_hits"], ch["total_cacheable"]))
    lines.append(
        "  Est. wall-clock saved : {}  ({:,} chars served from cache)".format(
            ms_to_human(ch["total_ms_saved"]), ch["total_hit_chars"]))
    if ch["write_failures"]:
        lines.append(
            "  Cache write failures  : {}  ({:.2f}%)".format(
                ch["write_failures"], ch["write_failure_rate"]))
    if ch["per_tier"]:
        tier_str = "  ".join(
            "{}: {}".format(t["tier"], t["hits"]) for t in ch["per_tier"])
        lines.append("  Tier breakdown        : {}".format(tier_str))
    lines.append("")
    table(
        ["tool", "cacheable", "hits", "hit%", "hit_med_ms", "miss_med_ms", "saved_ms/hit", "total_saved"],
        [22,      9,           6,      6,       10,           11,            13,              12],
        [
            [r["tool"],
             r["cacheable"], r["hits"],
             "{:.1f}".format(r["hit_rate"]),
             r["hit_dur_median"] if r["hit_dur_median"] is not None else "n/a",
             r["miss_dur_median"] if r["miss_dur_median"] is not None else "n/a",
             r["ms_saved_per_hit"] if r["ms_saved_per_hit"] is not None else "n/a",
             ms_to_human(r["total_ms_saved"]) if r["total_ms_saved"] else "0ms"]
            for r in ch["per_tool"]
        ],
    )

    # ------------------------------------------------------------------
    # 4. Outliers
    # ------------------------------------------------------------------
    section("4. Outliers  "
            "[Top-N slowest calls + high-error sessions]")
    lines.append("  Slowest {} individual calls:".format(len(outliers["slowest_calls"])))
    table(
        ["duration", "tool", "session_id", "seq", "timed_out", "exit_code"],
        [10,          22,     24,            5,     9,           9],
        [
            [ms_to_human(r["duration_ms"]),
             r["tool"],
             str(r["session_id"])[:24],
             str(r["seq"]) if r["seq"] is not None else "?",
             "YES" if r["timed_out"] else "",
             str(r["exit_code"]) if r["exit_code"] is not None else ""]
            for r in outliers["slowest_calls"]
        ],
    )
    if outliers["top_sessions_by_errors"]:
        lines.append("")
        lines.append("  Sessions with most errors:")
        table(
            ["session_id", "calls", "errors", "exit!=0", "timed_out"],
            [28,            7,       7,         7,         9],
            [
                [str(s["session_id"])[:28], s["calls"], s["errors"],
                 s["exit_nonzero"], s["timed_out"]]
                for s in outliers["top_sessions_by_errors"]
            ],
        )
    lines.append("")
    lines.append("  Top sessions by call volume:")
    table(
        ["session_id", "calls", "errors", "total_chars", "total_dur"],
        [28,            7,       7,         11,            10],
        [
            [str(s["session_id"])[:28], s["calls"], s["errors"],
             s["total_chars"],
             ms_to_human(s["total_dur_ms"])]
            for s in outliers["top_sessions_by_calls"]
        ],
    )

    # ------------------------------------------------------------------
    # 5. Daily Trend
    # ------------------------------------------------------------------
    if show_trend:
        section("5. Daily Trend  "
                "[p95+p99 for tail-latency regression detection]")
        table(
            ["day", "calls", "success%", "cache_hit%", "exec_exit!=0%", "p95ms", "p99ms", "chars_p95"],
            [12,     7,       9,          10,            14,               6,       6,       9],
            [
                [r["day"], r["calls"],
                 "{:.1f}".format(r["success_rate"]),
                 "{:.1f}".format(r["cache_hit_rate"]) if r["cache_hit_rate"] is not None else "n/a",
                 "{:.1f}".format(r["exec_nonzero_pct"]),
                 r["dur_p95"], r["dur_p99"],
                 r["chars_p95"]]
                for r in trend
            ],
        )

    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# JSON formatter
# ---------------------------------------------------------------------------

def fmt_json(latency, reliability, cache, outliers, trend, show_trend):
    data = {
        "latency": latency,
        "reliability": reliability,
        "cache": cache,
        "outliers": outliers,
    }
    if show_trend:
        data["trend"] = trend
    return json.dumps(data, indent=2)


# ---------------------------------------------------------------------------
# CSV formatter
# ---------------------------------------------------------------------------

def fmt_csv(latency, reliability, cache, outliers, trend, show_trend):
    buf = StringIO()
    w = csv.writer(buf)

    w.writerow(["## latency"])
    w.writerow(["tool", "calls", "dur_p50", "dur_p95", "dur_p99", "dur_max_ms",
                "chars_p50", "chars_p95", "chars_max", "truncated_pct"])
    for r in latency:
        w.writerow([r["tool"], r["calls"],
                    r["dur_p50"], r["dur_p95"], r["dur_p99"], r["dur_max"],
                    r["chars_p50"], r["chars_p95"], r["chars_max"],
                    "{:.2f}".format(r["truncated_pct"])])

    w.writerow([])
    w.writerow(["## reliability"])
    w.writerow(["tool", "calls", "success_rate", "error_rate", "errors",
                "exit_nonzero", "exit_nonzero_pct", "timed_out", "timed_out_pct"])
    for r in reliability:
        w.writerow([r["tool"], r["calls"],
                    "{:.2f}".format(r["success_rate"]),
                    "{:.2f}".format(r["error_rate"]),
                    r["errors"], r["exit_nonzero"],
                    "{:.2f}".format(r["exit_nonzero_pct"]),
                    r["timed_out"],
                    "{:.2f}".format(r["timed_out_pct"])])

    w.writerow([])
    w.writerow(["## cache"])
    w.writerow(["tool", "cacheable", "hits", "hit_rate",
                "hit_dur_median_ms", "miss_dur_median_ms",
                "ms_saved_per_hit", "total_ms_saved", "hit_chars"])
    for r in cache["per_tool"]:
        w.writerow([r["tool"], r["cacheable"], r["hits"],
                    "{:.2f}".format(r["hit_rate"]),
                    r["hit_dur_median"] if r["hit_dur_median"] is not None else "",
                    r["miss_dur_median"] if r["miss_dur_median"] is not None else "",
                    r["ms_saved_per_hit"] if r["ms_saved_per_hit"] is not None else "",
                    r["total_ms_saved"],
                    r["hit_chars"]])

    w.writerow([])
    w.writerow(["## outliers_slowest"])
    w.writerow(["duration_ms", "tool", "session_id", "seq", "timed_out", "exit_code"])
    for r in outliers["slowest_calls"]:
        w.writerow([r["duration_ms"], r["tool"], r["session_id"],
                    r["seq"] if r["seq"] is not None else "",
                    r["timed_out"], r["exit_code"] if r["exit_code"] is not None else ""])

    if show_trend and trend:
        w.writerow([])
        w.writerow(["## trend"])
        w.writerow(["day", "calls", "success_rate", "error_rate",
                    "cache_hit_rate", "exec_nonzero_pct",
                    "dur_p95", "dur_p99", "chars_p95"])
        for r in trend:
            w.writerow([r["day"], r["calls"],
                        "{:.2f}".format(r["success_rate"]),
                        "{:.2f}".format(r["error_rate"]),
                        "{:.2f}".format(r["cache_hit_rate"]) if r["cache_hit_rate"] is not None else "",
                        "{:.2f}".format(r["exec_nonzero_pct"]),
                        r["dur_p95"], r["dur_p99"], r["chars_p95"]])

    return buf.getvalue()


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description="MCP tool-call observability for aptu-coder JSONL metrics.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--from", dest="from_date", metavar="YYYY-MM-DD",
                        type=parse_date_arg,
                        help="Include files on or after this date.")
    parser.add_argument("--to", dest="to_date", metavar="YYYY-MM-DD",
                        type=parse_date_arg,
                        help="Include files on or before this date.")
    parser.add_argument("--dir", dest="metrics_dir", metavar="DIR",
                        help="Directory containing metrics-*.jsonl files.")
    parser.add_argument("--format", dest="fmt",
                        choices=["text", "json", "csv"], default="text",
                        help="Output format (default: text).")
    parser.add_argument("--tool", dest="tool_filter", metavar="TOOL",
                        help="Restrict analysis to one tool.")
    parser.add_argument("--trend", action="store_true",
                        help="Append daily trend breakdown (section 5).")
    parser.add_argument("--top", dest="top_n", type=int, default=10,
                        metavar="N",
                        help="Number of outlier rows to show (default: 10).")
    parser.add_argument("--all-tools", action="store_true",
                        help="Include records from obsolete/renamed tools.")
    args = parser.parse_args()

    metrics_dir = args.metrics_dir or default_metrics_dir()
    if not os.path.isdir(metrics_dir):
        print("No metrics data found at {}.".format(metrics_dir))
        sys.exit(0)

    records = load_records(
        metrics_dir,
        from_date=args.from_date,
        to_date=args.to_date,
        tool_filter=args.tool_filter,
        all_tools=args.all_tools,
    )
    if not records:
        print("No metrics data found.")
        sys.exit(0)

    latency     = compute_latency(records)
    reliability = compute_reliability(records)
    cache       = compute_cache(records)
    outliers    = compute_outliers(records, top_n=args.top_n)
    trend       = compute_trend(records) if args.trend else []

    if args.fmt == "json":
        print(fmt_json(latency, reliability, cache, outliers, trend, args.trend))
    elif args.fmt == "csv":
        print(fmt_csv(latency, reliability, cache, outliers, trend, args.trend), end="")
    else:
        print(fmt_text(latency, reliability, cache, outliers, trend, args.trend))


if __name__ == "__main__":
    main()
