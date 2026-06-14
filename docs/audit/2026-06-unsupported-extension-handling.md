# Audit: Unsupported Extension Handling in analyze_file and analyze_directory

**Date:** 2026-06-14
**Period observed:** 2026-06-10 to 2026-06-14
**Auditor:** goose (automated, delegate-validated)
**Status:** Pre-implementation -- findings confirmed, issues filed

---

## Executive Summary

Two independent bugs cause degraded agent behavior when tools encounter files with extensions not registered in `EXTENSION_MAP`:

1. `analyze_file` returns `INVALID_PARAMS` (28% error rate across 525 observed calls; 146 failures, all `invalid_params`, zero internal errors). Agents fall back to `exec_command cat/head/grep/sed` -- raw text, no semantic extraction, token cost proportional to file size.

2. `analyze_directory` succeeds but labels all unrecognized files as `language: "unknown"`, collapsing the Languages summary to `"unknown (95%)"` for repos with non-standard stacks (e.g., Astro-based sites).

Five approaches were evaluated by independent read-only validation delegates. Findings below are based on observed source code, not inference.

---

## Observed Data

| Metric | Value |
|---|---|
| analyze_file total calls (2026-06-10 to 2026-06-14) | 525 |
| analyze_file errors | 146 (28%) |
| Error type | 100% INVALID_PARAMS |
| analyze_directory errors | 0 |
| Most common failing extension | .astro (9 attempts, all from clouatre.ca sessions) |
| Other failing extensions observed | .css (1 attempt) |

Agent fallback when analyze_file fails: `exec_command` with `cat`, `head -N`, `grep -n`, `sed -n 'M,Np'`, `wc -l`. Zero semantic extraction. Token cost is O(file size).

---

## Root Causes

### RC-1: analyze_file hard-rejects all unregistered extensions

Call chain (validated against source):

```
analyze_file(path) [lib.rs]
  -> handle_file_details_mode() [analyze.rs]
  -> analyze::analyze_file() [analyze.rs:340]
     -> SemanticExtractor::extract(source, "unknown", ...) [parser.rs:526]
        -> get_language_info("unknown") = None
        -> ParserError::UnsupportedLanguage("unknown")
  -> INVALID_PARAMS returned [lib.rs:878]
```

The JSON schema `path` parameter has a pattern (`SUPPORTED_FILE_EXT_PATTERN`, `schema_helpers.rs:56-65`) but MCP clients (Goose, Claude Desktop) do not enforce it client-side. Every call reaches the server and fails at `SemanticExtractor`.

### RC-2: analyze_directory erases extension identity

In `analyze.rs:process_file_entry` (lines 174-204), the fallback branch is:

```rust
} else {
    ("unknown".to_string(), 0, 0)
};
```

The `ext` variable (`Option<&str>`) is in scope but discarded. All unregistered files receive `language: "unknown"` regardless of their actual extension. The `formatter.rs` language summary groups by this field (lines 217, 930), producing misleading output.

---

## Approach Validation Results

All five approaches were validated by independent read-only delegates against the actual source code.

### Approach A: tree-sitter grammars for additional languages

**Verdict: Partially recommended.** CSS, YAML, and Markdown grammars are production-ready. JSON and Astro require additional scrutiny.

Crate evaluation (verified via crates.io API):

