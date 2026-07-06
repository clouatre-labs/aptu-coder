# Audit: Usage, Observability, and Code Size — July 2026

Date: 2026-07-05
Data: 31 days (2026-06-05 to 2026-07-05), 118,113 JSONL calls
Method: `scripts/mcp-metrics.py`, validated jq one-liners, `analyze_directory` / `analyze_file`
Guard verdict: PASS_WITH_NOTES — all critical claims confirmed

## Purpose

1. Identify unused or unmetricated tool features as candidates for removal or added observability.
2. Identify source files and functions that exceed size thresholds and should be split.

## Metrics Snapshot

*Table 1: Tool call volume and error rates, 31-day window.*

| Tool | Calls | Share | Errors | Error rate | Avg latency |
|---|---:|---:|---:|---:|---:|
| `exec_command` | 99,620 | 84.3% | 181 | 0.2% | 1,435 ms |
| `edit_replace` | 7,208 | 6.1% | 297 | 4.1% | 1 ms |
| `analyze_file` | 4,814 | 4.1% | 247 | 5.1% | 41 ms |
| `edit_overwrite` | 3,458 | 2.9% | 181 | 5.2% | 1 ms |
| `analyze_directory` | 2,023 | 1.7% | 0 | 0.0% | 207 ms |
| `analyze_module` | 767 | 0.6% | 17 | 2.2% | 71 ms |
| `analyze_symbol` | 223 | 0.2% | 0 | 0.0% | 156 ms |

*Table 2: Cache performance, cacheable tools only.*

| Tool | Hit rate | Hits | Cacheable calls |
|---|---:|---:|---:|
| `analyze_file` | 68.1% | 2,162 | 3,174 |
| `analyze_module` | 51.3% | 272 | 530 |
| `analyze_directory` | 44.2% | 666 | 1,506 |
| `analyze_symbol` | 23.0% | 35 | 152 |

Overall cache hit rate: 10.7% (3,135 / 29,322 cacheable calls). L1 in-memory: 554 hits. L2 disk: 2,581 hits. Estimated wall-clock saved: ~6 minutes.

Cache hit rate was below 3% until 2026-06-14, then stabilized in the 40–80% range. The exact trigger commit is not identified here.

---

## Summary

*Table 3: Findings and recommended issue mapping.*

| ID | Severity | Area | Finding |
|---|---|---|---|
| O1 | High | Observability | `analyze_symbol` emits no error metrics |
| O2 | High | Observability | 19 tool parameters not recorded in JSONL |
| O3 | Medium | Observability | Pagination mode invisible across all tools |
| O4 | Medium | Observability | `analyze_symbol` mode not recorded (`import_lookup`, `def_use`, `impl_only`, `match_mode`) |
| O5 | Low | Observability | Cache L1 evictions and L2 size not metricated |
| O6 | Low | Observability | `scripts/mcp-metrics.py` has no per-parameter breakdown |
| S1 | Medium | Code size | `analyze.rs` 2,075 LOC — focused analysis orchestration should be extracted |
| S2 | Medium | Code size | `parser.rs` 1,949 LOC — element extraction should be extracted |
| S3 | Medium | Code size | `tests.rs` 1,886 LOC — monolithic test file, split by domain |
| S4 | Medium | Code size | `metrics.rs` 1,499 LOC — export logic should be extracted |
| S5 | Low | Code size | `exec_command.rs` 998 LOC, `analyze_symbol.rs` 927 LOC, `cache.rs` 976 LOC — each has an extractable subsystem |
| S6 | Low | Code size | `shell_write.rs` 615 LOC — two distinct concerns (heredoc validation, backward scanning) |
| S7 | Low | Code size | `formatter/summary.rs` 780 LOC, `edit_replace.rs` 553 LOC — large functions extractable |

---

## Findings

### O1 — `analyze_symbol` emits no error metrics

**Severity:** High
**File:** `crates/aptu-coder/src/tools/analyze_symbol.rs`
**Guard verdict:** CONFIRMED

All three `metrics_tx.send` calls in `analyze_symbol.rs` use `result="ok"`. Error paths (`invalid_params`, validation failures, analysis errors) record on the tracing span only (`span.record("error", true)`). No `result="error"` entry appears in JSONL across 225 observed calls.

Every other tool (6 of 7) emits `result="error"` with `error_type` on failure paths. `analyze_symbol` does not.

**Fix:** Add `metrics_tx.send` on all error exit paths in `analyze_symbol_handler`, mirroring the pattern in `analyze_file.rs`. Record `error_type` as `"invalid_params"` or `"analysis_error"` to match schema.

