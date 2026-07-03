# Roadmap

## Wave History

### [Complete] Wave 1: Core Analysis
Initial release. Four tools (`analyze_directory`, `analyze_file`, `analyze_module`, `analyze_symbol`), seven languages (Rust, Go, Java, Python, TypeScript, TSX, Fortran), tree-sitter AST extraction, rayon parallelism, .gitignore-aware walk via `ignore` crate. (language support has since grown to 18; see [Supported Languages](../README.md))

### [Complete] Wave 2: MCP Protocol (milestone 7)
Summary-first output, `outputSchema` per tool, cursor pagination.

### [Complete] Wave 3: Analysis Quality (milestone 8)
Multi-strategy call graphs, inheritance tracking, cross-client compatibility.

### [Complete] Wave 4: Advanced Analysis (milestone 9)
Type-aware call resolution, dataflow analysis.

### [Complete] Wave 5: Progressive Disclosure (milestone 10)
Summary and pagination for FileDetails and SymbolFocus modes.

### [Complete] Wave 6: Agent UX & Performance (milestone 11)
Issues: #340, #341, #342, #354, #355, #356, #357.

Target: close the non-Sonnet model performance gap identified in v10 benchmark.

Key changes:
- #340: `analyze_module` directory guard — actionable error steering agents to `analyze_directory`
- #341: Actionable SUGGESTION footer naming largest source directory with absolute path
- #342: Server instructions updated with 4-step recommended workflow
- #354: Async metrics collection via `src/metrics.rs` — zero hot-path overhead
- #356: Idempotency audit and cross-client compatibility verification
- #357: ROADMAP.md and OBSERVABILITY.md documentation

### [Complete] 0.3.0 Library API

Promotes `aptu-coder-core` to a stable public library API and adds structured output fields for programmatic consumption without text parsing.

Issues: #623, #624, #625.

Key changes:
- #623: `analyze_str(source, language, ast_recursion_limit)` -- public in-memory parsing API; eliminates TOCTOU race for consumers holding source text without an on-disk path; adds `AnalyzeError::UnsupportedLanguage` variant
- #624: `CallChainEntry { symbol, file, line }` public type; `callers`, `test_callers`, `callees` fields on `FocusedAnalysisOutput`; MCP clients can now consume caller/callee relationships from `structuredContent` without text parsing
- #625: `analyze_symbol` tool description updated to accurately reflect `FocusedAnalysisOutput` schema

### [Complete] Wave 7: OpenFAST Fortran Analysis (v13)

2x2 factorial design (model x tool_set) on Fortran scientific HPC code (OpenFAST). See [v13 methodology](benchmarks/v13/methodology.md). Haiku savings: 68% fewer tokens, 68% cheaper. Sonnet savings: 46% fewer tokens, 42% cheaper. Validated Fortran language support for scientific HPC repositories.

### [Complete] Wave 8: Rust Trait Dispatch Analysis (v14)

2x2 factorial design (model x tool_set) on Rust trait implementations (ripgrep). See [v14 methodology](benchmarks/v14/methodology.md).

### [Complete] observability-v1

Full observability stack shipped across #820–#824:

- **#820**: Span attribute policy and never-record list defined; see [OBSERVABILITY.md](../OBSERVABILITY.md).
- **#821**: All 7 tool handlers enriched with OpenTelemetry GenAI semantic attributes (`gen_ai.system`, `gen_ai.operation.name`, `gen_ai.tool.name`) and key parameters as span fields. Behavioral decisions (`auto_summary`, `cache_hit`, `truncated`) emitted as span events.
- **#822**: `tracing-opentelemetry` bridge added. Conditional OTLP export via `BatchSpanProcessor` gated on `OTEL_EXPORTER_OTLP_ENDPOINT`. Noop providers when unset; zero overhead. OTel Metrics SDK initialized in parallel (JSONL channel retained as always-on local trail).
- **#823**: Log-trace correlation via `opentelemetry-appender-tracing`; every `info!`/`error!` callsite gains `trace_id` and `span_id` automatically. W3C Trace Context (`traceparent`, `tracestate`) extracted from MCP `params._meta` and propagated as span parent -- tool spans become children in the calling agent's distributed trace. Child spans added for key sub-operations: `ast.parse_batch` (directory parse batch), `graph.traverse` (BFS per depth), `walk_directory` (traversal). Graceful shutdown flushes all three OTel providers.
- **#824**: Observability documentation updated in [docs/METRICS.md](METRICS.md).

### [Complete] Project rename: code-analyze-mcp to aptu-coder (#826)

All source, docs, benchmark tooling, env vars, and binary references updated. Env vars renamed (breaking for users who had these set):

- `CODE_ANALYZE_DIR_CACHE_CAPACITY` → `APTU_CODER_DIR_CACHE_CAPACITY`
- `CODE_ANALYZE_FILE_CACHE_CAPACITY` → `APTU_CODER_FILE_CACHE_CAPACITY`

`migrate_legacy_metrics_dir()` handles XDG data path migration at runtime for existing users with metrics data in the old directory.

