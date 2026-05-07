# Wave 9 SML: TypeScript JSX (TSX) Language Support Re-wiring Benchmark

## Overview

Wave 9 SML measures the impact of MCP edit tools versus native tools on Rust language implementation tasks.
It uses the same 2x2 factorial design as v14 (model x tool_set) with 4 conditions, scored across 3 rubric
dimensions, and analyzed for tool-set effects on implementation correctness and structural fidelity.

The target task is re-wiring TypeScript JSX (tsx) language support in the aptu-coder repository. The tsx
feature has been partially stripped from the codebase before each run, and the agent must restore it by
modifying two files (mod.rs and lang.rs) with four precise edits. The task exercises both MCP edit tools
(edit_replace) and native file operations (read/write), and requires careful attention to three genuine
traps: namespace mismatch, shared pub mod declarations, and correct constant naming.

## Background

Wave 9 introduced 5 new edit tools to the aptu-coder MCP server: edit_overwrite, edit_replace, edit_rename,
edit_insert. This benchmark validates their utility on a realistic implementation task. The SML (Small
Model Language) constraint from the ROADMAP requires that the benchmark task be completable by smaller
models (haiku) within reasonable token budgets. Unlike v14 (which required creating new files and writing
complex logic), the tsx re-wiring task is more focused: restore existing functionality by adding back
stripped code, with verification via grep checks instead of cargo compilation.

## Repository

- **Repository:** `clouatre-labs/aptu-coder`
- **Commit:** `origin/main` (pinned at run time; fresh worktree per run)
- **License:** Apache-2.0
- **Language:** Rust (edition 2024)

## Module Structure

| Directory | Files | Role |
|-----------|-------|------|
| crates/aptu-coder-core/src/languages/ | ~10 | Language handlers (java.rs, python.rs, typescript.rs, etc.) |
| crates/aptu-coder-core/src/lang.rs | 1 | Extension map, supported_languages() |
| crates/aptu-coder-core/src/languages/mod.rs | 1 | Language registry, get_language_info(), get_ts_language() |

## Why This Codebase

1. **Well-known to the model:** aptu-coder is the subject of this benchmark; the model has context on
   its structure and patterns from the system prompt and repository analysis.

2. **Focused edit task:** Unlike v14 (which required creating new files and writing complex logic), the
   tsx re-wiring task is more constrained: restore existing functionality by adding back stripped code.
   This allows smaller models (haiku) to complete the task within token budgets.

3. **Genuine traps:** Three real mistakes (namespace mismatch, shared pub mod, constant naming) create
   a native failure surface without requiring cargo compilation for verification.

4. **No external clone:** The task targets the repo itself. Each run gets a fresh worktree from origin/main,
   eliminating setup overhead and ensuring reproducibility.

## Task Description

### Pre-run Preparation

Before the agent runs, the benchmark runner strips tsx wiring from two files:

1. **mod.rs:** Remove the tsx arms from `get_language_info()` and `get_ts_language()` functions.
2. **lang.rs:** Remove tsx entries from `EXTENSION_MAP` and `supported_languages()`.

The stripping is idempotent and leaves the codebase in a valid, compilable state (typescript support
remains intact; only tsx is removed).

### Agent Task

The agent must restore tsx support by:

1. Reading mod.rs and lang.rs to understand the existing typescript pattern.
2. Adding back the tsx arms to mod.rs (two locations).
3. Adding back the tsx entries to lang.rs (two locations).

The task description explicitly lists the exact targets (line numbers and code snippets) and discloses
the three traps to avoid.

### Verification

Post-run verification uses five grep checks (no cargo compilation):

1. **grep_mod_rs_tsx_arm:** mod.rs contains `"tsx" => Some(LanguageInfo {` with `typescript::ELEMENT_QUERY`
2. **grep_mod_rs_language_tsx:** mod.rs contains `"tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX`
3. **grep_lang_rs_extension:** lang.rs contains `("tsx", "tsx")`
4. **grep_lang_rs_supported:** lang.rs contains `"tsx",` in supported_languages
5. **grep_no_spurious_pub_mod:** mod.rs does NOT contain `pub mod tsx;` (should only be feature-gated)

All five checks must pass for the run to be scored as correct.

## Conditions

### Edit Profile Constraint

As of PR #763, the `analyze_raw` tool requires explicit `start_line` and `end_line` parameters for files
exceeding 100 lines. Bare calls (without line ranges) on large files return a server error. The benchmark
prompts for conditions A and C (MCP tools) have been updated to include explicit line ranges:

- `analyze_raw(path="...mod.rs", start_line=62, end_line=260)` -- covers the get_language_info and get_ts_language functions
- `analyze_raw(path="...lang.rs", start_line=7, end_line=100)` -- covers EXTENSION_MAP and supported_languages

Additionally, Wave 9 analysis recommends `edit_overwrite` over `edit_replace` for multi-edit small-file tasks,
as it avoids the overhead of exact-text matching. Conditions A and C prompts have been updated to use
`edit_overwrite` for all four edits.

## Rubric

Scoring uses three dimensions, each 0-3 points (max 9 total):

| Dimension | 0 | 1 | 2 | 3 |
|-----------|---|---|---|---|
| **namespace_correctness** | No tsx wiring added | Partial wiring (1-2 checks pass) | Mostly correct (3-4 checks pass) | All checks pass, correct namespace |
| **extension_registration** | No extension entries | Only EXTENSION_MAP or supported_languages | Both entries present but incomplete | Both entries complete and correct |
| **structural_fidelity** | Syntax errors or invalid Rust | Compiles but missing arms | All arms present, minor issues | All arms present, correct structure |

## Run Count and Design

- **Total runs:** 24 (4 pilots + 20 scored)
- **Pilots:** 1 per condition (A, B, C, D) to verify task clarity and calibrate rubric
- **Scored runs:** 20 total (5 per condition, randomized seed=42)
- **Conditions:**
  - A: claude-sonnet-4-6 + MCP tools (analyze_raw, edit_replace)
  - B: claude-sonnet-4-6 + native tools (read, write)
  - C: claude-haiku-4-5 + MCP tools
  - D: claude-haiku-4-5 + native tools

## Analysis

Post-run analysis will examine:

1. **Tool-set effect:** Do MCP tools (A, C) outperform native tools (B, D) on edit accuracy?
2. **Model effect:** Do larger models (sonnet, A/B) outperform smaller models (haiku, C/D)?
3. **Trap detection:** How often do agents fall into the three traps (namespace, pub mod, constant)?
4. **Verification coverage:** Do grep checks reliably distinguish correct from incorrect wiring?

## Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| Stripping tsx breaks the build | Stripping is idempotent; typescript support remains intact; verified locally |
| Grep checks miss incorrect wiring | Five checks cover all four edits; checks are specific (not just "tsx" substring) |
| Agents add spurious pub mod tsx | Explicit trap disclosure in task description; grep check detects this |
| Namespace confusion (tsx:: vs typescript::) | Explicit trap disclosure; grep checks verify correct namespace |

## Verification Method

Verification is self-contained (no cargo, no rustc):

```bash
# Five grep checks run after agent completes
grep '"tsx" => Some(LanguageInfo {' mod.rs && grep 'typescript::ELEMENT_QUERY' mod.rs
grep '"tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX' mod.rs
grep '("tsx", "tsx")' lang.rs
grep '"tsx",' lang.rs
! grep '^pub mod tsx;' mod.rs
```

All five must pass for the run to be scored as correct.
