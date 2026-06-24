# Structural Refactor Audit -- June 2026

**Date:** 2026-06-24
**Codebase snapshot:** v0.21.0 (3fbd64a)
**Scope:** Full codebase -- `crates/aptu-coder` (5,952L prod) + `crates/aptu-coder-core` (11,756L prod)
**Focus:** File size, function size, dead code, bleeding API surface, Rust idiomatic best practices, performance

Three read-only audit passes were run in parallel:

- **Structure scout:** file/function decomposition, dead types, pub surface
- **Dead code scout:** `cargo clippy` warnings, unreachable_pub, feature flag gaps, re-export leaks
- **Idioms guard:** clone amplification, allocation patterns, unsafe blocks, workspace lint gaps

---

## Previous audits and what is already resolved

| Audit | Date | Status |
|---|---|---|
| Code quality | 2026-06-13 | Resolved: all 8 findings closed or tracked in reaudit.json |
| Performance / token efficiency | 2026-06-13 | Resolved: double-read, depth filter, L1 cache all in |
| Re-audit (reaudit.json) | 2026-06-15 | F1 (ExecCommandParams.cache field doc), F5 (L2 disk cache for analyze_symbol) still partial |
| Unsupported extension handling | 2026-06-15 | Resolved |

The current audit picks up where those left off and covers new ground introduced since the post-M17 cleanup (9d23b14).

---

## Findings

Severity: **HIGH** (blocks maintainability or hides bugs), **MEDIUM** (technical debt, measurable cost), **LOW** (polish, lint hygiene).

---

### F1 -- BLOCKER: `exec_command_impl` is 378 lines [HIGH]

**File:** `crates/aptu-coder/src/tools/exec_command.rs`

`exec_command_impl` handles heredoc validation, stdin setup, child process spawn, output collection, size limiting, filter application, and metric emission all in one function. At 378 lines it is the largest function in the codebase by a significant margin.

**Proposed split:**

| New function | Responsibility | Est. lines |
|---|---|---|
| `validate_and_prepare_stdin` | heredoc detection, stdin content validation | ~60 |
| `spawn_and_collect` | child spawn, output drain, timeout | ~100 |
| `apply_output_filters` | size limiting, filter rule application | ~80 |
| `exec_command_impl` (residual) | orchestration only | ~140 |

No logic changes required; pure extraction.

---

### F2 -- `formatter.rs` must be split [HIGH]

**File:** `crates/aptu-coder-core/src/formatter.rs` -- **2,886 lines**, 58 functions

This is the largest file in the codebase. It mixes four independent formatting concerns with no shared state between them. Two functions individually exceed 200 lines:

- `format_summary` -- 233 lines
- `format_focused_internal` -- 218 lines

**Proposed split (matches existing `formatter_defuse.rs` precedent):**

| New file | Content | Est. lines |
|---|---|---|
| `formatter_directory.rs` | `format_structure`, `format_summary`, tree-building helpers | ~700 |
| `formatter_file.rs` | `format_file_details`, `format_module_index`, imports/classes/functions sections | ~800 |
| `formatter_symbol.rs` | `format_focused_internal`, call-graph rendering | ~600 |
| `formatter.rs` (residual) | shared utilities, public dispatch | ~250 |
| `formatter_defuse.rs` | (keep as-is) | 147 |

All formatter functions are only used within `aptu-coder-core`; the public split is zero-risk.

---

### F3 -- Dead public API types in `types.rs` [HIGH]

**File:** `crates/aptu-coder-core/src/types.rs` and `crates/aptu-coder-core/src/lib.rs`

Three types are re-exported from `lib.rs` with full `pub` visibility but have zero instantiation sites anywhere in the workspace:

| Type | Re-exported? | Derives carried | Action |
|---|---|---|---|
| `AnalysisResult` | Yes | `Serialize, Deserialize, JsonSchema` | Remove |
| `FocusedAnalysisData` | Yes | `Serialize, Deserialize, JsonSchema` | Remove |
| `CallChain` | Yes | `Serialize, Deserialize, JsonSchema` | Remove |

