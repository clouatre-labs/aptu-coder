# Audit: Unsupported Extension Handling

**Observed period:** 2026-06-10 to 2026-06-14
**Validated:** delegate-verified against source (read-only, no inference)

---

## Summary

Two independent bugs cause degraded agent behavior on files with extensions not registered in
`EXTENSION_MAP`:

1. `analyze_file` returns `INVALID_PARAMS` on any unregistered extension. 146 of 525 calls
   failed (28%) -- all `invalid_params`, zero internal errors. Agents fall back to
   `exec_command cat/head/grep/sed`: raw text, no semantic extraction, token cost O(file size).

2. `analyze_directory` succeeds but labels all unregistered files `language: "unknown"`,
   collapsing the Languages summary regardless of actual extension distribution.

Five approaches were validated. All are complementary. Four are filed as issues; one (Approach E)
is superseded by Approach B and not filed separately.

---

## Observed Data

| Metric | Value |
|---|---|
| `analyze_file` total calls | 525 |
| `analyze_file` errors | 146 (28%) |
| Error classification | 100% `INVALID_PARAMS`, 0% internal |
| `analyze_directory` errors | 0 |
| Most common failing extension | `.astro` (9 attempts) |
| Other failing extensions observed | `.css` (1 attempt) |

---

## Root Causes

### RC-1 -- `analyze_file` hard-rejects all unregistered extensions

**Files:** `crates/aptu-coder-core/src/analyze.rs:340`,
`crates/aptu-coder-core/src/parser.rs:526`,
`crates/aptu-coder/src/lib.rs:878`

Call chain (verified):

```
analyze_file(path)
  -> SemanticExtractor::extract(source, "unknown", ...)  [parser.rs:526]
     -> get_language_info("unknown") = None
     -> ParserError::UnsupportedLanguage("unknown")
  -> INVALID_PARAMS                                       [lib.rs:878]
```

The JSON schema `path` pattern (`SUPPORTED_FILE_EXT_PATTERN`, `schema_helpers.rs:56-65`) lists
only registered extensions. MCP clients do not enforce this pattern client-side; every call
reaches the server and fails at `SemanticExtractor`.

---

### RC-2 -- `analyze_directory` discards extension identity

**Files:** `crates/aptu-coder-core/src/analyze.rs:182-204, 623`,
`crates/aptu-coder-core/src/formatter.rs:217, 930`

In `process_file_entry`, `ext` (`Option<&str>`) is in scope but discarded in the fallback
branch:

```rust
} else {
    ("unknown".to_string(), 0, 0)
};
```

`formatter.rs` groups the Languages summary by `FileInfo.language` (lines 217, 930). All
unregistered files collapse into a single `"unknown"` bucket regardless of actual extension.

---

## Findings

### F1 -- Extension label erased in `analyze_directory` (issue #1060)

**Files:** `crates/aptu-coder-core/src/analyze.rs:191, 623`,
`crates/aptu-coder-core/tests/integration_tests.rs:213`

**Verdict:** CONFIRMED -- `ext` is `Option<&str>` and is in scope at both fallback sites.
`FileInfo.language` is used only for display grouping; it does not affect routing or extraction.
Two-line fix; two test updates required.

`test_analyze_unsupported_file_type` (`integration_tests.rs:213`) asserts
`assert_eq!(txt.language, "unknown")`. This assertion encodes the current broken behavior, not a
specification. It must be updated alongside the fix.

A parallel instance exists at `analyze.rs:623` (`collect_file_analysis`); both must be changed
together for consistency.

**Fix:**

```rust
// before
("unknown".to_string(), 0, 0)

// after
(ext.map(|e| e.to_lowercase()).unwrap_or_else(|| "unknown".to_string()), 0, 0)
```

Files with no extension: `ext` is `None`; `unwrap_or_else` returns `"unknown"` -- correct.

**Regression gate:** `cargo test -p aptu-coder-core` must pass. Language summary output for
supported extensions must be unchanged.

**PR group:** standalone (trivial, zero dependencies).

---

### F2 -- `analyze_file` returns error instead of degraded response (issue #1061)

**Files:** `crates/aptu-coder-core/src/analyze.rs:340-368`,
`crates/aptu-coder/src/lib.rs:878`,
`crates/aptu-coder-core/tests/integration_tests.rs:188-215`

**Verdict:** CONFIRMED -- `ParserError::UnsupportedLanguage` propagates to `INVALID_PARAMS`
unconditionally. `FileAnalysisOutput` can represent a degraded state: empty `SemanticAnalysis`
(all vecs default to `Vec::new()`) is valid -- confirmed by `test_format_file_details_summary_empty`
(`formatter.rs:1610-1633`). No first-N-lines utility exists; implement inline.

**Fix:** In `analyze_file()` (`analyze.rs:340`), catch `UnsupportedLanguage` and return a
success response:

- `line_count`: `source.lines().count()`
- `semantic`: empty `SemanticAnalysis`
- `formatted`: file header + `source.lines().take(50).collect::<Vec<_>>().join("\n")`
- tool description updated: "For unsupported extensions, returns line count and file head."

`test_analyze_unsupported_file_type` must be updated to assert a success response with empty
semantic rather than an error.

**Regression gate:** `cargo test` must pass. All supported extensions must return unchanged
responses. `INVALID_PARAMS` must still be returned for invalid paths (not just unknown
extensions).

**PR group:** standalone (prerequisite for F3).

---

### F3 -- No symbol extraction for Astro, CSS, YAML, JSON, TOML (issue #1062)