**Acceptance criteria:** `analyze_symbol` JSONL records include `result="error"` entries when the tool returns an error response. `cargo test` passes.

---

### O2 — 19 tool parameters not recorded in JSONL

**Severity:** High
**Files:** All tool handlers in `crates/aptu-coder/src/tools/`, `crates/aptu-coder/src/metrics.rs`

The following parameters are accepted but produce no JSONL field. Usage of these features cannot be measured.

*Table 4: Unmetricated parameters.*

| Tool | Parameter | Proposed field |
|---|---|---|
| `analyze_directory` | `git_ref` | `git_ref_used: bool` |
| `analyze_directory` | `summary` | `summary_mode: bool` |
| `analyze_directory` | `cursor` | `is_paginated: bool` |
| `analyze_file` | `fields` | `fields_projected: bool` |
| `analyze_file` | `summary` | `summary_mode: bool` |
| `analyze_file` | `cursor` | `is_paginated: bool` |
| `analyze_symbol` | `match_mode` | `match_mode: String` |
| `analyze_symbol` | `follow_depth` | `follow_depth: u8` |
| `analyze_symbol` | `import_lookup` | `import_lookup: bool` |
| `analyze_symbol` | `def_use` | `def_use: bool` |
| `analyze_symbol` | `impl_only` | `impl_only: bool` |
| `analyze_symbol` | `git_ref` | `git_ref_used: bool` |
| `analyze_symbol` | `cursor` / `summary` | `is_paginated: bool`, `summary_mode: bool` |
| `edit_overwrite` | `working_dir` | `working_dir_used: bool` |
| `edit_replace` | `working_dir` | `working_dir_used: bool` |
| `exec_command` | `stdin` presence | `stdin_provided: bool` |
| `exec_command` | `timeout_secs` | `timeout_configured_ms: Option<u64>` |
| `exec_command` | `drain_timeout_secs` | `drain_timeout_ms: Option<u64>` |
| `exec_command` | `working_dir` | `working_dir_used: bool` |

**Fix:** Add fields to `MetricEvent` struct, populate in each tool handler, document in `docs/METRICS.md`. `stdin` should record presence only (bool), not content.

Note: `impl_only`, `def_use`, `git_ref`, and `drain_timeout_secs` are candidates for removal if usage proves to be zero after 30 days of metrication. They cannot be removed before that data exists.

**Acceptance criteria:** Each listed parameter produces a corresponding JSONL field on every tool call. `docs/METRICS.md` updated with field descriptions. `cargo test` passes.

---

### O3 — Pagination mode invisible across all tools

**Severity:** Medium
**Files:** All tool handlers, `crates/aptu-coder/src/metrics.rs`
**Guard verdict:** CONFIRMED — zero references to `cursor`, `summary`, or `is_paginated` in `metrics.rs` or any handler.

Whether a call is a first-page, continuation, or summary-mode call is not recorded. Pagination adoption and the cursor-vs-summary split cannot be measured.

This is a subset of O2 but warrants a separate issue because it applies to four tools (`analyze_directory`, `analyze_file`, `analyze_symbol`, and `analyze_module` does not support pagination) and informs UX decisions independently of parameter-level metrication.

---

### O4 — `analyze_symbol` analysis mode not recorded

**Severity:** Medium
**File:** `crates/aptu-coder/src/tools/analyze_symbol.rs`

`analyze_symbol` supports three distinct modes: call graph (default), `import_lookup`, and `def_use`. Additionally `match_mode` controls name matching strategy and `follow_depth` controls traversal depth. None of these are in JSONL. Usage of advanced modes is entirely invisible.

Covered by O2's table; listed separately because it directly blocks the removal decision for `import_lookup`, `def_use`, and `impl_only`.

---

### O5 — Cache L1 evictions and L2 size not metricated

**Severity:** Low
**File:** `crates/aptu-coder-core/src/cache.rs`

L1 (in-memory LRU) evictions are not counted. L2 disk cache entry count and total size are not recorded. On cache miss, the miss tier (L1 or L2) is not distinguished — only hits carry `cache_tier`. Cache write failures are recorded for L2 only.

---

### O6 — `scripts/mcp-metrics.py` has no per-parameter breakdown

**Severity:** Low
**File:** `scripts/mcp-metrics.py`

The script exposes tool call volume, cache health, session patterns, and daily trends. It does not expose per-parameter usage distributions, pagination adoption, feature adoption rates (`import_lookup`, `def_use`, `impl_only`, `match_mode`), or per-session parameter breakdown. All of these require O2 to be resolved first.

---

### S1 — `analyze.rs` 2,075 LOC

**Severity:** Medium
**File:** `crates/aptu-coder-core/src/analyze.rs`