| Crate | Version | Published | Downloads | ABI compat | Age gate | Verdict |
|---|---|---|---|---|---|---|
| tree-sitter-css | 0.25.0 | 2025-09-28 | 2,745,060 | Yes | Pass | Recommended |
| tree-sitter-yaml | 0.7.2 | 2025-10-07 | 2,284,539 | Yes | Pass | Recommended |
| tree-sitter-md | 0.5.3 | 2026-02-26 | 747,599 | Yes | Pass | Recommended (already in #969) |
| tree-sitter-json | 0.24.8 | 2024-11-11 | 2,802,137 | Yes | Fail | Acceptable with SKIP_PACKAGE_AGE_CHECK |
| tree-sitter-astro-next | 0.1.1 | 2026-02-14 | 22,000 | Yes | Pass | Defer -- production vetting required |

All three recommended grammars are from the official `tree-sitter-grammars` GitHub organization. ABI compatibility is confirmed via `tree-sitter-language ^0.1` dependency, matching workspace `tree-sitter-language 0.1.7`.

`tree-sitter-astro-next 0.1.1`: 22k downloads vs 2M+ for the others. No `defuse_query` support. No Astro-specific component extraction queries. Grammar is 4 months old. Not suitable for production without further vetting.

`tree-sitter-json 0.24.8`: published 2024-11-11, predates workspace `tree-sitter = "0.26.6"`. The age gate (`SKIP_PACKAGE_AGE_CHECK`) must be explicitly bypassed. A newer release may become available; check before implementation.

Integration pattern (verified): each grammar requires a `lang-*` feature flag in `crates/aptu-coder-core/Cargo.toml`, an entry in `EXTENSION_MAP` (`lang.rs`), a `LanguageInfo` struct with element/call/import queries, and an extension to `SUPPORTED_FILE_EXT_PATTERN` (`schema_helpers.rs:56-65`).

### Approach B: graceful fallback in analyze_file for unknown extensions

**Verdict: Sound. Implement.**

When `language_for_extension` returns `None`, instead of propagating `ParserError::UnsupportedLanguage` to `INVALID_PARAMS`, catch it in `analyze_file()` (`analyze.rs:340`) and return a degraded `FileAnalysisOutput`:

- `line_count`: actual line count (from `source.lines().count()`)
- `semantic`: empty `SemanticAnalysis` (all vecs default to `Vec::new()`, no required fields violated)
- `formatted`: file header + first 30-50 lines of source text
- `next_cursor`: `None`

Validated findings:
- `FileAnalysisOutput` can represent the degraded state without violating invariants. `test_format_file_details_summary_empty()` (`formatter.rs:1610-1633`) confirms empty semantic renders without panic.
- No first-N-lines utility exists; implement inline as `source.lines().take(50).collect::<Vec<_>>().join("\n")`.
- The schema `path` pattern may be left as-is (clients that enforce it will still filter) or expanded to match all extensions (clients that do not enforce it already reach the server). The tool description must be updated to state the fallback behavior.
- Existing tests for supported languages are unaffected. The test `test_analyze_unsupported_file_type` (`integration_tests.rs:188-215`) currently asserts an error response; it must be updated to assert a success response with empty semantic.
- Regression risk: low. The change is additive -- only the `UnsupportedLanguage` error path changes behavior. All other error types (`ParseError`, `Timeout`, etc.) continue to propagate.

### Approach C: use file extension as language label in analyze_directory

**Verdict: Sound, but requires test update.**

The validator flagged this as "unsound" because `test_analyze_unsupported_file_type` (`integration_tests.rs:213`) asserts `assert_eq!(txt.language, "unknown")`. That test encodes the current broken behavior, not a specification. The assertion must be updated alongside the fix.

Verified source (exact code at `analyze.rs:182-204`):

```rust
let ext = entry.path.extension().and_then(|e| e.to_str());

let (language, function_count, class_count) = if let Some(ext_str) = ext
    && let Some(lang) = language_for_extension(ext_str)
{
    // supported extension path
} else {
    ("unknown".to_string(), 0, 0)  // <- fix here
};
```

`ext` is `Option<&str>`, in scope. Correct replacement:

```rust
} else {
    (ext.map(|e| e.to_lowercase()).unwrap_or_else(|| "unknown".to_string()), 0, 0)
};
```

A parallel instance exists at `analyze.rs:623` in `collect_file_analysis`; both must be updated together.

`FileInfo.language` is used only for display grouping (`formatter.rs:217`, `formatter.rs:930`). It does not affect routing, filtering, or any semantic extraction path. The change is safe.

Files with no extension: `ext` is `None`, `unwrap_or_else` returns `"unknown"` -- correct.

### Approach D: regex-based extraction for non-code patterns

**Verdict: Sound for CSS, YAML, JSON, TOML. Sound and high-value for Astro frontmatter.**

Validated findings:
- `regex = "1"` is already a workspace dependency (`Cargo.toml`). It is not currently used in `aptu-coder-core`. No new dependency required.
- `FunctionInfo` requires only `name`, `line`, `end_line`; `parameters` and `return_type` can be `None`. Regex extraction can populate these fields without violating any struct invariants.
- `ClassInfo` requires `name`, `line`, `end_line`; `methods`, `fields`, `inherits` can be empty vecs.
- `LanguageInfo` is a struct with mandatory `name`, `language`, `element_query`, `call_query` fields and optional function pointers. A regex-backed language can provide no-op tree-sitter queries and implement symbol extraction via the optional handler function pointers.
- `SemanticExtractor::extract(source: &str, language: &str, ...)` accepts arbitrary source text -- confirmed. The Astro frontmatter approach (extract `---...---` block, pass to TypeScript extractor) is mechanically sound.

Validated regex patterns (corrected where needed):

| Format | Pattern | Correction from report |
|---|---|---|
| CSS | `r"^[.#][\w-]+[\s,:{]"` | None -- correct as stated |
| YAML | `r"^(\w[\w-]*): "` | Report had `r"^(\w[\w-]*):"` without trailing space; space required to exclude nested keys reliably |
| JSON | `r#"^\s{0,2}"(\w+)":"#` | None -- correct as stated |
| TOML | `r"^\[([^\]]+)\]"` | None -- correct as stated |

Integration location: new file `crates/aptu-coder-core/src/languages/regex_fallback.rs`, dispatched from `languages/mod.rs` after `get_language_info()` returns `None`.

The Astro frontmatter approach gives full import/export extraction (TypeScript extractor applied to frontmatter block) with zero new grammar dependencies. It is the highest-value item in this approach.

### Approach E: generic line-based content index for all unknown types

**Verdict: Superseded by Approach B. Do not implement as a separate workstream.**

Approach E is a more complex version of Approach B with added section-marker extraction. The validator confirmed:
- Section marker patterns (lines starting with `#`, lines ending with `:`, XML tags at indent 0) have high false positive rates and are unsafe on binary files without explicit guards.
- `FileAnalysisOutput` requires a populated `SemanticAnalysis` struct; "just raw lines" cannot be represented without implementing the same fallback as Approach B.
- Approach B (first N lines) and Approach E (section markers + boundary lines) solve the same root cause. Approach B is simpler, safer, and sufficient.

Approach E is not filed as a separate issue. If section-marker extraction proves valuable after Approach B ships, it can be scoped as an incremental enhancement within Approach D.

---

## Interaction with Issue #969

Issue #969 covers HTML (no-op stub, no grammar dependency) and Markdown (`tree-sitter-md 0.5.3`). It does not cover:

- CSS (5 files observed in clouatre.ca sessions)
- YAML, JSON, TOML (config files present in all repos)
- Astro (9 failed attempts, highest observed impact)
- SVG (59 files in clouatre.ca, semantic extraction not useful -- Approach B fallback is sufficient)

Issue #969 must be implemented before or concurrently with the issues filed below, since its patterns (feature flag, `EXTENSION_MAP` entry, `LanguageInfo` stub, schema pattern update) are the template for all subsequent grammar additions.

---

## Implementation Order

The approaches are complementary and not mutually exclusive. Recommended sequencing:

1. **Issue #969** (already open): HTML stub + Markdown grammar. Establishes the integration template.
2. **Approach C** (new issue): Extension label in `analyze_directory`. Two-line change, two test updates. Zero dependencies. Immediate win for language summaries.
3. **Approach B** (new issue): Graceful fallback in `analyze_file`. Eliminates the 28% error rate. Blocks on nothing; can ship in parallel with #969.
4. **Approach D -- Astro frontmatter** (new issue): Zero-dependency Astro import extraction. Highest semantic value for the observed failure cases.
5. **Approach A -- CSS + YAML grammars** (new issue): Add `tree-sitter-css 0.25.0` and `tree-sitter-yaml 0.7.2`. Full structural extraction for the two remaining high-frequency file types.
6. **Approach D -- JSON/TOML regex** (deferred to Approach A issue or separate): JSON via regex is lower-value than tree-sitter (Approach A JSON deferred due to age gate); TOML regex is a small addition alongside CSS/YAML.
7. **Approach A -- JSON grammar** (deferred): Re-evaluate `tree-sitter-json` when a version compatible with `tree-sitter 0.26.6` without the age-gate bypass is available.
8. **Approach A -- Astro grammar** (deferred): Re-evaluate `tree-sitter-astro-next` when download count and query coverage mature.

---

## Benchmark Against grep/head/sed

After all filed issues are implemented:

| Operation | grep/head/sed | analyze_directory (current) | analyze_file (current) | After fixes |
|---|---|---|---|---|
| File list with LOC | `wc -l **/*.astro` | Equivalent, one call | N/A | Equivalent |
| Language breakdown | `find . -name '*.astro' \| wc -l` per type | `"unknown (95%)"` -- worse | N/A | `"astro (35%), typescript (20%)"` -- equivalent |
| File head | `head -30 file.astro` | N/A | INVALID_PARAMS | First 30-50 lines returned |
| Import list (Astro) | `grep -n '^import' file.astro` | N/A | INVALID_PARAMS | Structured imports via TS extractor -- better than grep |
| CSS selectors | `grep -n '^[.#]' global.css` | N/A | INVALID_PARAMS | Structured selector list (Approach A) -- better than grep |
| YAML keys | `grep -E '^[a-z_]+:' config.yaml` | N/A | INVALID_PARAMS | Structured key list -- better than grep |
| Function names (.ts) | `grep -n 'function\|const.*=>'` | N/A | Full extraction | Full extraction (unchanged) |

---

## Files Referenced

| File | Relevance |
|---|---|
| `crates/aptu-coder-core/src/analyze.rs:174-204` | `process_file_entry` -- Approach C target |
| `crates/aptu-coder-core/src/analyze.rs:340,368` | `analyze_file` -- Approach B target |
| `crates/aptu-coder-core/src/analyze.rs:623` | `collect_file_analysis` -- second instance of Approach C pattern |
| `crates/aptu-coder-core/src/parser.rs:526` | `UnsupportedLanguage` raised here |
| `crates/aptu-coder/src/lib.rs:878` | `UnsupportedLanguage` -> INVALID_PARAMS conversion |
| `crates/aptu-coder-core/src/schema_helpers.rs:56-65` | `SUPPORTED_FILE_EXT_PATTERN` |
| `crates/aptu-coder-core/src/lang.rs:7-49` | `EXTENSION_MAP` |
| `crates/aptu-coder-core/src/languages/mod.rs` | `LanguageInfo` registry |
| `crates/aptu-coder-core/src/formatter.rs:217,930` | Language summary grouping |
| `crates/aptu-coder-core/tests/integration_tests.rs:188-215` | `test_analyze_unsupported_file_type` -- must be updated in both B and C |
| `crates/aptu-coder-core/tests/integration_tests.rs:623` | Second test instance for Approach C |

---

## Non-Goals

- SVG files: Approach B fallback (line count + file head) is the correct answer. Grammar support is not warranted.
- Binary files: existing `read_to_string` error handling (`analyze.rs:356`) already skips non-UTF-8 files. No change needed.
- WASM target: not affected by any of these changes.
