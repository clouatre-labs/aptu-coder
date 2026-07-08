#!/usr/bin/env python3
"""
mcp-metrics.py -- MCP tool-call observability for aptu-coder.

Reads daily-rotated JSONL files from $XDG_DATA_HOME/aptu-coder/ and
produces ten evidence-based sections aligned with OTel GenAI semantic
conventions (mcp.server.operation.duration, error.type) and industry-
standard AI agent observability practice (latency SLOs at p50/p95/p99,
tool success rate, cache latency savings, non-zero exit rate, per-
parameter breakdowns, pagination adoption, feature usage, git_ref
adoption, and exec_command timeout configuration).

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
from typing import Any, Callable, Dict, List, NamedTuple


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


def load_records(
    metrics_dir, from_date=None, to_date=None, tool_filter=None, all_tools=False
):
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
                        print(
                            "WARNING: {}:{}: {}: skipping".format(path, lineno, exc),
                            file=sys.stderr,
                        )
                        skipped += 1
                        continue
                    tool = rec.get("tool")
                    if tool_filter and tool != tool_filter:
                        continue
                    if not all_tools and tool not in CURRENT_TOOLS:
                        continue
                    records.append(rec)
        except OSError as exc:
            print("WARNING: cannot read {}: {}".format(path, exc), file=sys.stderr)
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
        rows.append(
            {
                "tool": tool,
                "calls": len(recs),
                "dur_p50": int(quantile(dur, 0.50)),
                "dur_p95": int(quantile(dur, 0.95)),
                "dur_p99": int(quantile(dur, 0.99)),
                "dur_max": dur[-1] if dur else 0,
                "chars_p50": int(quantile(chars, 0.50)),
                "chars_p95": int(quantile(chars, 0.95)),
                "chars_max": chars[-1] if chars else 0,
                "truncated_pct": pct(
                    sum(1 for r in recs if r.get("output_truncated") is True), len(recs)
                ),
            }
        )
    return rows


# ---------------------------------------------------------------------------
# Section 2: Reliability (Tool Success Rate + exec non-zero exits + timeouts)
# ---------------------------------------------------------------------------
# OTel reference: error.type attribute on mcp.server.operation.duration
# Metrics: tool success rate (primary SLO signal per industry consensus),
#   exec_command non-zero exit rate (silent partial failures not counted as
#   errors by MCP), and timeout rate (timed_out=true).


def compute_reliability(records):
    by_tool = defaultdict(
        lambda: {
            "calls": 0,
            "errors": 0,
            "exit_nonzero": 0,
            "timed_out": 0,
            "error_types": defaultdict(int),
        }
    )
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
        rows.append(
            {
                "tool": tool,
                "calls": t["calls"],
                "success_rate": pct(t["calls"] - t["errors"], t["calls"]),
                "error_rate": pct(t["errors"], t["calls"]),
                "errors": t["errors"],
                "exit_nonzero": t["exit_nonzero"],
                "exit_nonzero_pct": pct(t["exit_nonzero"], t["calls"]),
                "timed_out": t["timed_out"],
                "timed_out_pct": pct(t["timed_out"], t["calls"]),
                "error_types": dict(
                    sorted(t["error_types"].items(), key=lambda x: -x[1])
                ),
            }
        )
    return rows


# ---------------------------------------------------------------------------
# Section 3: Cache Performance
# ---------------------------------------------------------------------------
# OTel reference: cache.hit_rate (custom metric)
# Metrics: per-tool cache hit rate, median latency for hits vs misses,
#   and total milliseconds saved by cache.


def compute_cache(records):
    cacheable_tools = {
        "analyze_directory",
        "analyze_file",
        "analyze_module",
        "analyze_symbol",
    }
    by_tool = defaultdict(list)
    by_tier = defaultdict(int)
    write_failures = 0

    for rec in records:
        tool = rec.get("tool", "unknown")
        if tool in cacheable_tools:
            by_tool[tool].append(rec)
            ch = rec.get("cache_hit")
            if ch is True:
                tier = rec.get("cache_tier") or "unknown"
                by_tier[tier] += 1
            if rec.get("cache_write_failure") is True:
                write_failures += 1

    per_tool = []
    total_hits = 0
    total_misses = 0
    total_ms_saved = 0
    total_hit_chars = 0

    for tool, recs in sorted(by_tool.items()):
        hits = sum(1 for r in recs if r.get("cache_hit") is True)
        misses = len(recs) - hits
        total_hits += hits
        total_misses += misses

        hit_durs = sorted(
            r.get("duration_ms", 0) for r in recs if r.get("cache_hit") is True
        )
        miss_durs = sorted(
            r.get("duration_ms", 0) for r in recs if r.get("cache_hit") is False
        )

        hit_dur_median = quantile(hit_durs, 0.5) if hit_durs else None
        miss_dur_median = quantile(miss_durs, 0.5) if miss_durs else None

        ms_saved_per_hit = (
            (miss_dur_median - hit_dur_median)
            if hit_dur_median and miss_dur_median
            else 0
        )
        total_ms_saved_tool = int(ms_saved_per_hit * hits)
        total_ms_saved += total_ms_saved_tool

        hit_chars = sum(
            r.get("output_chars", 0) for r in recs if r.get("cache_hit") is True
        )
        total_hit_chars += hit_chars

        per_tool.append(
            {
                "tool": tool,
                "cacheable": len(recs),
                "hits": hits,
                "hit_rate": pct(hits, len(recs)),
                "hit_dur_median": int(hit_dur_median) if hit_dur_median else None,
                "miss_dur_median": int(miss_dur_median) if miss_dur_median else None,
                "ms_saved_per_hit": int(ms_saved_per_hit),
                "total_ms_saved": total_ms_saved_tool,
                "hit_chars": hit_chars,
            }
        )

    total_cacheable = total_hits + total_misses
    return {
        "per_tool": per_tool,
        "total_hits": total_hits,
        "total_misses": total_misses,
        "total_ms_saved": total_ms_saved,
        "write_failures": write_failures,
        "write_failure_rate": pct(write_failures, total_cacheable)
        if total_cacheable > 0
        else 0,
        "per_tier": [{"tier": t, "hits": c} for t, c in sorted(by_tier.items())],
        "overall_hit_rate": pct(total_hits, total_cacheable)
        if total_cacheable > 0
        else 0,
        "total_cacheable": total_cacheable,
        "total_hit_chars": total_hit_chars,
    }


# ---------------------------------------------------------------------------
# Section 4: Outliers (Slowest Calls)
# ---------------------------------------------------------------------------
# Top N slowest calls by duration_ms, with tool, session_id, seq, timed_out, exit_code.


def compute_outliers(records, top_n=10):
    sorted_recs = sorted(records, key=lambda r: r.get("duration_ms", 0), reverse=True)
    slowest = []
    for r in sorted_recs[:top_n]:
        slowest.append(
            {
                "duration_ms": r.get("duration_ms", 0),
                "tool": r.get("tool", "unknown"),
                "session_id": r.get("session_id", ""),
                "seq": r.get("seq"),
                "timed_out": r.get("timed_out", False),
                "exit_code": r.get("exit_code"),
            }
        )
    return {"slowest_calls": slowest}


# ---------------------------------------------------------------------------
# Section 5: Trend (Daily Breakdown)
# ---------------------------------------------------------------------------
# Per-day aggregates: calls, success_rate, error_rate, cache_hit_rate,
# exec_nonzero_pct, dur_p95, dur_p99, chars_p95.


def compute_trend(records):
    by_day = defaultdict(list)
    for rec in records:
        # Try file_date first; fall back to deriving from ts if absent
        fdate = rec.get("file_date")
        if not fdate:
            ts = rec.get("ts")
            if ts:
                try:
                    fdate = datetime.fromtimestamp(ts / 1000.0, timezone.utc).strftime(
                        "%Y-%m-%d"
                    )
                except (ValueError, OSError, OverflowError):
                    continue
            else:
                continue
        by_day[fdate].append(rec)

    rows = []
    for day in sorted(by_day.keys()):
        day_recs = by_day[day]
        errors = sum(1 for r in day_recs if r.get("result") == "error")
        success = len(day_recs) - errors
        cache_hits = sum(1 for r in day_recs if r.get("cache_hit") is True)
        exec_nonzero = sum(1 for r in day_recs if r.get("exit_code") not in (None, 0))
        dur = sorted(r.get("duration_ms", 0) for r in day_recs)
        chars = sorted(r.get("output_chars", 0) for r in day_recs)

        rows.append(
            {
                "day": day,
                "calls": len(day_recs),
                "success_rate": pct(success, len(day_recs)),
                "error_rate": pct(errors, len(day_recs)),
                "cache_hit_rate": pct(cache_hits, len(day_recs))
                if cache_hits
                else None,
                "exec_nonzero_pct": pct(exec_nonzero, len(day_recs)),
                "dur_p95": int(quantile(dur, 0.95)),
                "dur_p99": int(quantile(dur, 0.99)),
                "chars_p95": int(quantile(chars, 0.95)),
            }
        )
    return rows


# ---------------------------------------------------------------------------
# Section 6: Parameter Usage
# ---------------------------------------------------------------------------
# Per-tool breakdown of optional bool parameters: summary_mode,
# fields_projected, working_dir_used, stdin_provided.


def compute_params_usage(records):
    by_tool = defaultdict(
        lambda: {
            "summary_mode": {"true": 0, "present": 0},
            "fields_projected": {"true": 0, "present": 0},
            "working_dir_used": {"true": 0, "present": 0},
            "stdin_provided": {"true": 0, "present": 0},
        }
    )

    for rec in records:
        tool = rec.get("tool", "unknown")
        for field in [
            "summary_mode",
            "fields_projected",
            "working_dir_used",
            "stdin_provided",
        ]:
            val = rec.get(field)
            if val is not None:
                by_tool[tool][field]["present"] += 1
                if val is True:
                    by_tool[tool][field]["true"] += 1

    return by_tool


# ---------------------------------------------------------------------------
# Section 7: Pagination Adoption
# ---------------------------------------------------------------------------
# Tracks cursor-based pagination (is_paginated) vs summary-mode calls vs
# first-page-only calls across all tools.  Skipped when no record carries
# pagination or summary fields.


def compute_pagination(records):
    has_any = False
    for rec in records:
        if rec.get("is_paginated") is not None or rec.get("summary_mode") is not None:
            has_any = True
            break
    if not has_any:
        return None

    paginated = 0
    summary_mode = 0
    first_page_only = 0
    for rec in records:
        if rec.get("is_paginated") is True:
            paginated += 1
        elif rec.get("summary_mode") is True:
            summary_mode += 1
        else:
            first_page_only += 1

    return {
        "paginated": paginated,
        "summary_mode": summary_mode,
        "first_page_only": first_page_only,
        "total": paginated + summary_mode + first_page_only,
    }


# ---------------------------------------------------------------------------
# Section 8: Feature Adoption (analyze_symbol)
# ---------------------------------------------------------------------------
# For analyze_symbol calls only: import_lookup, def_use, impl_only bool
# distributions and match_mode string histogram (default 'exact').


def compute_features(records):
    has_any = False
    sym_records = [r for r in records if r.get("tool") == "analyze_symbol"]
    feature_fields = ["import_lookup", "def_use", "impl_only"]
    for rec in sym_records:
        if (
            any(rec.get(f) is not None for f in feature_fields)
            or rec.get("match_mode") is not None
        ):
            has_any = True
            break
    if not has_any:
        return None

    bool_counts = {f: {"true": 0, "false": 0} for f in feature_fields}
    match_mode_counts = defaultdict(int)
    for rec in sym_records:
        for f in feature_fields:
            val = rec.get(f)
            if val is True:
                bool_counts[f]["true"] += 1
            elif val is False:
                bool_counts[f]["false"] += 1
        mm = rec.get("match_mode") or "exact"
        match_mode_counts[mm] += 1

    return {
        "total": len(sym_records),
        "bool_fields": bool_counts,
        "match_mode": dict(match_mode_counts),
    }


# ---------------------------------------------------------------------------
# Section 9: git_ref Adoption
# ---------------------------------------------------------------------------
# Per-tool breakdown of git_ref_used for tools that support git refs
# (analyze_directory, analyze_symbol).


def compute_git_ref(records):
    has_any = False
    for rec in records:
        if rec.get("git_ref_used") is not None:
            has_any = True
            break
    if not has_any:
        return None

    ref_tools = {"analyze_directory", "analyze_symbol"}
    by_tool = {}
    for rec in records:
        tool = rec.get("tool", "unknown")
        if tool not in ref_tools:
            continue
        t = by_tool.setdefault(tool, {"calls": 0, "git_ref_used": 0})
        t["calls"] += 1
        if rec.get("git_ref_used") is True:
            t["git_ref_used"] += 1
    return by_tool


# ---------------------------------------------------------------------------
# Section 10: exec_command Timeout Configuration
# ---------------------------------------------------------------------------
# Histogram of timeout_configured_ms in buckets and drain_timeout_ms
# adoption for exec_command records only.


def compute_timeout(records):
    exec_records = [r for r in records if r.get("tool") == "exec_command"]
    has_any = False
    for rec in exec_records:
        if (
            rec.get("timeout_configured_ms") is not None
            or rec.get("drain_timeout_ms") is not None
        ):
            has_any = True
            break
    if not has_any:
        return None

    buckets = {
        "none": 0,
        "<1000": 0,
        "1000-10000": 0,
        "10000-60000": 0,
        ">60000": 0,
    }
    drain_count = 0
    for rec in exec_records:
        to = rec.get("timeout_configured_ms")
        if to is None:
            buckets["none"] += 1
        elif to < 1000:
            buckets["<1000"] += 1
        elif to <= 10000:
            buckets["1000-10000"] += 1
        elif to <= 60000:
            buckets["10000-60000"] += 1
        else:
            buckets[">60000"] += 1
        if rec.get("drain_timeout_ms") is not None:
            drain_count += 1

    return {
        "total": len(exec_records),
        "timeout_buckets": buckets,
        "drain_configured": drain_count,
    }


# ---------------------------------------------------------------------------
# Section Registry and Helpers
# ---------------------------------------------------------------------------


def _section(lines: List[str], title: str) -> None:
    """Emit a section header."""
    lines.append("")
    lines.append("=" * 76)
    lines.append("  {}".format(title))
    lines.append("=" * 76)


def _table(
    lines: List[str], headers: List[str], widths: List[int], rows_data: List[List[Any]]
) -> None:
    """Emit a formatted table."""
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


# Private implementation detail: registry entry for a section.
# Used internally by fmt_text and fmt_csv; not part of the public API.
class SectionSpec(NamedTuple):
    """Registry entry for a section."""

    key: str
    title: str
    render_text: Callable[[List[str], Any], None]
    render_csv: Callable[[Any, csv.writer], None]


def _render_text_latency(lines: List[str], data: List[Dict]) -> None:
    """Render latency section for text output."""
    _section(
        lines,
        "1. Latency & Output Size  "
        "[OTel: mcp.server.operation.duration | SLO: p50/p95/p99]",
    )
    _table(
        lines,
        [
            "tool",
            "calls",
            "p50",
            "p95",
            "p99",
            "max",
            "chars_p50",
            "chars_p95",
            "trunc%",
        ],
        [22, 7, 7, 7, 7, 9, 9, 9, 7],
        [
            [
                r["tool"],
                r["calls"],
                r["dur_p50"],
                r["dur_p95"],
                r["dur_p99"],
                ms_to_human(r["dur_max"]),
                r["chars_p50"],
                r["chars_p95"],
                "{:.1f}".format(r["truncated_pct"]),
            ]
            for r in data
        ],
    )


def _render_csv_latency(data: List[Dict], w: csv.writer) -> None:
    """Render latency section for CSV output."""
    w.writerow(["## latency"])
    w.writerow(
        [
            "tool",
            "calls",
            "dur_p50",
            "dur_p95",
            "dur_p99",
            "dur_max_ms",
            "chars_p50",
            "chars_p95",
            "chars_max",
            "truncated_pct",
        ]
    )
    for r in data:
        w.writerow(
            [
                r["tool"],
                r["calls"],
                r["dur_p50"],
                r["dur_p95"],
                r["dur_p99"],
                r["dur_max"],
                r["chars_p50"],
                r["chars_p95"],
                r["chars_max"],
                "{:.2f}".format(r["truncated_pct"]),
            ]
        )


def _render_text_reliability(lines: List[str], data: List[Dict]) -> None:
    """Render reliability section for text output."""
    _section(
        lines,
        "2. Reliability  [OTel: error.type | Signals: success_rate, exit!=0, timed_out]",
    )
    _table(
        lines,
        [
            "tool",
            "calls",
            "success%",
            "error%",
            "errors",
            "exit!=0",
            "exit!=0%",
            "timedout",
        ],
        [22, 7, 9, 7, 7, 7, 9, 8],
        [
            [
                r["tool"],
                r["calls"],
                "{:.1f}".format(r["success_rate"]),
                "{:.1f}".format(r["error_rate"]),
                r["errors"],
                r["exit_nonzero"],
                "{:.1f}".format(r["exit_nonzero_pct"]),
                r["timed_out"],
            ]
            for r in data
        ],
    )
    # Error type breakdown (non-zero only)
    all_etypes = defaultdict(int)
    for r in data:
        for et, cnt in r.get("error_types", {}).items():
            all_etypes[et] += cnt
    if all_etypes:
        lines.append("")
        lines.append("  Error type distribution:")
        for et, cnt in sorted(all_etypes.items(), key=lambda x: -x[1]):
            lines.append("    {:30s}  {:>5}".format(et, cnt))


def _render_csv_reliability(data: List[Dict], w: csv.writer) -> None:
    """Render reliability section for CSV output."""
    w.writerow([])
    w.writerow(["## reliability"])
    w.writerow(
        [
            "tool",
            "calls",
            "success_rate",
            "error_rate",
            "errors",
            "exit_nonzero",
            "exit_nonzero_pct",
            "timed_out",
            "timed_out_pct",
        ]
    )
    for r in data:
        w.writerow(
            [
                r["tool"],
                r["calls"],
                "{:.2f}".format(r["success_rate"]),
                "{:.2f}".format(r["error_rate"]),
                r["errors"],
                r["exit_nonzero"],
                "{:.2f}".format(r["exit_nonzero_pct"]),
                r["timed_out"],
                "{:.2f}".format(r["timed_out_pct"]),
            ]
        )


def _render_text_cache(lines: List[str], data: Dict) -> None:
    """Render cache section for text output."""
    _section(lines, "3. Cache Performance  [hit_rate, latency_savings, total_ms_saved]")
    ch = data
    # Only render summary lines if the fields are present
    if "overall_hit_rate" in ch and "total_hits" in ch and "total_cacheable" in ch:
        lines.append(
            "  Overall hit rate : {:.1f}%  ({} hits / {} cacheable calls)".format(
                ch["overall_hit_rate"], ch["total_hits"], ch["total_cacheable"]
            )
        )
    if "total_ms_saved" in ch and "total_hit_chars" in ch:
        lines.append(
            "  Est. wall-clock saved : {}  ({:,} chars served from cache)".format(
                ms_to_human(ch["total_ms_saved"]), ch["total_hit_chars"]
            )
        )
    if ch.get("write_failures"):
        lines.append(
            "  Cache write failures  : {}  ({:.2f}%)".format(
                ch["write_failures"], ch["write_failure_rate"]
            )
        )
    if ch.get("per_tier"):
        tier_str = "  ".join(
            "{}: {}".format(t["tier"], t["hits"]) for t in ch["per_tier"]
        )
        lines.append("  Tier breakdown        : {}".format(tier_str))
    lines.append("")
    _table(
        lines,
        [
            "tool",
            "cacheable",
            "hits",
            "hit%",
            "hit_ms",
            "miss_ms",
            "saved/hit",
            "total_saved",
        ],
        [22, 10, 6, 6, 8, 8, 10, 12],
        [
            [
                r["tool"],
                r["cacheable"],
                r["hits"],
                "{:.1f}".format(r["hit_rate"]),
                r["hit_dur_median"] if r["hit_dur_median"] is not None else "",
                r["miss_dur_median"] if r["miss_dur_median"] is not None else "",
                r["ms_saved_per_hit"],
                r["total_ms_saved"],
            ]
            for r in data["per_tool"]
        ],
    )
    lines.append("")
    lines.append("  Total ms saved by cache: {}".format(data["total_ms_saved"]))


def _render_csv_cache(data: Dict, w: csv.writer) -> None:
    """Render cache section for CSV output."""
    w.writerow([])
    w.writerow(["## cache"])
    w.writerow(
        [
            "tool",
            "cacheable",
            "hits",
            "hit_rate",
            "hit_dur_median_ms",
            "miss_dur_median_ms",
            "ms_saved_per_hit",
            "total_ms_saved",
            "hit_chars",
        ]
    )
    for r in data["per_tool"]:
        w.writerow(
            [
                r["tool"],
                r["cacheable"],
                r["hits"],
                "{:.2f}".format(r["hit_rate"]),
                r["hit_dur_median"] if r["hit_dur_median"] is not None else "",
                r["miss_dur_median"] if r["miss_dur_median"] is not None else "",
                r["ms_saved_per_hit"],
                r["total_ms_saved"],
                r["hit_chars"],
            ]
        )
    # Add summary rows if present
    w.writerow([])
    if "overall_hit_rate" in data:
        w.writerow(["overall_hit_rate", "{:.2f}".format(data["overall_hit_rate"])])
    if "total_cacheable" in data:
        w.writerow(["total_cacheable", data["total_cacheable"]])
    if "total_hits" in data:
        w.writerow(["total_hits", data["total_hits"]])
    if "total_hit_chars" in data:
        w.writerow(["total_hit_chars", data["total_hit_chars"]])
    if "total_ms_saved" in data:
        w.writerow(["total_ms_saved", data["total_ms_saved"]])
    if "write_failures" in data:
        w.writerow(["write_failures", data["write_failures"]])
    if "write_failure_rate" in data:
        w.writerow(["write_failure_rate", "{:.2f}".format(data["write_failure_rate"])])
    if data.get("per_tier"):
        w.writerow(["per_tier"])
        for t in data["per_tier"]:
            w.writerow(["  " + t["tier"], t["hits"]])


def _render_text_outliers(lines: List[str], data: Dict) -> None:
    """Render outliers section for text output."""
    _section(lines, "4. Outliers (Slowest Calls)  [top 10 by duration_ms]")
    _table(
        lines,
        ["duration_ms", "tool", "session_id", "seq", "timed_out", "exit_code"],
        [12, 22, 36, 6, 10, 9],
        [
            [
                r["duration_ms"],
                r["tool"],
                r["session_id"],
                r["seq"] if r["seq"] is not None else "",
                r["timed_out"],
                r["exit_code"] if r["exit_code"] is not None else "",
            ]
            for r in data["slowest_calls"]
        ],
    )


def _render_csv_outliers(data: Dict, w: csv.writer) -> None:
    """Render outliers section for CSV output."""
    w.writerow([])
    w.writerow(["## outliers"])
    w.writerow(["duration_ms", "tool", "session_id", "seq", "timed_out", "exit_code"])
    for r in data["slowest_calls"]:
        w.writerow(
            [
                r["duration_ms"],
                r["tool"],
                r["session_id"],
                r["seq"] if r["seq"] is not None else "",
                r["timed_out"],
                r["exit_code"] if r["exit_code"] is not None else "",
            ]
        )


def _render_text_trend(lines: List[str], data: List[Dict]) -> None:
    """Render trend section for text output."""
    _section(lines, "5. Trend (Daily Breakdown)")
    _table(
        lines,
        [
            "day",
            "calls",
            "success%",
            "error%",
            "cache%",
            "exec_nz%",
            "p95",
            "p99",
            "chars_p95",
        ],
        [12, 7, 9, 7, 7, 9, 7, 7, 10],
        [
            [
                r["day"],
                r["calls"],
                "{:.1f}".format(r["success_rate"]),
                "{:.1f}".format(r["error_rate"]),
                "{:.1f}".format(r["cache_hit_rate"])
                if r["cache_hit_rate"] is not None
                else "",
                "{:.1f}".format(r["exec_nonzero_pct"]),
                r["dur_p95"],
                r["dur_p99"],
                r["chars_p95"],
            ]
            for r in data
        ],
    )


def _render_csv_trend(data: List[Dict], w: csv.writer) -> None:
    """Render trend section for CSV output."""
    w.writerow([])
    w.writerow(["## trend"])
    w.writerow(
        [
            "day",
            "calls",
            "success_rate",
            "error_rate",
            "cache_hit_rate",
            "exec_nonzero_pct",
            "dur_p95",
            "dur_p99",
            "chars_p95",
        ]
    )
    for r in data:
        w.writerow(
            [
                r["day"],
                r["calls"],
                "{:.2f}".format(r["success_rate"]),
                "{:.2f}".format(r["error_rate"]),
                "{:.2f}".format(r["cache_hit_rate"])
                if r["cache_hit_rate"] is not None
                else "",
                "{:.2f}".format(r["exec_nonzero_pct"]),
                r["dur_p95"],
                r["dur_p99"],
                r["chars_p95"],
            ]
        )


def _render_text_params_usage(lines: List[str], data: Dict) -> None:
    """Render params_usage section for text output."""
    _section(lines, "6. Parameter Usage  [per-tool bool-field breakdown]")
    bool_fields = [
        "summary_mode",
        "fields_projected",
        "working_dir_used",
        "stdin_provided",
    ]
    headers = ["tool"] + bool_fields
    widths = [22] + [14] * len(bool_fields)
    rows_data = []
    for tool in sorted(data):
        t = data[tool]
        row = [tool]
        for f in bool_fields:
            cnts = t.get(f)
            if cnts and cnts["present"]:
                row.append("{}/{}".format(cnts["true"], cnts["present"]))
            else:
                row.append("")
        rows_data.append(row)
    _table(lines, headers, widths, rows_data)


def _render_csv_params_usage(data: Dict, w: csv.writer) -> None:
    """Render params_usage section for CSV output."""
    w.writerow([])
    w.writerow(["# Section 6: Parameter Usage"])
    bool_fields = [
        "summary_mode",
        "fields_projected",
        "working_dir_used",
        "stdin_provided",
    ]
    w.writerow(["tool"] + bool_fields)
    for tool in sorted(data):
        t = data[tool]
        row = [tool]
        for f in bool_fields:
            cnts = t.get(f)
            if cnts and cnts["present"]:
                row.append("{}/{}".format(cnts["true"], cnts["present"]))
            else:
                row.append("")
        w.writerow(row)


def _render_text_pagination(lines: List[str], data: Dict) -> None:
    """Render pagination section for text output."""
    _section(
        lines,
        "7. Pagination Adoption  [cursor-based vs summary-mode vs first-page-only]",
    )
    _table(
        lines,
        ["mode", "calls", "pct"],
        [22, 7, 6],
        [
            [
                "paginated (cursor)",
                data["paginated"],
                "{:.1f}".format(pct(data["paginated"], data["total"])),
            ],
            [
                "summary-mode",
                data["summary_mode"],
                "{:.1f}".format(pct(data["summary_mode"], data["total"])),
            ],
            [
                "first-page-only",
                data["first_page_only"],
                "{:.1f}".format(pct(data["first_page_only"], data["total"])),
            ],
        ],
    )


def _render_csv_pagination(data: Dict, w: csv.writer) -> None:
    """Render pagination section for CSV output."""
    w.writerow([])
    w.writerow(["# Section 7: Pagination Adoption"])
    w.writerow(["mode", "calls", "pct"])
    w.writerow(
        [
            "paginated (cursor)",
            data["paginated"],
            "{:.1f}".format(pct(data["paginated"], data["total"])),
        ]
    )
    w.writerow(
        [
            "summary-mode",
            data["summary_mode"],
            "{:.1f}".format(pct(data["summary_mode"], data["total"])),
        ]
    )
    w.writerow(
        [
            "first-page-only",
            data["first_page_only"],
            "{:.1f}".format(pct(data["first_page_only"], data["total"])),
        ]
    )


def _render_text_features(lines: List[str], data: Dict) -> None:
    """Render features section for text output."""
    _section(
        lines,
        "8. Feature Adoption (analyze_symbol)  "
        "[import_lookup, def_use, impl_only, match_mode]",
    )
    lines.append("  Total analyze_symbol calls: {}".format(data["total"]))
    lines.append("")
    bool_fields = ["import_lookup", "def_use", "impl_only"]
    _table(
        lines,
        ["field", "true", "false"],
        [22, 6, 6],
        [
            [
                f,
                data["bool_fields"][f]["true"],
                data["bool_fields"][f]["false"],
            ]
            for f in bool_fields
        ],
    )
    lines.append("")
    lines.append("  match_mode distribution:")
    for mm, cnt in sorted(data["match_mode"].items(), key=lambda x: -x[1]):
        lines.append("    {:20s}  {:>5}".format(mm, cnt))


def _render_csv_features(data: Dict, w: csv.writer) -> None:
    """Render features section for CSV output."""
    w.writerow([])
    w.writerow(["# Section 8: Feature Adoption (analyze_symbol)"])
    w.writerow(["total_analyze_symbol_calls", data["total"]])
    bool_fields = ["import_lookup", "def_use", "impl_only"]
    w.writerow(["field", "true", "false"])
    for f in bool_fields:
        w.writerow(
            [
                f,
                data["bool_fields"][f]["true"],
                data["bool_fields"][f]["false"],
            ]
        )
    for mm, cnt in sorted(data["match_mode"].items(), key=lambda x: -x[1]):
        w.writerow(["match_mode:{}".format(mm), cnt])


def _render_text_git_ref(lines: List[str], data: Dict) -> None:
    """Render git_ref section for text output."""
    _section(lines, "9. git_ref Adoption  [analyze_directory + analyze_symbol]")
    _table(
        lines,
        ["tool", "calls", "git_ref_used", "adoption%"],
        [22, 7, 12, 10],
        [
            [
                tool,
                t["calls"],
                t["git_ref_used"],
                "{:.1f}".format(pct(t["git_ref_used"], t["calls"])),
            ]
            for tool, t in sorted(data.items())
        ],
    )


def _render_csv_git_ref(data: Dict, w: csv.writer) -> None:
    """Render git_ref section for CSV output."""
    w.writerow([])
    w.writerow(["# Section 9: git_ref Adoption"])
    w.writerow(["tool", "calls", "git_ref_used", "adoption_pct"])
    for tool, t in sorted(data.items()):
        w.writerow(
            [
                tool,
                t["calls"],
                t["git_ref_used"],
                "{:.1f}".format(pct(t["git_ref_used"], t["calls"])),
            ]
        )


def _render_text_timeout(lines: List[str], data: Dict) -> None:
    """Render timeout section for text output."""
    _section(
        lines,
        "10. exec_command Timeout Configuration  "
        "[timeout_configured_ms + drain_timeout_ms]",
    )
    lines.append("  Total exec_command calls: {}".format(data["total"]))
    lines.append("")
    _table(
        lines,
        ["bucket", "calls", "pct"],
        [16, 7, 6],
        [
            [bucket, cnt, "{:.1f}".format(pct(cnt, data["total"]))]
            for bucket, cnt in sorted(data["timeout_buckets"].items())
        ],
    )
    lines.append("")
    lines.append(
        "  drain_timeout_ms configured: {} / {} ({:.1f}%)".format(
            data["drain_configured"],
            data["total"],
            pct(data["drain_configured"], data["total"]),
        )
    )


def _render_csv_timeout(data: Dict, w: csv.writer) -> None:
    """Render timeout section for CSV output."""
    w.writerow([])
    w.writerow(["# Section 10: exec_command Timeout Configuration"])
    w.writerow(["total_exec_command_calls", data["total"]])
    w.writerow(["bucket", "calls", "pct"])
    for bucket, cnt in sorted(data["timeout_buckets"].items()):
        w.writerow([bucket, cnt, "{:.1f}".format(pct(cnt, data["total"]))])
    w.writerow(["drain_timeout_ms_configured", data["drain_configured"]])


# Section registry in emit order
SECTIONS = [
    SectionSpec(
        "latency", "1. Latency & Output Size", _render_text_latency, _render_csv_latency
    ),
    SectionSpec(
        "reliability",
        "2. Reliability",
        _render_text_reliability,
        _render_csv_reliability,
    ),
    SectionSpec("cache", "3. Cache Performance", _render_text_cache, _render_csv_cache),
    SectionSpec("outliers", "4. Outliers", _render_text_outliers, _render_csv_outliers),
    SectionSpec("trend", "5. Trend", _render_text_trend, _render_csv_trend),
    SectionSpec(
        "params_usage",
        "6. Parameter Usage",
        _render_text_params_usage,
        _render_csv_params_usage,
    ),
    SectionSpec(
        "pagination", "7. Pagination", _render_text_pagination, _render_csv_pagination
    ),
    SectionSpec("features", "8. Features", _render_text_features, _render_csv_features),
    SectionSpec("git_ref", "9. git_ref", _render_text_git_ref, _render_csv_git_ref),
    SectionSpec("timeout", "10. Timeout", _render_text_timeout, _render_csv_timeout),
]


# ---------------------------------------------------------------------------
# Text formatter
# ---------------------------------------------------------------------------


def fmt_text(
    latency,
    reliability,
    cache,
    outliers,
    trend,
    show_trend,
    params_usage=None,
    pagination=None,
    features=None,
    git_ref=None,
    timeout=None,
):
    lines = []

    # Build data dict for section rendering
    section_data = {
        "latency": latency,
        "reliability": reliability,
        "cache": cache,
        "outliers": outliers,
        "trend": trend,
        "params_usage": params_usage,
        "pagination": pagination,
        "features": features,
        "git_ref": git_ref,
        "timeout": timeout,
    }

    # Render sections in order
    for spec in SECTIONS:
        data = section_data[spec.key]

        # Guard conditions
        if spec.key == "trend" and not show_trend:
            continue
        if spec.key == "trend" and not data:
            continue
        if (
            spec.key in ("params_usage", "pagination", "features", "git_ref", "timeout")
            and data is None
        ):
            continue

        spec.render_text(lines, data)

    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# JSON formatter
# ---------------------------------------------------------------------------


def fmt_json(
    latency,
    reliability,
    cache,
    outliers,
    trend,
    show_trend,
    params_usage=None,
    pagination=None,
    features=None,
    git_ref=None,
    timeout=None,
):
    data = {
        "latency": latency,
        "reliability": reliability,
        "cache": cache,
        "outliers": outliers,
    }
    if show_trend:
        data["trend"] = trend
    if params_usage is not None:
        data["params_usage"] = params_usage
    if pagination is not None:
        data["pagination"] = pagination
    if features is not None:
        data["features"] = features
    if git_ref is not None:
        data["git_ref"] = git_ref
    if timeout is not None:
        data["timeout"] = timeout
    return json.dumps(data, indent=2)


# ---------------------------------------------------------------------------
# CSV formatter
# ---------------------------------------------------------------------------


def fmt_csv(
    latency,
    reliability,
    cache,
    outliers,
    trend,
    show_trend,
    params_usage=None,
    pagination=None,
    features=None,
    git_ref=None,
    timeout=None,
):
    buf = StringIO()
    w = csv.writer(buf)

    # Build data dict for section rendering
    section_data = {
        "latency": latency,
        "reliability": reliability,
        "cache": cache,
        "outliers": outliers,
        "trend": trend,
        "params_usage": params_usage,
        "pagination": pagination,
        "features": features,
        "git_ref": git_ref,
        "timeout": timeout,
    }

    # Render sections in order
    for i, spec in enumerate(SECTIONS):
        data = section_data[spec.key]

        # Guard conditions
        if spec.key == "trend" and not show_trend:
            continue
        if spec.key == "trend" and not data:
            continue
        if (
            spec.key in ("params_usage", "pagination", "features", "git_ref", "timeout")
            and data is None
        ):
            continue

        # Add blank separator row before non-first sections
        if i > 0 and spec.key != "trend":
            w.writerow([])

        spec.render_csv(data, w)

    return buf.getvalue()


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(
        description="MCP tool-call observability for aptu-coder.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  python scripts/mcp-metrics.py                        # full summary
  python scripts/mcp-metrics.py --trend                # + daily breakdown
  python scripts/mcp-metrics.py --tool exec_command    # single tool
  python scripts/mcp-metrics.py --from 2026-05-01      # date filter
  python scripts/mcp-metrics.py --format json | jq .   # machine-readable
  python scripts/mcp-metrics.py --format csv           # spreadsheet export
  python scripts/mcp-metrics.py --all-tools            # include legacy tools
        """,
    )
    parser.add_argument(
        "--metrics-dir",
        default=default_metrics_dir(),
        help="Path to metrics directory (default: $XDG_DATA_HOME/aptu-coder)",
    )
    parser.add_argument(
        "--from",
        dest="from_date",
        type=parse_date_arg,
        help="Start date (YYYY-MM-DD)",
    )
    parser.add_argument(
        "--to",
        dest="to_date",
        type=parse_date_arg,
        help="End date (YYYY-MM-DD)",
    )
    parser.add_argument(
        "--tool",
        dest="tool_filter",
        help="Filter to single tool",
    )
    parser.add_argument(
        "--trend",
        action="store_true",
        help="Include daily trend breakdown",
    )
    parser.add_argument(
        "--format",
        choices=["text", "json", "csv"],
        default="text",
        help="Output format (default: text)",
    )
    parser.add_argument(
        "--all-tools",
        action="store_true",
        help="Include legacy/renamed tools",
    )

    args = parser.parse_args()

    records = load_records(
        args.metrics_dir,
        from_date=args.from_date,
        to_date=args.to_date,
        tool_filter=args.tool_filter,
        all_tools=args.all_tools,
    )

    if not records:
        print("No records found.", file=sys.stderr)
        sys.exit(1)

    # Enrich records with file_date for trend computation
    for rec in records:
        path = rec.get("_path", "")
        fdate = file_date(path)
        if fdate:
            rec["file_date"] = fdate

    latency = compute_latency(records)
    reliability = compute_reliability(records)
    cache = compute_cache(records)
    outliers = compute_outliers(records)
    trend = compute_trend(records) if args.trend else []
    params_usage = compute_params_usage(records)
    pagination = compute_pagination(records)
    features = compute_features(records)
    git_ref = compute_git_ref(records)
    timeout = compute_timeout(records)

    if args.format == "json":
        output = fmt_json(
            latency,
            reliability,
            cache,
            outliers,
            trend,
            args.trend,
            params_usage=params_usage,
            pagination=pagination,
            features=features,
            git_ref=git_ref,
            timeout=timeout,
        )
    elif args.format == "csv":
        output = fmt_csv(
            latency,
            reliability,
            cache,
            outliers,
            trend,
            args.trend,
            params_usage=params_usage,
            pagination=pagination,
            features=features,
            git_ref=git_ref,
            timeout=timeout,
        )
    else:
        output = fmt_text(
            latency,
            reliability,
            cache,
            outliers,
            trend,
            args.trend,
            params_usage=params_usage,
            pagination=pagination,
            features=features,
            git_ref=git_ref,
            timeout=timeout,
        )

    print(output)


if __name__ == "__main__":
    main()
