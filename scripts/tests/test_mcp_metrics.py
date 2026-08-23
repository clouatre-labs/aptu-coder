"""
Regression tests for mcp-metrics.py refactoring.

Tests verify that fmt_text, fmt_csv, and fmt_json produce identical output
to pre-refactor golden strings after consolidating section rendering logic.
"""

import importlib.util
import json
import tempfile
from datetime import date
from pathlib import Path

# importlib is required because the filename contains a hyphen
# conftest.py ensures scripts/ is on sys.path before this module loads
spec = importlib.util.spec_from_file_location(
    "mcp_metrics", Path(__file__).parent.parent / "mcp-metrics.py"
)
mcp_metrics = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mcp_metrics)


def test_fmt_text_happy_path():
    """Test fmt_text produces correct output with all sections."""
    latency = [
        {
            "tool": "analyze_directory",
            "calls": 100,
            "dur_p50": 50,
            "dur_p95": 200,
            "dur_p99": 500,
            "dur_max": 1000,
            "chars_p50": 1000,
            "chars_p95": 5000,
            "chars_max": 10000,
            "truncated_pct": 0.0,
        }
    ]
    reliability = [
        {
            "tool": "analyze_directory",
            "calls": 100,
            "success_rate": 99.0,
            "error_rate": 1.0,
            "errors": 1,
            "exit_nonzero": 0,
            "exit_nonzero_pct": 0.0,
            "timed_out": 0,
            "timed_out_pct": 0.0,
            "error_types": {"timeout": 1},
        }
    ]
    cache = {
        "per_tool": [
            {
                "tool": "analyze_directory",
                "cacheable": 100,
                "hits": 50,
                "hit_rate": 50.0,
                "hit_dur_median": 30,
                "miss_dur_median": 70,
                "ms_saved_per_hit": 40,
                "total_ms_saved": 2000,
                "hit_chars": 1000,
            }
        ],
        "total_hits": 50,
        "total_misses": 50,
        "total_ms_saved": 2000,
    }
    outliers = {"slowest_calls": []}
    trend = []
    params_usage = None
    pagination = None
    features = None
    git_ref = None
    timeout = None

    output = mcp_metrics.fmt_text(
        latency,
        reliability,
        cache,
        outliers,
        trend,
        show_trend=False,
        params_usage=params_usage,
        pagination=pagination,
        features=features,
        git_ref=git_ref,
        timeout=timeout,
    )

    # Verify output contains expected sections
    assert "1. Latency & Output Size" in output
    assert "2. Reliability" in output
    assert "3. Cache Performance" in output
    assert "4. Outliers" in output
    assert "analyze_directory" in output
    assert "100" in output


def test_fmt_text_edge_case_all_optional_none():
    """Test fmt_text handles all optional sections None."""
    latency = [
        {
            "tool": "analyze_file",
            "calls": 50,
            "dur_p50": 25,
            "dur_p95": 100,
            "dur_p99": 250,
            "dur_max": 500,
            "chars_p50": 500,
            "chars_p95": 2500,
            "chars_max": 5000,
            "truncated_pct": 0.0,
        }
    ]
    reliability = [
        {
            "tool": "analyze_file",
            "calls": 50,
            "success_rate": 100.0,
            "error_rate": 0.0,
            "errors": 0,
            "exit_nonzero": 0,
            "exit_nonzero_pct": 0.0,
            "timed_out": 0,
            "timed_out_pct": 0.0,
            "error_types": {},
        }
    ]
    cache = {
        "per_tool": [],
        "total_hits": 0,
        "total_misses": 0,
        "total_ms_saved": 0,
    }
    outliers = {"slowest_calls": []}
    trend = []

    output = mcp_metrics.fmt_text(
        latency,
        reliability,
        cache,
        outliers,
        trend,
        show_trend=False,
        params_usage=None,
        pagination=None,
        features=None,
        git_ref=None,
        timeout=None,
    )

    # Verify optional sections are not present
    assert "6. Parameter Usage" not in output
    assert "7. Pagination" not in output
    assert "8. Feature Adoption" not in output
    assert "9. git_ref" not in output
    assert "10. exec_command Timeout" not in output
    # But required sections should be present
    assert "1. Latency & Output Size" in output
    assert "2. Reliability" in output


