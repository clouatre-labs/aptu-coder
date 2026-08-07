# Grammar Crate Version Audit -- 2026-08-06

Audited all 13 tree-sitter grammar crates pinned in `Cargo.toml` against their
current crates.io latest versions. Tree-sitter core: **0.26.11** (from `Cargo.lock`).
ABI bridge: `tree-sitter-language 0.1.7` (all 13 grammar crates depend on this, not
directly on `tree-sitter`; the bridge is backward-compatible across the 0.24--0.26 range).

---

## Version Table

| Crate | Pinned | Latest (crates.io) | Gap | Breaking? | Action |
|---|---|---|---|---|---|
| tree-sitter-rust | 0.24.2 | 0.24.2 | none | no | hold (at latest) |
| tree-sitter-go | 0.25.0 | 0.25.0 | none | no | hold (at latest) |
| tree-sitter-cpp | 0.23.4 | 0.23.4 | none | no | hold (at latest) |
| tree-sitter-c-sharp | 0.23.5 | 0.23.5 | none | no | hold (at latest) |
| tree-sitter-java | 0.23.5 | 0.23.5 | none | no | hold (at latest) |
| tree-sitter-kotlin-ng | 1.1.0 | 1.1.0 | none | no | hold (at latest) |
| tree-sitter-python | 0.25.0 | 0.25.0 | none | no | hold (at latest) |
| tree-sitter-typescript | 0.23.2 | 0.23.2 | none | no | hold (at latest) |
| tree-sitter-javascript | 0.25.0 | 0.25.0 | none | no | hold (at latest) |
| tree-sitter-fortran | 0.6.0 | 0.6.0 | none | no | hold (at latest) |
| tree-sitter-md | 0.5.3 | 0.5.3 | none | no | hold (at latest) |
| tree-sitter-css | 0.25.0 | 0.25.0 | none | no | hold (at latest) |
| tree-sitter-yaml | 0.7.2 | 0.7.2 | none | no | hold (at latest) |

---

## Per-Crate Recommendation

All 13 grammar crates are pinned to their current crates.io latest version. No
version gap exists for any crate. Recommendation for each: **hold (at latest)**.

| Crate | Recommendation | Rationale |
|---|---|---|
| tree-sitter-rust | hold (at latest) | 0.24.2 == crates.io latest |
| tree-sitter-go | hold (at latest) | 0.25.0 == crates.io latest |
| tree-sitter-cpp | hold (at latest) | 0.23.4 == crates.io latest |
| tree-sitter-c-sharp | hold (at latest) | 0.23.5 == crates.io latest |
| tree-sitter-java | hold (at latest) | 0.23.5 == crates.io latest |
| tree-sitter-kotlin-ng | hold (at latest) | 1.1.0 == crates.io latest |
| tree-sitter-python | hold (at latest) | 0.25.0 == crates.io latest |
| tree-sitter-typescript | hold (at latest) | 0.23.2 == crates.io latest |
| tree-sitter-javascript | hold (at latest) | 0.25.0 == crates.io latest |
| tree-sitter-fortran | hold (at latest) | 0.6.0 == crates.io latest |
| tree-sitter-md | hold (at latest) | 0.5.3 == crates.io latest |
| tree-sitter-css | hold (at latest) | 0.25.0 == crates.io latest |
| tree-sitter-yaml | hold (at latest) | 0.7.2 == crates.io latest |

---

## ABI Compatibility Note

Since tree-sitter 0.24, grammar crates do not depend on the `tree-sitter` crate
directly. They depend on `tree-sitter-language 0.1.7`, a stable, version-agnostic
ABI bridge crate that exposes only the `Language` type. The calling crate
(`aptu-coder-core`) links `tree-sitter 0.26.11` separately. As long as grammar
crates were published against `tree-sitter-language >= 0.1` (all 13 were), they are
ABI-compatible with any tree-sitter core in the 0.24--0.26+ range. No
compatibility issues exist in the current workspace.

Version spread across the grammar crates (0.23.x, 0.24.x, 0.25.x, 1.x) reflects
each upstream grammar project's independent release cadence, not a version skew
problem. Cargo resolves a single `tree-sitter-language 0.1.7` for all of them.

---

## Summary

| Category | Count | Crates |
|---|---|---|
| bump-safe | 0 | -- |
| bump-needs-test | 0 | -- |
| hold (at latest) | 13 | all 13 |

**Safe to bump in a single PR:** 0 (all crates are already at crates.io latest).
**Need investigation:** 0.

No grammar-bump PR is needed at this time. Re-run this audit after any upstream
grammar crate publishes a new version; the tooling is: `cargo search <crate> --limit 1`.

---

## Follow-on PR Prompt

No follow-on bump PR is warranted because all 13 crates are already at their latest
published version. The prompt below is templated for future use when gaps exist.

---

### Template: Grammar Bump PR (activate when gaps are found)

```text
Bump the following tree-sitter grammar crates in Cargo.toml.
No source or test changes -- dependency version lines only.
Run `cargo test` and `cargo clippy -- -D warnings` after each change to confirm
no API breakage. Commit with:

  chore(deps): bump tree-sitter grammar crates to latest

Cargo.toml changes (replace pinned version strings):
  # <crate-name> = "<old>" -> "<new>"
  # List populated by next audit run when gaps exist.

Verification:
  cargo build
  cargo test
  cargo clippy -- -D warnings
  cargo fmt --check

PR: draft; target main; one squash commit; no Cargo.lock hand-edits.
```

---

*Audit method: `cargo search <crate> --limit 1` for each crate on 2026-08-06.*
*Pinned versions sourced from `Cargo.toml` root workspace.*
*Lock file: `Cargo.lock` at HEAD on branch `main`.*