**Files:** `crates/aptu-coder-core/src/languages/` (new file `regex_fallback.rs`),
`crates/aptu-coder-core/src/languages/mod.rs`,
`crates/aptu-coder-core/src/lang.rs`

**Verdict:** CONFIRMED -- `regex = "1"` is already a workspace dependency; it is not currently
used in `aptu-coder-core`. `FunctionInfo` requires only `name`, `line`, `end_line`; regex
extraction can populate these without violating struct invariants. `SemanticExtractor::extract`
accepts arbitrary source text -- the Astro frontmatter approach (extract `---...---` block, pass
to TypeScript extractor) is mechanically sound and requires zero new dependencies.

Validated regex patterns:

| Format | Pattern | Notes |
|---|---|---|
| CSS | `r"^[.#][\w-]+[\s,:{]"` | selectors |
| YAML | `r"^(\w[\w-]*): "` | top-level keys; trailing space excludes nested block keys |
| JSON | `r#"^\s{0,2}"(\w+)":"#` | first-level keys |
| TOML | `r"^\[([^\]]+)\]"` | section headers |
| Astro | frontmatter block -> TypeScript extractor | full import/export extraction, zero new deps |

**Fix:** New `crates/aptu-coder-core/src/languages/regex_fallback.rs` with per-format
extractors. Dispatch from `languages/mod.rs` after `get_language_info()` returns `None`.
Register `astro`, `css`, `yaml`, `json`, `toml` in `EXTENSION_MAP` (`lang.rs`) and
`SUPPORTED_FILE_EXT_PATTERN` (`schema_helpers.rs:56-65`).

**Regression gate:** `cargo test -p aptu-coder-core` must pass. Supported language extraction
must be unchanged. New tests required: one happy path per format, one edge case (empty file,
no-extension file).

**PR group:** depends on F2 (#1061).

---

### F4 -- CSS and YAML lack tree-sitter grammar support (issue #1063)

**Files:** `Cargo.toml` (workspace deps), `crates/aptu-coder-core/Cargo.toml`,
`crates/aptu-coder-core/src/lang.rs`,
`crates/aptu-coder-core/src/languages/mod.rs`,
`crates/aptu-coder-core/src/schema_helpers.rs:56-65`

**Verdict:** CONFIRMED -- `tree-sitter-css 0.25.0` and `tree-sitter-yaml 0.7.2` are from the
official `tree-sitter-grammars` GitHub organization, ABI-compatible (`tree-sitter-language ^0.1`
matches workspace `0.1.7`), and pass the 7-day age gate. Download counts confirm production
maturity (CSS: 2,745,060; YAML: 2,284,539). Both are superseded by the regex extractors in F3
for basic key/selector extraction; the grammar versions provide structural accuracy (no false
positives in string literals, nested blocks, or comments).

Deferred crates (do not include in this issue):

- `tree-sitter-json 0.24.8`: published 2024-11-11; age gate fails without `SKIP_PACKAGE_AGE_CHECK`. Track in #1064.
- `tree-sitter-astro-next 0.1.1`: 22k downloads, no `defuse_query`, no Astro-specific component
  queries. Track in #1064.

**Fix:** Follow #969 integration pattern for each grammar: `lang-css` and `lang-yaml` feature
flags, workspace dep declarations, `EXTENSION_MAP` entries, `LanguageInfo` structs with
element/call/import queries, `SUPPORTED_FILE_EXT_PATTERN` update. F3 regex extractors for CSS
and YAML are superseded by these grammars once merged; remove or gate them accordingly.

**Regression gate:** `cargo build && cargo test && cargo clippy -- -D warnings` must pass.
Binary size delta must be documented in PR description. Existing language extraction unchanged.

**PR group:** depends on #969, F2 (#1061), F3 (#1062).

---

## Non-Findings (considered and dismissed)

- **Approach E (generic section-marker extraction):** overlaps with F2 (Approach B). Both catch
  `UnsupportedLanguage` and return a success response. Section-marker patterns (lines starting
  with `#`, lines ending with `:`) have high false-positive rates and are unsafe on binary
  content without explicit guards. F2 (first N lines) is simpler, safer, and sufficient. Not
  filed.
- **SVG files:** Approach B fallback (line count + file head) is correct. Grammar support is not
  warranted. No issue filed.
- **Binary files:** existing `read_to_string` error handling (`analyze.rs:356`) skips non-UTF-8
  files. No change needed.
- **`tree-sitter-astro-next 0.1.1`:** 22k downloads vs 2M+ for comparable grammars; no
  `defuse_query`; grammar is 4 months old. Tracked in #1064, not implemented.

---

## Implementation Order

| Order | Issue | Scope | Risk |
|---|---|---|---|
| 1 | #969 (open) | HTML stub + Markdown grammar; establishes integration template | Low |
| 1 | #1060 | Extension label in `analyze_directory`; two-line change | Zero |
| 2 | #1061 | Graceful fallback in `analyze_file`; eliminates 28% error rate | Low |
| 3 | #1062 | Regex extraction for Astro, CSS, YAML, JSON, TOML | Low |
| 3 | #1072 | Observability: `tracing::warn` on Astro TypeScript extractor error | Zero |
| 4 | #1063 | `tree-sitter-css` + `tree-sitter-yaml` grammars | Medium |
| -- | #1064 | Tracking: deferred grammar candidates; no code | Zero |

Order 1 items are independent and can be developed in parallel. Order 2 gates on Order 1
(#1061 provides the fallback path #1062 extends). Order 3 (#1072) gates on #1062. Order 4 gates on #969, #1061, and #1062.

All items through order 3 (including #1072) are merged; the unsupported-extension-handling milestone is complete.