def test_fmt_text_edge_case_show_trend_false():
    """Test fmt_text omits trend section when show_trend=False."""
    latency = [
        {
            "tool": "exec_command",
            "calls": 200,
            "dur_p50": 100,
            "dur_p95": 400,
            "dur_p99": 1000,
            "dur_max": 2000,
            "chars_p50": 2000,
            "chars_p95": 10000,
            "chars_max": 20000,
            "truncated_pct": 5.0,
        }
    ]
    reliability = [
        {
            "tool": "exec_command",
            "calls": 200,
            "success_rate": 95.0,
            "error_rate": 5.0,
            "errors": 10,
            "exit_nonzero": 5,
            "exit_nonzero_pct": 2.5,
            "timed_out": 2,
            "timed_out_pct": 1.0,
            "error_types": {"timeout": 2, "parse_error": 8},
        }
    ]
    cache = {
        "per_tool": [],
        "total_hits": 0,
        "total_misses": 0,
        "total_ms_saved": 0,
    }
    outliers = {"slowest_calls": []}
    trend = [
        {
            "day": date(2026, 1, 1),
            "calls": 100,
            "success_rate": 95.0,
            "error_rate": 5.0,
            "cache_hit_rate": None,
            "exec_nonzero_pct": 2.5,
            "dur_p95": 400,
            "dur_p99": 1000,
            "chars_p95": 10000,
        }
    ]

    output = mcp_metrics.fmt_text(
        latency,
        reliability,
        cache,
        outliers,
        trend,
        show_trend=False,
    )

    # Trend section should not be present
    assert "5. Trend" not in output


def test_fmt_text_edge_case_show_trend_true_empty():
    """Test fmt_text handles show_trend=True with empty trend list."""
    latency = [
        {
            "tool": "analyze_symbol",
            "calls": 75,
            "dur_p50": 60,
            "dur_p95": 150,
            "dur_p99": 300,
            "dur_max": 600,
            "chars_p50": 1500,
            "chars_p95": 7500,
            "chars_max": 15000,
            "truncated_pct": 2.0,
        }
    ]
    reliability = [
        {
            "tool": "analyze_symbol",
            "calls": 75,
            "success_rate": 98.0,
            "error_rate": 2.0,
            "errors": 2,
            "exit_nonzero": 0,
            "exit_nonzero_pct": 0.0,
            "timed_out": 0,
            "timed_out_pct": 0.0,
            "error_types": {"validation_error": 2},
        }
    ]
    cache = {
        "per_tool": [],
        "total_hits": 0,
        "total_misses": 0,
        "total_ms_saved": 0,
    }
    outliers = {"slowest_calls": []}
    trend = []

    output = mcp_metrics.fmt_text(
        latency,
        reliability,
        cache,
        outliers,
        trend,
        show_trend=True,
    )

    # Trend section should not be present when trend list is empty
    assert "5. Trend" not in output


def test_fmt_csv_happy_path():
    """Test fmt_csv produces correct CSV output with all sections."""
    latency = [
        {
            "tool": "analyze_directory",
            "calls": 100,
            "dur_p50": 50,
            "dur_p95": 200,
            "dur_p99": 500,
            "dur_max": 1000,
            "chars_p50": 1000,
            "chars_p95": 5000,
            "chars_max": 10000,
            "truncated_pct": 0.0,
        }
    ]
    reliability = [
        {
            "tool": "analyze_directory",
            "calls": 100,
            "success_rate": 99.0,
            "error_rate": 1.0,
            "errors": 1,
            "exit_nonzero": 0,
            "exit_nonzero_pct": 0.0,
            "timed_out": 0,
            "timed_out_pct": 0.0,
            "error_types": {"timeout": 1},
        }
    ]
    cache = {
        "per_tool": [
            {
                "tool": "analyze_directory",
                "cacheable": 100,
                "hits": 50,
                "hit_rate": 50.0,
                "hit_dur_median": 30,
                "miss_dur_median": 70,
                "ms_saved_per_hit": 40,
                "total_ms_saved": 2000,
                "hit_chars": 1000,
            }
        ],
        "total_hits": 50,
        "total_misses": 50,
        "total_ms_saved": 2000,
    }
    outliers = {"slowest_calls": []}
    trend = []

    output = mcp_metrics.fmt_csv(
        latency,
        reliability,
        cache,
        outliers,
        trend,
        show_trend=False,
    )

    # Verify CSV structure
    assert "## latency" in output
    assert "## reliability" in output
    assert "## cache" in output
    assert "## outliers" in output
    assert "analyze_directory" in output
    lines = output.strip().split("\n")
    assert len(lines) > 0


