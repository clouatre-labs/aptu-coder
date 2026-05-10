# Roadmap

This document captures planned work and its rationale. Issues are the authoritative source of truth for scope and status; this file provides the strategic narrative.

## Active milestones

### openssf-gold

Track criteria needed to achieve the OpenSSF Best Practices Gold badge (project 12275). See milestone for individual issues.

---

## Completed milestones

### [Complete] observability-v1

All issues (#820, #821, #822, #823, #824) merged. See [docs/ROADMAP.md](docs/ROADMAP.md) for the full wave history entry.

Key deliverables shipped:
- GenAI semantic convention attributes on all 7 tool handlers (#821)
- `tracing-opentelemetry` bridge with conditional OTLP export (#822)
- Log-trace correlation, W3C Trace Context extraction, child spans for sub-operations (#823)
- JSONL metrics retain daily-rotating local trail; OTel Metrics SDK initialized in parallel when endpoint is set (#823)
- Span attribute policy and never-record list documented in [OBSERVABILITY.md](OBSERVABILITY.md) (#820)
- Observability documentation updated in [docs/OBSERVABILITY.md](docs/OBSERVABILITY.md) (#824)

**Spec reference:** [OpenTelemetry GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/) (Development status, May 2026), [MCP-specific OTel conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/mcp/).

### [Complete] wave-9

Editing tools completing the read-analyze-write loop. See [docs/ROADMAP.md](docs/ROADMAP.md) for the full wave history entry. `edit_rename` and `edit_insert` were subsequently removed (#779); `edit_overwrite` and `edit_replace` remain.

---

## Recent completions

- **Project rename** (#826): `code-analyze-mcp` renamed to `aptu-coder` across all source, docs, and benchmark tooling. Env vars `CODE_ANALYZE_DIR_CACHE_CAPACITY` and `CODE_ANALYZE_FILE_CACHE_CAPACITY` renamed to `APTU_CODER_DIR_CACHE_CAPACITY` and `APTU_CODER_FILE_CACHE_CAPACITY`. `migrate_legacy_metrics_dir()` handles XDG path migration at runtime for existing users.
- **Fortran handler** (#828): Complete handler with module extraction, subroutine/function name extraction, Fortran 2003+ OOP bound procedure calls (`obj%method()`), and call graph traversal. Registers `extract_function_name`, `find_receiver_type`, `find_method_for_receiver` in LanguageInfo. Validated against OpenFAST source tree (v13 benchmark).

## Backlog

### Language support

- Swift grammar support (issue #648, good first issue)

### Tooling

- Remove `analyze_module` pending usage data review (issue #780)

### Benchmarks

- Go interface dispatch analysis (issue #661)
- `analyze_symbol` def-use micro-benchmark (issue #660)
