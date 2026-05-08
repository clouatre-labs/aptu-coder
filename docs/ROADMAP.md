# Roadmap

## Wave History

### [Complete] Wave 1: Core Analysis
Initial release. Four tools (`analyze_directory`, `analyze_file`, `analyze_module`, `analyze_symbol`), seven languages (Rust, Go, Java, Python, TypeScript, TSX, Fortran), tree-sitter AST extraction, rayon parallelism, .gitignore-aware walk via `ignore` crate. (language support has since grown to 11; see [Supported Languages](../README.md))

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

### [Benchmarking] Wave 7: OpenFAST Fortran Analysis (v13)

2x2 factorial design (model x tool_set) on Fortran scientific HPC code (OpenFAST). See [v13 methodology](docs/benchmarks/v13/methodology.md).

### [Benchmarking] Wave 8: Rust Trait Dispatch Analysis (v14)

2x2 factorial design (model x tool_set) on Rust trait implementations (ripgrep). See [v14 methodology](docs/benchmarks/v14/methodology.md).

---

## Benchmark-Driven Development

Each Wave closes with a benchmark run validating the Wave's hypotheses.

**Benchmark location:** `docs/benchmarks/vN/` (v3–v10, v12 present)

**Scoring rubric (v12+):** 3 dimensions scored 0–3 each:
- `structural_accuracy`
- `cross_module_tracing`
- `approach_quality`

`quality_score = sum` (max 9)

Earlier benchmarks (v3–v10) used a 4-dimension rubric including `tool_efficiency` (max 12). The `tool_efficiency` dimension was dropped in v12; see each benchmark's `methodology.md` for the rubric in effect at the time.

**Evaluation protocol:** Blind scoring — scorer does not see condition labels during evaluation.

**Statistical method:** Mann-Whitney U with Bonferroni correction; 15 pairwise tests at alpha = 0.05/15 = 0.0033.

---

## Small-Model-First Constraint

All output changes, error messages, server instructions, and tool descriptions must be evaluated against Haiku, Mistral-small-2603, and MiniMax-M2.5 **before** Sonnet.

These models follow tool descriptions literally; they do not apply contextual reasoning to infer optimal paths. A change that improves Sonnet but regresses Haiku is a regression.

---

## Shared Exclusion List

The following directories are non-source and excluded from SUGGESTION footer logic (`src/formatter.rs`) and server instruction guidance (`src/lib.rs`). The constant is defined in `src/lib.rs`:

```
node_modules, vendor, .git, __pycache__, target, dist, build, .venv
```

This list is a single constant in the codebase:

```rust
// src/lib.rs
pub(crate) const EXCLUDED_DIRS: &[&str] = &[
    "node_modules", "vendor", ".git", "__pycache__",
    "target", "dist", "build", ".venv",
];
```

Do not duplicate this constant across modules. Both `#341` and `#342` reference `EXCLUDED_DIRS` from `src/lib.rs`.

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

## Wave 7+ Direction (Tentative)

Unimplemented and pertinent:

- MCP SEP adoption: #1487 (`trustedHint`), #1561 (`unsafeOutputHint`), #1913 (trust/sensitivity annotations), #1984 (governance annotations) -- open upstream; no action until specs stabilize. #1560 (`secretHint`) closed 2026-03-23; evaluate adoption once merged into spec.
- Streamable HTTP transport: add `--http` flag exposing `StreamableHttpService` (axum + rmcp `transport-streamable-http-server` + `transport-streamable-http-server-session` features) alongside existing stdio. Tower middleware: `RequestBodyLimitLayer` (4 MB) + `tower-governor` (per-token rate limit) + static Bearer token from env var. Target deployment: GCP e2-micro Always Free (us-central1) behind Cloudflare proxy (free tier, TLS termination, WAF, 5 rate-limit rules). No changes to tool handlers or session logic required.

## Wave 9: Editing Tools [Complete, Partially Removed]