These are legacy API stubs. Each carries `schemars::JsonSchema` derive which generates schema code at compile time. Removing them reduces compile time and eliminates dead API surface that consumers might accidentally depend on.

---

### F4 -- 28 `unreachable_pub` warnings in `aptu-coder` [HIGH]

**Files:** `crates/aptu-coder/src/metrics.rs`, `crates/aptu-coder/src/tools/mod.rs`

`cargo clippy -W unreachable_pub` produces 28 warnings:

- **19 warnings in `metrics.rs`:** `MetricEventBuilder` is `pub(crate)` but its 19 builder methods are `pub`. Since the struct is not accessible outside the crate, all 19 methods are unreachable as public items. Change to `pub(crate)`.
- **9 warnings in `tools/mod.rs`:** The `mod tools` declaration is private (`mod tools`, not `pub mod tools`) but the 9 submodules inside are declared `pub`. The visibility hierarchy is violated. Change inner `pub mod` to `pub(crate) mod`.

These warnings are currently suppressed only because `unreachable_pub` is not in the deny list. Adding it to workspace lints will enforce the fix.

---

### F5 -- `validation.rs` mixes two unrelated concerns [MEDIUM]

**File:** `crates/aptu-coder/src/validation.rs` -- **770 lines**, 18 functions

Two independent subsystems share one file with no shared state:

- **Heredoc validation:** `validate_heredocs`, `scan_backward_for_file_write`, `check_line_for_heredoc` -- the heredoc scanner (`scan_backward_for_file_write`, 189 lines) is its own state machine.
- **Path validation:** `validate_path`, `resolve_path`, `is_safe_path`, and related helpers.

**Proposed split:**

| New file | Content | Est. lines |
|---|---|---|
| `heredoc_validation.rs` | heredoc scanner, multi-line state machine | ~350 |
| `path_validation.rs` | path resolution, safety checks | ~250 |
| `validation.rs` (residual) | public entry points, dispatch | ~170 |

---

### F6 -- `metrics.rs` pub utility functions [MEDIUM]

**File:** `crates/aptu-coder/src/metrics.rs` -- **1,301 lines**

Five free functions are declared `pub` but are only referenced within `metrics.rs` itself:

- `unix_ms`
- `path_component_count`
- `path_file_ext`
- `path_language`
- `current_date_str`

All five should be `pub(crate)` at most; `unix_ms` and `current_date_str` have no callers outside metrics and could be `fn` (private).

Additionally, `migrate_legacy_metrics_dir_impl` carries `#[allow(dead_code)]` -- the compiler already flags it. Review whether the migration window has passed; if so, remove it.

---

### F7 -- Unnecessary `.to_string()` allocations in OTEL hot path [MEDIUM]

**File:** `crates/aptu-coder/src/metrics.rs`, function `record_otel_metrics`

Three `.to_string()` calls on `&'static str` / `&str` values:

```rust
// Current (3 allocations per tool call):
KeyValue::new("tool", event.tool.to_string())
KeyValue::new("error_type", error_type.to_string())
KeyValue::new("tier", tier.to_string())
```

`KeyValue::new` accepts `impl Into<Value>` and `&'static str` implements `Into<Value>` directly. Removing these three `.to_string()` calls eliminates 3 heap allocations per tool invocation on the OTEL path (~60-80 ns each).

---

### F8 -- `graph.rs` clone amplification on BFS traversal [MEDIUM]

**File:** `crates/aptu-coder-core/src/graph.rs` -- **1,176 lines**

`build_from_results` and `find_chains_bfs` clone `String` keys multiple times per edge: once for the HashMap entry key, once for the neighbor name, and once for the secondary index. Switching symbol names from `String` to `Arc<str>` in `CallGraph` and the lowercase index reduces clone cost during BFS to a reference-count increment. For large codebases with deeply connected call graphs this is an estimated 15-30% reduction in allocator pressure during graph construction (not benchmarked).