### [Complete] Fortran handler: module extraction and call graph (#828)

Completes the Fortran language handler that was partially implemented:

- Fixed `ELEMENT_QUERY` to capture module constructs via `internal_procedures` for `CONTAINS` sections; corrected stale comment about `module_statement` (name child is required in tree-sitter-fortran 0.6.0).
- Added `derived_type_member_expression` pattern to `CALL_QUERY` for Fortran 2003+ `obj%method()` bound procedure calls.
- Implemented `extract_function_name`, `find_receiver_type`, and `find_method_for_receiver` handlers to unblock call graph traversal and module-scoped procedure tracking.
- Added `extract_module_name` private helper for the two-level child walk on `module_statement`.
- 16 AAA-pattern tests: module extraction, subroutine/function name extraction, USE import detection, direct CALL and OOP member call patterns, module-scoped vs top-level procedure distinction, CONTAINS sections, empty modules.

---

## Benchmark-Driven Development

Each Wave closes with a benchmark run. Benchmarks live in `docs/benchmarks/vN/`. Scoring rubric (v12+): 3 dimensions × 0–3 = max 9 (`structural_accuracy`, `cross_module_tracing`, `approach_quality`). Earlier benchmarks (v3–v10) used a 4-dimension rubric including `tool_efficiency` (max 12). Blind scoring; Mann-Whitney U with Bonferroni correction. See [DESIGN-GUIDE.md](DESIGN-GUIDE.md) for methodology detail.

---

## Small-Model-First Constraint

All output changes, error messages, server instructions, and tool descriptions must be evaluated against Haiku, Mistral-small-2603, and MiniMax-M2.5 **before** Sonnet.

These models follow tool descriptions literally; they do not apply contextual reasoning to infer optimal paths. A change that improves Sonnet but regresses Haiku is a regression.

---

## Shared Exclusion List

`EXCLUDED_DIRS` in `src/lib.rs` lists non-source directories skipped by SUGGESTION footer logic and server instruction guidance: `node_modules`, `vendor`, `.git`, `__pycache__`, `target`, `dist`, `build`, `.venv`. Do not duplicate this constant.

---

## Annotation Posture Policy

Current settings are stable and reflect ground truth:

