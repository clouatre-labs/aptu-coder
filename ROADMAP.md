# Roadmap

This document captures planned work and its rationale. Issues are the authoritative source of truth for scope and status; this file provides the strategic narrative.

## Active milestones

### observability-v1

**Goal:** Replace benchmark waves as the primary tool for runtime performance investigation. Give contributors and users the ability to answer "what happened and why" without grepping session logs.

**Background:** aptu-coder uses the `tracing` crate throughout, but spans carry zero semantic attributes and are never exported. The custom JSONL metrics system records `duration_ms` and `error_type` per tool call but these are file-local, not correlated with spans or logs, and not queryable across sessions. When a tool call fails or performs unexpectedly in production, the only recourse is to dig through goose or Claude session logs -- a slow, manual process.

**What changes:**

- All 7 tool handlers gain GenAI semantic convention attributes (`gen_ai.operation.name`, `gen_ai.tool.name`, `gen_ai.system`) and key parameters as span fields, so any span is fully self-describing.
- Behavioral decisions (`auto_summary`, `cache_hit`, `truncated`) become span events, making them queryable without touching log files.
- Error recording lands on spans: error type, status, and exception events at every error return path.
- A `tracing-opentelemetry` bridge wires the existing `#[instrument]` spans to OTLP export, gated on `OTEL_EXPORTER_OTLP_ENDPOINT`. When the variable is unset (the default), overhead is zero: spans are created but never exported. When set, a `BatchSpanProcessor` handles export asynchronously with no hot-path latency.
- `opentelemetry-appender-tracing` forwards log events as OTel `LogRecord`s, injecting `trace_id` and `span_id` automatically. Every `info!` and `error!` callsite gains trace correlation without code changes.
- W3C Trace Context is extracted from MCP `params._meta` (`traceparent`, `tracestate`) and used as the parent span context, connecting tool spans to the calling agent's trace (goose session, Claude turn).
- Child spans for sub-operations inside `analyze_symbol` and `analyze_directory` (parse, AST query, call graph traversal, pagination, formatting) expose P95 breakdowns by sub-operation from real traffic.
- JSONL metrics migrate to the OTel Metrics SDK, emitting `mcp.server.operation.duration` (the standard MCP histogram) and per-tool counters. The JSONL writer is retained as a local-dev fallback.

**Performance commitment:** zero overhead when `OTEL_EXPORTER_OTLP_ENDPOINT` is unset. Span creation with a noop provider costs ~6.6 µs; for a tool call taking tens of milliseconds this is irrelevant. All export is asynchronous (BatchSpanProcessor, background thread). Benchmarks must not regress.

**Issues (in dependency order):**

Span attribute policy: [OBSERVABILITY.md](OBSERVABILITY.md). Every PR adding span attributes must comply with the never-record list defined there. The policy issue (#820) is a prerequisite for all attribute enrichment work.

| Issue | Title | Tier | Depends on |
|---|---|---|---|
| #820 | define span attribute policy and never-record list | 0 (prerequisite) | - |
| #812 | enrich tool spans with GenAI semantic attributes and error recording | 1 | #820 |
| #813 | emit behavioral decisions as span events | 1 | #820 |
| #814 | add tracing-opentelemetry bridge with conditional OTLP export | 2 (keystone) | - |
| #815 | add log-trace correlation via opentelemetry-appender-tracing | 2 | #814, #820 |
| #816 | extract W3C Trace Context from MCP params._meta | 2 | #814, #820 |
| #817 | add child spans for sub-operations in analyze_symbol and analyze_directory | 3 | #814, #820 |
| #818 | migrate JSONL metrics to OTel Metrics SDK | 3 | #814 |

**Spec reference:** [OpenTelemetry GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/) (Development status, May 2026), [MCP-specific OTel conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/mcp/).

---

### openssf-gold

Track criteria needed to achieve the OpenSSF Best Practices Gold badge (project 12275). See milestone for individual issues.

### wave-9

Editing tools: five MCP tools (`read_file`, `write_file`, `edit_file`, `rename_symbol`, `insert_at_symbol`) completing the read-analyze-write loop. Phase 1 (mechanical, no AST) then Phase 2 (AST-backed). Closes with a benchmark.

---

## Backlog

### Language support

- Swift grammar support (issue #648, good first issue)

### Tooling

- Remove `analyze_module` pending usage data review (issue #780)

### Benchmarks

- Go interface dispatch analysis (issue #661)
- `analyze_symbol` def-use micro-benchmark (issue #660)