Augmented aptu-coder with five tools in two phases: one read-only file-content tool (`analyze_raw`) and four mechanical code-editing tools (`edit_overwrite`, `edit_replace`, `edit_rename`, `edit_insert`). The existing analysis tools and composition API remain unchanged. This wave completed the read-analyze-write loop that the coder-build agent (#664, #665) required without introducing a second MCP server.

Note: `edit_rename` and `edit_insert` were removed in issue #779 due to limited adoption and maintenance burden. The two remaining edit tools (`edit_overwrite`, `edit_replace`) continue to support the core write workflow.

### Rationale

Both a combined server and two separate servers inject into the same model context window. Token cost is identical. A single server with one MCP config entry, one binary, and one version pin is operationally simpler. Five editing tools keep the total tool count below the reliable SML selection ceiling (~10-12 tools).

The `ToolRouter::merge()` / `Add` / `AddAssign` API (verified against rmcp 1.5.0 source) supports multi-group composition with no breaking changes from 1.1.0. Write tools are placed in a second `#[tool_router(router = write_router, vis = "pub")]` impl block and merged at construction.

### Phase 1: Mechanical tools [Complete]

Three tools with no tree-sitter dependency. These validated the BUILD agent workflow and established the write-path integration before adding AST complexity.

- `analyze_raw(path, start_line?, end_line?)` -- "Raw file content with optional line range. Prefer start_line/end_line to limit tokens on large files; omit both for full content. Use analyze_file for structure, not content. Example queries: Read lines 10-40 of src/lib.rs; Show the full contents of config.toml." `read_only_hint=true`, `idempotent_hint=true`
- `edit_overwrite(path, content)` -- "Create or overwrite a file at path with content. Creates parent directories if needed. Overwrites without confirmation; use edit_replace to replace a specific block instead of the whole file. Example queries: Write a new test file at tests/foo_test.rs; Overwrite src/config.rs with updated content." `read_only_hint=false`, `destructive_hint=true`, `idempotent_hint=false`
- `edit_replace(path, old_text, new_text)` -- "Replace a unique exact text block in a file. Errors if old_text appears zero times or more than once -- fix by making old_text longer and more specific. Use edit_overwrite to replace the whole file. Example queries: Replace the error handling block in src/main.rs; Update the function signature in lib.rs." `read_only_hint=false`, `destructive_hint=true`, `idempotent_hint=false`

Cache invalidation: `edit_overwrite` and `edit_replace` call `cache.invalidate_file(path)` after every write. mtime-based cache keys self-invalidate in the common case, but mtime granularity is 1 second on some filesystems (HFS+, some ext4 configurations); explicit invalidation prevents stale reads within the same second.

### Phase 2: AST-backed tools [Removed in issue #779]

Two tools that required `aptu-coder-core` (formerly `code-analyze-core`) capture data were implemented but later removed due to limited adoption and maintenance burden.

- `edit_rename(path, old_name, new_name, kind?)` -- [REMOVED] AST-aware rename within a single file. Matched by node kind, not string -- identifiers in string literals and comments were excluded.
- `edit_insert(path, symbol_name, position, content)` -- [REMOVED] Insert content immediately before or after a named AST node. Used start_byte/end_byte from the capture pipeline.

### Annotation posture update

Wave 9 write tools were the exception to the annotation freeze established in the Annotation Posture Policy section. Write tools (`edit_overwrite`, `edit_replace`) carry `read_only_hint=false`, `destructive_hint=true`, `idempotent_hint=false`. Read tools (all existing analysis tools) retain `read_only_hint=true`. The per-tool `#[tool(annotations(...))]` macro attribute in rmcp 1.5.0 is confirmed to support mixed postures within one server.

Note: `read_only_hint` is a hint surfaced to MCP clients in `tools/list`; rmcp 1.5.0 has no per-tool access control enforcement.

### SML validation requirement

Per the Small-Model-First Constraint: the editing tools were evaluated against Haiku, Mistral-small-2603, and MiniMax-M2.5 before Sonnet in a Wave 9 benchmark. Tool descriptions followed literal-instruction style -- SML models follow tool descriptions literally.