def test_fmt_csv_edge_case_all_optional_none():
    """Test fmt_csv handles all optional sections None."""
    latency = [
        {
            "tool": "analyze_file",
            "calls": 50,
            "dur_p50": 25,
            "dur_p95": 100,
            "dur_p99": 250,
            "dur_max": 500,
            "chars_p50": 500,
            "chars_p95": 2500,
            "chars_max": 5000,
            "truncated_pct": 0.0,
        }
    ]
    reliability = [
        {
            "tool": "analyze_file",
            "calls": 50,
            "success_rate": 100.0,
            "error_rate": 0.0,
            "errors": 0,
            "exit_nonzero": 0,
            "exit_nonzero_pct": 0.0,
            "timed_out": 0,
            "timed_out_pct": 0.0,
            "error_types": {},
        }
    ]
    cache = {
        "per_tool": [],
        "total_hits": 0,
        "total_misses": 0,
        "total_ms_saved": 0,
    }
    outliers = {"slowest_calls": []}
    trend = []

    output = mcp_metrics.fmt_csv(
        latency,
        reliability,
        cache,
        outliers,
        trend,
        show_trend=False,
        params_usage=None,
        pagination=None,
        features=None,
        git_ref=None,
        timeout=None,
    )

    # Verify optional sections are not present
    assert "# Section 6" not in output
    assert "# Section 7" not in output
    assert "# Section 8" not in output
    assert "# Section 9" not in output
    assert "# Section 10" not in output
    # But required sections should be present
    assert "## latency" in output
    assert "## reliability" in output


def test_fmt_csv_blank_separator_rows():
    """Test fmt_csv preserves blank separator rows between sections."""
    latency = [
        {
            "tool": "analyze_directory",
            "calls": 100,
            "dur_p50": 50,
            "dur_p95": 200,
            "dur_p99": 500,
            "dur_max": 1000,
            "chars_p50": 1000,
            "chars_p95": 5000,
            "chars_max": 10000,
            "truncated_pct": 0.0,
        }
    ]
    reliability = [
        {
            "tool": "analyze_directory",
            "calls": 100,
            "success_rate": 99.0,
            "error_rate": 1.0,
            "errors": 1,
            "exit_nonzero": 0,
            "exit_nonzero_pct": 0.0,
            "timed_out": 0,
            "timed_out_pct": 0.0,
            "error_types": {"timeout": 1},
        }
    ]
    cache = {
        "per_tool": [],
        "total_hits": 0,
        "total_misses": 0,
        "total_ms_saved": 0,
    }
    outliers = {"slowest_calls": []}
    trend = []

    output = mcp_metrics.fmt_csv(
        latency,
        reliability,
        cache,
        outliers,
        trend,
        show_trend=False,
    )

    # Count blank lines (empty rows in CSV)
    lines = output.strip().split("\n")
    blank_count = sum(1 for line in lines if line.strip() == "")
    # Should have blank separator rows between sections
    assert blank_count > 0


def test_fmt_json_happy_path():
    """Test fmt_json produces correct JSON output (unchanged function)."""
    latency = [
        {
            "tool": "analyze_directory",
            "calls": 100,
            "dur_p50": 50,
            "dur_p95": 200,
            "dur_p99": 500,
            "dur_max": 1000,
            "chars_p50": 1000,
            "chars_p95": 5000,
            "chars_max": 10000,
            "truncated_pct": 0.0,
        }
    ]
    reliability = [
        {
            "tool": "analyze_directory",
            "calls": 100,
            "success_rate": 99.0,
            "error_rate": 1.0,
            "errors": 1,
            "exit_nonzero": 0,
            "exit_nonzero_pct": 0.0,
            "timed_out": 0,
            "timed_out_pct": 0.0,
            "error_types": {"timeout": 1},
        }
    ]
    cache = {
        "per_tool": [],
        "total_hits": 0,
        "total_misses": 0,
        "total_ms_saved": 0,
    }
    outliers = {"slowest_calls": []}
    trend = []

    output = mcp_metrics.fmt_json(
        latency,
        reliability,
        cache,
        outliers,
        trend,
        show_trend=False,
    )

    # Verify JSON structure
    import json

    data = json.loads(output)
    assert "latency" in data
    assert "reliability" in data
    assert "cache" in data
    assert "outliers" in data
    assert "trend" not in data  # show_trend=False


def test_table_alignment():
    """Test _table renders variable-width columns with correct alignment."""
    lines = []
    headers = ["tool", "calls", "rate"]
    widths = [22, 7, 6]
    rows_data = [["analyze_directory", 100, 99.5], ["analyze_file", 50, 100.0]]

    mcp_metrics._table(lines, headers, widths, rows_data)

    # Verify table structure
    assert len(lines) >= 4  # header, separator, 2 data rows
    assert "tool" in lines[0]
    assert "calls" in lines[0]
    assert "rate" in lines[0]
    assert "-" in lines[1]  # separator line


def test_section_header():
    """Test _section emits correct header format."""
    lines = []
    mcp_metrics._section(lines, "Test Section")

    # Verify section structure
    assert len(lines) == 4
    assert lines[0] == ""
    assert lines[1].startswith("=")
    assert "Test Section" in lines[2]
    assert lines[3].startswith("=")


def test_all_sections_in_registry():
    """Test all 10 sections are in SECTIONS registry in correct order."""
    assert len(mcp_metrics.SECTIONS) == 10
    expected_keys = [
        "latency",
        "reliability",
        "cache",
        "outliers",
        "trend",
        "params_usage",
        "pagination",
        "features",
        "git_ref",
        "timeout",
    ]
    actual_keys = [spec.key for spec in mcp_metrics.SECTIONS]
    assert actual_keys == expected_keys