---

### F9 -- 4 `unsafe` blocks replaceable in `cache.rs` [MEDIUM]

**File:** `crates/aptu-coder-core/src/cache.rs`

Four uses of `NonZeroUsize::new_unchecked` where `NonZeroUsize::new(x).unwrap()` would be zero-cost after const propagation:

```rust
// Current:
unsafe { NonZeroUsize::new_unchecked(CAP) }

// Replacement (no unsafe, same codegen after const prop):
NonZeroUsize::new(CAP).unwrap()
```

Removing these four `unsafe` blocks satisfies the existing `undocumented_unsafe_blocks = "deny"` lint without the `// SAFETY:` annotation burden, and shrinks the unsafe surface.

---

### F10 -- `AnalyzeSymbolContext.disk_cache` dead field [MEDIUM]

**File:** `crates/aptu-coder/src/tools/analyze_symbol.rs`, line 37

The `disk_cache` field on `AnalyzeSymbolContext` is annotated `#[allow(dead_code)]`. The comment states it is "retained for notification infrastructure." If that infrastructure has not landed in v0.21.0, this field should be removed and re-added when the feature ships. Carrying dead fields in a hot context struct wastes memory layout and misleads future maintainers.

---

### F11 -- Missing `#[must_use]` on multiple pure functions [LOW]

**Files:** `validation.rs`, `cache.rs`, `formatter.rs`, several others

Pure functions returning `Result` or computed values without `#[must_use]` allow callers to silently discard results. Examples:

- All cache constructor functions (`DiskCache::new`, `CallGraphCache::new`)
- `validate_path`, `resolve_path`, `is_safe_path` in `validation.rs`
- Formatter utility functions in `formatter.rs`

Adding `#[must_use]` is a zero-cost, zero-risk annotation that turns silent discard into a compile-time warning.

---

### F12 -- Workspace lint gaps [LOW]

**File:** `Cargo.toml` workspace `[lints.clippy]`

Current deny list has only 2 lints: `undocumented_unsafe_blocks = "deny"` and `unwrap_used = "deny"`. Missing high-value additions:

| Lint | Level | Rationale |
|---|---|---|
| `unreachable_pub` | `"warn"` | Catches F4 class of issues at CI time |
| `must_use_candidate` | `"warn"` | Catches F11 class at CI time |
| `redundant_clone` | `"warn"` | Catches F8 class at CI time |
| `needless_pass_by_value` | `"warn"` | Catches unnecessary owned-parameter patterns |
| `large_enum_variant` | `"warn"` | Guards against enum size blowup in hot paths |

Adding these five lints to `[lints.clippy]` in the workspace `Cargo.toml` will enforce all the categories above going forward.

---

### F13 -- Feature flag inconsistency in `lang.rs` [LOW]

**File:** `crates/aptu-coder-core/src/lang.rs`

Languages `astro`, `json`, and `toml` appear unconditionally in `language_for_extension` with no feature gate, while `html`, `markdown`, and `cpp` are gated behind Cargo features. The asymmetry has no documented intent and makes the feature system misleading. Either gate all languages consistently or remove the feature flags and include all languages unconditionally.

---

### F14 -- `pub mod` re-export leaks in `aptu-coder/src/lib.rs` [LOW]

**File:** `crates/aptu-coder/src/lib.rs`

`pub mod metrics`, `pub mod otel`, and `pub mod logging` are exported as fully public modules. They are only consumed by `main.rs` and test code; they carry no stable API contract. All three should be `pub(crate)` to prevent downstream crates from depending on internal telemetry modules if `aptu-coder` is ever used as a library.

---

### F15 -- `main()` is 120 lines [LOW]

**File:** `crates/aptu-coder/src/main.rs`

