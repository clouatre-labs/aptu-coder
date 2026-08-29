# Audit: Tree-Sitter Grammar Crate Versions

Date: 2026-08-06  
Commit: c4d6fb8  
Version: v0.27.0  
Toolchain: Rust / tree-sitter 0.26.11 / tree-sitter-language 0.1.7

## Purpose

Point-in-time audit of all 13 tree-sitter grammar crates pinned in the root `Cargo.toml` against their current crates.io latest versions. Establishes whether any crate is behind, how large each gap is, and whether bumping would require migration work or is safe to land in a single batch PR.

Scope: version gap classification, ABI compatibility against tree-sitter core 0.26.11, and a recommendation per crate. No `Cargo.toml` changes in this session.

## Methodology

`cargo search <crate> --limit 1` for each of the 13 crates. Pinned versions read from `Cargo.toml`. ABI bridge chain confirmed from `Cargo.lock` (all grammar crates resolve to `tree-sitter-language 0.1.7`; none carry a direct `tree-sitter` dependency). Gap classified as:

- **BEHIND**: pinned version is below the latest published release.
- **CURRENT**: pinned version equals the latest published release.

---

## Findings

### G01 -- CURRENT -- tree-sitter-rust at 0.24.2

Latest on crates.io: `0.24.2`. No gap. The upstream grammar has not published a newer release. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G02 -- CURRENT -- tree-sitter-go at 0.25.0

Latest on crates.io: `0.25.0`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G03 -- CURRENT -- tree-sitter-cpp at 0.23.4

Latest on crates.io: `0.23.4`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G04 -- CURRENT -- tree-sitter-c-sharp at 0.23.5

Latest on crates.io: `0.23.5`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G05 -- CURRENT -- tree-sitter-java at 0.23.5

Latest on crates.io: `0.23.5`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G06 -- CURRENT -- tree-sitter-kotlin-ng at 1.1.0

Latest on crates.io: `1.1.0`. No gap. This crate uses an independent versioning convention (major version tracks a significant grammar rewrite, not tree-sitter core). The `1.x` version is not a semantic mismatch with the `0.2x` range of other grammar crates. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G07 -- CURRENT -- tree-sitter-python at 0.25.0

Latest on crates.io: `0.25.0`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G08 -- CURRENT -- tree-sitter-typescript at 0.23.2

Latest on crates.io: `0.23.2`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G09 -- CURRENT -- tree-sitter-javascript at 0.25.0

Latest on crates.io: `0.25.0`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G10 -- CURRENT -- tree-sitter-fortran at 0.6.0

Latest on crates.io: `0.6.0`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G11 -- CURRENT -- tree-sitter-md at 0.5.3

Latest on crates.io: `0.5.3`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G12 -- CURRENT -- tree-sitter-css at 0.25.0

Latest on crates.io: `0.25.0`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

### G13 -- CURRENT -- tree-sitter-yaml at 0.7.2

Latest on crates.io: `0.7.2`. No gap. ABI: links `tree-sitter-language 0.1.7`. Compatible with core 0.26.11.

**Action:** none.

---

## ABI Compatibility Note

Since tree-sitter 0.24, grammar crates depend on `tree-sitter-language` rather than `tree-sitter` directly. `tree-sitter-language 0.1.7` is a stable, version-agnostic bridge crate that exposes only the `Language` struct. The calling crate (`aptu-coder-core`) links `tree-sitter 0.26.11` separately. All grammar crates published against `tree-sitter-language >= 0.1` are ABI-compatible with any tree-sitter core in the 0.24--0.26+ range. Cargo resolves a single `tree-sitter-language 0.1.7` across all 13 grammar crates; no duplicate resolver conflict exists.

The version spread across grammar crates (0.23.x, 0.24.x, 0.25.x, 1.x) reflects each upstream grammar project's independent release cadence. It is not a skew problem.

---

## Summary

*Table 1: Finding classification by count.*

| Classification | Count | Findings |
|---|---|---|
| CURRENT (no gap) | 13 | G01-G13 |
| BEHIND (gap exists) | 0 | -- |

**Safe to bump in a single PR:** 0 (all crates are already at crates.io latest).
**Require investigation before bumping:** 0.

No grammar-bump PR is warranted. Re-run this audit after any upstream grammar crate publishes a new version. Tooling: `cargo search <crate> --limit 1` for each entry in `Cargo.toml`.

## Recommended Action Order

No actions required. All 13 grammar crates are at their published crates.io latest version and are ABI-compatible with tree-sitter core 0.26.11 via the `tree-sitter-language 0.1.7` bridge.

Next audit trigger: any Dependabot or Renovate alert on a grammar crate, or a manual re-run after 30 days.