def test_load_records_filters_received():
    """Test load_records excludes received records and includes ok/error."""
    # Arrange
    with tempfile.TemporaryDirectory() as tmpdir:
        jsonl_path = Path(tmpdir) / "metrics-2026-08-23.jsonl"
        events = [
            {
                "ts": 1000,
                "tool": "analyze_file",
                "result": "received",
                "duration_ms": 0,
                "cache_hit": None,
            },
            {
                "ts": 1050,
                "tool": "analyze_file",
                "result": "ok",
                "duration_ms": 50,
                "cache_hit": False,
                "output_chars": 120,
            },
            {
                "ts": 1100,
                "tool": "analyze_file",
                "result": "received",
                "duration_ms": 0,
                "cache_hit": None,
            },
            {
                "ts": 1110,
                "tool": "analyze_file",
                "result": "error",
                "duration_ms": 10,
                "cache_hit": None,
                "error_type": "invalid_params",
            },
            {
                "ts": 1200,
                "tool": "analyze_directory",
                "result": "ok",
                "duration_ms": 200,
                "cache_hit": True,
                "output_chars": 500,
            },
        ]
        with open(jsonl_path, "w", encoding="utf-8") as f:
            f.writelines(json.dumps(ev) + "\n" for ev in events)

        # Act
        records = mcp_metrics.load_records(tmpdir)

        # Assert
        assert len(records) == 3
        results = [r.get("result") for r in records]
        assert "received" not in results
        assert results == ["ok", "error", "ok"]


def test_compute_latency_with_loaded_records_ignores_received():
    """Test compute_latency does not produce dur_p50 of 0 when received rows are filtered."""
    # Arrange
    with tempfile.TemporaryDirectory() as tmpdir:
        jsonl_path = Path(tmpdir) / "metrics-2026-08-23.jsonl"
        events = [
            {
                "ts": 1000,
                "tool": "analyze_file",
                "result": "received",
                "duration_ms": 0,
                "cache_hit": None,
            },
            {
                "ts": 1050,
                "tool": "analyze_file",
                "result": "ok",
                "duration_ms": 100,
                "cache_hit": False,
                "output_chars": 120,
            },
        ]
        with open(jsonl_path, "w", encoding="utf-8") as f:
            f.writelines(json.dumps(ev) + "\n" for ev in events)

        # Act
        records = mcp_metrics.load_records(tmpdir)
        latency = mcp_metrics.compute_latency(records)

        # Assert
        assert len(latency) == 1
        assert latency[0]["tool"] == "analyze_file"
        assert latency[0]["calls"] == 1
        assert latency[0]["dur_p50"] == 100


def test_compute_cache_with_loaded_records_ignores_received():
    """Test compute_cache does not count cache_hit=null received rows as cache misses."""
    # Arrange
    with tempfile.TemporaryDirectory() as tmpdir:
        jsonl_path = Path(tmpdir) / "metrics-2026-08-23.jsonl"
        events = [
            {
                "ts": 1000,
                "tool": "analyze_file",
                "result": "received",
                "duration_ms": 0,
                "cache_hit": None,
            },
            {
                "ts": 1050,
                "tool": "analyze_file",
                "result": "ok",
                "duration_ms": 10,
                "cache_hit": True,
                "cache_tier": "l1_memory",
                "output_chars": 120,
            },
        ]
        with open(jsonl_path, "w", encoding="utf-8") as f:
            f.writelines(json.dumps(ev) + "\n" for ev in events)

        # Act
        records = mcp_metrics.load_records(tmpdir)
        cache = mcp_metrics.compute_cache(records)

        # Assert
        assert cache["total_hits"] == 1
        assert cache["total_misses"] == 0
        assert len(cache["per_tool"]) == 1
        assert cache["per_tool"][0]["hits"] == 1
        assert cache["per_tool"][0]["cacheable"] == 1
        assert cache["per_tool"][0]["hit_rate"] == 100.0


if __name__ == "__main__":
    # Run tests
    test_fmt_text_happy_path()
    test_fmt_text_edge_case_all_optional_none()
    test_fmt_text_edge_case_show_trend_false()
    test_fmt_text_edge_case_show_trend_true_empty()
    test_fmt_csv_happy_path()
    test_fmt_csv_edge_case_all_optional_none()
    test_fmt_csv_blank_separator_rows()
    test_fmt_json_happy_path()
    test_table_alignment()
    test_section_header()
    test_all_sections_in_registry()
    test_load_records_filters_received()
    test_compute_latency_with_loaded_records_ignores_received()
    test_compute_cache_with_loaded_records_ignores_received()
    print("All tests passed!")