51 functions. Largest: `analyze_focused_with_progress_with_entries_internal` (~196 LOC). Focused-analysis orchestration and directory-level analysis are co-located with no module boundary.

**Proposed split:** Extract focused-analysis path into `analyze_focused.rs`. Keep directory walk and coordination in `analyze.rs`.

---

### S2 — `parser.rs` 1,949 LOC

**Severity:** Medium
**File:** `crates/aptu-coder-core/src/parser.rs`

42 functions. Element extraction (class, function, import node parsing) and tree traversal coordination are co-located.

**Proposed split:** Extract element extraction into `parser_elements.rs`. Keep traversal and dispatch in `parser.rs`.

---

### S3 — `tests.rs` 1,886 LOC

**Severity:** Medium
**File:** `crates/aptu-coder/src/tests.rs`

66 test functions in a single file covering metrics, filters, exec, and edit subsystems.

**Proposed split:** `tests/metrics_tests.rs`, `tests/filter_tests.rs`, `tests/exec_tests.rs`, `tests/edit_tests.rs`.

---

### S4 — `metrics.rs` 1,499 LOC

**Severity:** Medium
**File:** `crates/aptu-coder/src/metrics.rs`

65 functions mixing event building/buffering with file I/O and export logic. Largest test: `test_metrics_export_file_created` (lines 1050–1162, ~112 LOC).

**Proposed split:** Extract file I/O and export logic into `metrics_export.rs`. Keep `MetricEvent`, builder, and in-memory buffer in `metrics.rs`.

---

### S5 — Three tool/support files each with an extractable subsystem

**Severity:** Low

| File | LOC | Largest function | Proposed extraction |
|---|---|---|---|
| `tools/exec_command.rs` | 998 | process spawning, ~136 LOC | `exec_runtime.rs` |
| `tools/analyze_symbol.rs` | 927 | focused resolution, ~183 LOC | `symbol_focused.rs` |
| `cache.rs` | 976 | `DiskCache` impl | `cache_disk.rs` |

---

### S6 — `shell_write.rs` 615 LOC, two distinct concerns

**Severity:** Low
**File:** `crates/aptu-coder/src/shell_write.rs`

`validate_heredocs` (lines 26–259, ~234 LOC) and `scan_backward_for_file_write` (lines 265–453, ~189 LOC) implement two independent subsystems: heredoc safety validation and backward token scanning.

**Proposed split:** `heredoc_validation.rs` (validation + error constructors), `shell_scan.rs` (backward scanner + token helpers). Top-level dispatch stays in `shell_write.rs`.

---

### S7 — `formatter/summary.rs` and `edit_replace.rs` large functions

**Severity:** Low

| File | LOC | Function | Function LOC | Proposed extraction |
|---|---|---|---|---|
| `formatter/summary.rs` | 780 | `format_summary` | ~234 | `formatter/focused.rs` |
| `tools/edit_replace.rs` | 553 | `handle_edit_error` | ~189 | `edit_errors.rs` |

---

## Non-Findings

- **No tool is a removal candidate.** All 7 tools were called within the 31-day window. `analyze_symbol` is the lowest volume (223 calls) but architecturally distinct with no substitute.
- All 20 fields defined in `docs/METRICS.md` are present in actual JSONL files. Schema is current.
- `graph.rs` (1,185 LOC, 40 functions), `types.rs` (934 LOC), `lib.rs` (865 LOC, shim-only), `languages/kotlin.rs` (616 LOC), `languages/csharp.rs` (510 LOC) — not recommended for splitting; each is internally cohesive or constrained by role.

---

## Recommended issue grouping

| Issue | Findings | Scope |
|---|---|---|
| 1 | O1 | Add error metrics to `analyze_symbol` |
| 2 | O2, O3, O4 | Add 19 unmetricated parameters to JSONL + `METRICS.md` |
| 3 | O5 | Add cache L1 eviction and L2 size metrics |
| 4 | O6 | Extend `mcp-metrics.py` with per-parameter breakdown (depends on issue 2) |
| 5 | S1 | Split `analyze.rs` → `analyze_focused.rs` |
| 6 | S2 | Split `parser.rs` → `parser_elements.rs` |
| 7 | S3 | Split `tests.rs` into domain test files |
| 8 | S4 | Split `metrics.rs` → `metrics_export.rs` |
| 9 | S5 | Split `exec_command.rs`, `analyze_symbol.rs`, `cache.rs` (one PR each, can be parallel) |
| 10 | S6 | Split `shell_write.rs` → `heredoc_validation.rs` + `shell_scan.rs` |
| 11 | S7 | Extract large functions from `formatter/summary.rs` and `edit_replace.rs` |