| Annotation | Value | Rationale |
|---|---|---|
| `readOnlyHint` | `true` | All tools are read-only filesystem operations |
| `destructiveHint` | `false` | No writes, no side effects |
| `idempotentHint` | `true` | Same input produces same output (verified by #347) |
| `openWorldHint` | `false` | Results are bounded by the input path |

**Exception:** The two `edit_*` tools (`edit_overwrite`, `edit_replace`) and the `exec_command` tool deviate from the default posture. Write tools (`edit_overwrite`, `edit_replace`) carry `readOnlyHint=false`, `destructiveHint=true`, and `idempotentHint=false` to accurately reflect their write-capable, non-idempotent nature. The `exec_command` tool additionally sets `openWorldHint=true` to surface the shell-execution safety warning to MCP clients.

No annotation changes until new MCP SEPs land (tracked in #1913, #1984, #1561, #1560, #1487). Validated against external MCP Blog 2 reference (2026-03-16).

---

### [Complete] Streamable HTTP transport (#885)

Added `--port N` flag. When set, aptu-coder binds to `127.0.0.1:N` and serves all tools over the MCP streamable HTTP transport (2025-11-25 spec) using `StreamableHttpService` with `NeverSessionManager` (tools are pure functions; session state buys nothing). When `--port` is absent, stdio mode is unchanged.

---

### [Complete] exec_command output management (v0.14.2)

Four PRs shipped together to make `exec_command` output safe for large contexts:

- **#955**: `analyze_file` and `analyze_module` return `INVALID_PARAMS` for unsupported file extensions instead of a generic error; `inputSchema` pattern constraint added so MCP clients surface the restriction before calling. (Superseded by #1061 and #1062, which replaced `analyze_file` with graceful fallback and regex extraction.)
- **#957**: CI runners migrated to `ubuntu-24.04-arm` everywhere (ARM64).
- **#961**: Byte-level output caps on `exec_command`: stdout 30k chars (tail-preserving), stderr 10k chars (tail-preserving), combined `output_text` 50k chars. Cap thresholds are data-driven from 27,981 observed calls (0.33% exceed 30k stdout). `output_truncated: Option<bool>` added to `MetricEvent` and the JSONL schema.
- **#962**: Command-pattern output filter table: 7 built-in rules (git pull/fetch/push/log/status, cargo build/test) suppress per-file noise before output reaches the model. `git pull` additionally injects `--no-stat` before execution. Project-local rules via `.aptu/filters.toml`. `filter_applied` field in `structuredContent` identifies which rule fired. Filters apply on success only; raw output preserved on failure.
- **#964**: Regression test suite for output volume and filter correctness.

---

### [Complete] Language expansion: Markdown, HTML, CSS, YAML, Astro, JSON, TOML (#1063--#1073)

Added six new languages with varying extraction depth:

- **#1066**: Markdown language support (`lang-markdown` feature; extracts headings as functions, links/images as imports).
- **#1063, #1073**: CSS and YAML tree-sitter grammar support (`lang-css`, `lang-yaml`; full selector/rule extraction for CSS, key extraction for YAML). Regex fallback applies when the feature flag is disabled.
- **#1069**: Regex-based extraction for Astro (TypeScript frontmatter), CSS, YAML, JSON (first-level key extraction), and TOML (section header extraction). Astro, JSON, and TOML are always-on regardless of feature flags.
- **#1067, #1068**: Graceful fallback for unsupported extensions in `analyze_file` and `analyze_module`: returns file extension as language label plus a structured `unsupported: true` marker instead of an error.
- **#1077**: `INVALID_PARAMS` references updated; `analyze_file` no longer rejects unsupported extensions -- it falls back gracefully.
- **#1084**: Language table and supported-extension audit completed; all entries verified.

### [Complete] Observability and schema extensions (v0.17--v0.20)

Several incremental improvements to the metrics schema and runtime behavior:

- **#1127, #1128**: `unsupported: Option<bool>` field added to `FileAnalysisOutput` and `ModuleInfo` structs. Set to `true` when a file extension has no AST handler. Tool descriptions reordered for clarity.
- **#1129, #1130**: `force`, `verbose`, and `ast_recursion_limit` parameters removed from `analyze_file` and `analyze_symbol`. These were vestigial no-ops; removing them reduces schema noise and prevents agents from passing dead options.
- **#1131**: Missing `no_cache_meta` on `analyze_symbol` pagination error responses fixed.
- **#1132**: Stale-context circuit breaker added to `edit_replace`. After 5 consecutive `not_found` or `ambiguous` failures on the same (session_id, canonical_path) pair, the handler returns a directive error instructing the agent to re-read the file. The failure counter map is capped at 1024 entries to prevent unbounded growth.
- **#1136**: Path validation fix: replaced ancestor-walk with parent-dir validation in the `require_exists=false` branch of `validate_path`.
- **#1137**: CRLF line endings in `old_text` are now normalized to LF before matching in `edit_replace`. Prevents spurious `not_found` errors when editing files with Windows line endings.
- **#1147, #1148, #1149**: L2 on-disk call-graph cache added to `analyze_symbol`. Cache is keyed by canonical path + git HEAD SHA. Configurable via `APTU_CODER_DISK_CACHE_DIR` (default: `$XDG_DATA_HOME/aptu-coder/analysis-cache`) and `APTU_CODER_DISK_CACHE_DISABLED=1`. `cache_tier: l1_memory | l2_disk` added to `MetricEvent`; `cache_write_failure` field tracks disk write failures.
- **#1150, #1154**: `language` field added to JSONL `MetricEvent` schema. Populated for `analyze_file` and `analyze_module` calls with the human-readable language name (e.g., `"rust"`, `"python"`). Omitted from JSONL when null for backward compatibility.
- **#1153**: `exec_command` now rejects heredoc syntax (`<<MARKER`) where the closing delimiter is absent before spawning the child process, returning `INVALID_PARAMS` with a diagnostic message.
- **#1155**: `timeout_secs` parameter re-added to `exec_command` (was removed in #1122). When set to a positive integer, the child process is killed after that many seconds; `timed_out: true` is set in `ShellOutput` and `MetricEvent`; `exit_code` is null. A value of 0 or omitted means no limit.
- **#1156**: `call_frequency` on `analyze_symbol` output is now filtered out when the `Functions` field is not in the projected fields set, reducing response size for callers that only request caller/callee lists.
- **#1124**: Raw path interpolation removed from model-visible error messages (security fix).
- **#1125**: `edit_replace` accepts empty `new_text` to delete the matched block. `max_depth` defaults to 3 when omitted. Login shell PATH snapshot on macOS now uses `$SHELL` first for correct profile sourcing.
- **#1102**: JSON Schema `uint`/`uint64` formats replaced with draft-07 compliant `integer`.
- **#1085, #1133**: `exec_command` tool description updated to prefer `working_dir` over `cd` and to frame the `cd` prohibition as a mechanical fact rather than a policy.

---

## Direction (Tentative)

Unimplemented and pertinent:

- MCP SEP adoption: #1487 (`trustedHint`), #1561 (`unsafeOutputHint`), #1913 (trust/sensitivity annotations), #1984 (governance annotations) -- open upstream; no action until specs stabilize. #1560 (`secretHint`) closed 2026-03-23; evaluate adoption once merged into spec.

## Wave 9: Editing Tools [Complete]

Added `edit_overwrite` and `edit_replace` to complete the read-analyze-write loop (#664, #665). `analyze_raw`, `edit_rename`, and `edit_insert` were removed in #779 due to limited adoption. Write tools carry `readOnlyHint=false`, `destructiveHint=true`, `idempotentHint=false`; `exec_command` additionally sets `openWorldHint=true`.