`main()` at 120 lines handles CLI parsing, tracing initialization, metrics startup, analyzer construction, and stdio vs HTTP dispatch. Extracting `setup_tracing()` (~30 lines) and `parse_cli_args()` (~40 lines) reduces `main` to ~50 lines and makes each concern independently testable.

---

## Summary table

| ID | Severity | File | Finding | Action |
|---|---|---|---|---|
| F1 | HIGH | `tools/exec_command.rs` | `exec_command_impl` 378 lines | Split into 4 functions |
| F2 | HIGH | `formatter.rs` | 2,886 lines, 2 functions >200L | Split into 4 files |
| F3 | HIGH | `types.rs` + `lib.rs` | 3 dead public API types with derive | Remove |
| F4 | HIGH | `metrics.rs`, `tools/mod.rs` | 28 `unreachable_pub` warnings | `pub` -> `pub(crate)` |
| F5 | MEDIUM | `validation.rs` | Two unrelated concerns, 770 lines | Split into 2 files |
| F6 | MEDIUM | `metrics.rs` | 5 utility functions over-`pub` | `pub` -> `pub(crate)` or `fn` |
| F7 | MEDIUM | `metrics.rs` | 3 `.to_string()` on `&str` in OTEL path | Remove, use `Into<Value>` directly |
| F8 | MEDIUM | `graph.rs` | `String` clone amplification in BFS | `Arc<str>` for symbol names |
| F9 | MEDIUM | `cache.rs` | 4 `unsafe NonZeroUsize::new_unchecked` | Replace with `.new(x).unwrap()` |
| F10 | MEDIUM | `tools/analyze_symbol.rs` | Dead `disk_cache` field with `#[allow(dead_code)]` | Remove field |
| F11 | LOW | Multiple | Multiple pure functions missing `#[must_use]` | Add annotation |
| F12 | LOW | `Cargo.toml` | Only 2 workspace clippy lints | Add 5 lints |
| F13 | LOW | `lang.rs` | Feature flag asymmetry across languages | Unify policy |
| F14 | LOW | `aptu-coder/src/lib.rs` | `pub mod` for internal telemetry modules | `pub(crate)` |
| F15 | LOW | `main.rs` | `main()` 120 lines | Extract 2 helpers |

---

## Carry-over from reaudit.json (still open)

| ID | Finding | Status |
|---|---|---|
| F1 (reaudit) | `ExecCommandParams.cache` field doc still advertises caching as active | Still open |
| F5 (reaudit) | `analyze_symbol` L2 disk cache absent (cross-restart hits lost) | Still open |

---

## Estimated impact

- **LOC removed by dead code cleanup (F3, F10):** ~50 lines
- **Compile time reduction:** Removing 3 `JsonSchema` derives (F3) reduces macro expansion
- **Runtime allocation savings (F7):** 3 heap allocations eliminated per tool call on OTEL path (~180-240 ns total, compounding under load)
- **Graph memory savings (F8):** estimated 15-30% reduction in allocator pressure during BFS on large codebases (not benchmarked)
- **Unsafe surface reduction (F9):** 4 `unsafe` blocks eliminated, no safety annotations required
- **Maintainability (F1, F2, F5):** Largest file (2,886L) and largest function (378L) both split; average file size drops by ~40% for the formatter module

---

## Recommended order of execution

1. **F3** -- Remove dead types (pure deletion, zero risk, unblocks API clarity)
2. **F4 + F12** -- Fix `unreachable_pub` and add workspace lints (CI enforcement before further work)
3. **F7 + F9** -- Performance and unsafe cleanup (trivial mechanical fixes)
4. **F6 + F10 + F14** -- Visibility corrections (mechanical, no logic change)
5. **F1** -- Split `exec_command_impl` (moderate complexity, high value)
6. **F2** -- Split `formatter.rs` (largest effort, lowest risk since all callers are internal)
7. **F5** -- Split `validation.rs` (moderate effort)
8. **F8** -- `Arc<str>` in graph (requires type propagation across graph.rs)
9. **F11 + F13 + F15** -- Polish pass (low effort, low risk)
