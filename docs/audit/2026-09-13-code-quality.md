# Audit: Code Quality and Structural Cleanup -- September 2026

Date: 2026-09-13  
Commit: d6ef2ff  
Version: v0.32.6  
Toolchain: Rust 1.98.0 / rmcp 3.3.0 / tokio async

## See Also

- [ARCHITECTURE.md](../ARCHITECTURE.md) -- module map and data flow
- [DESIGN-GUIDE.md](../DESIGN-GUIDE.md) -- idiom and naming conventions
- [REPO-STANDARDS.md](../REPO-STANDARDS.md) -- contribution and CI standards

## Purpose

Repo-wide scan for dead code, duplicated logic, and speculative flexibility that doesn't earn its keep. Each finding below was independently verified by a second, adversarial pass before being turned into an issue -- the verification pass was told to try to refute the claim, not confirm it, and to default to a lower-confidence verdict when it couldn't fully check something.

All 5 findings survived verification as **CONFIRMED**, several with corrections that are reflected in the linked issues.

---

## Methodology

- Discovery: `rg` across both crates, manifest inspection, targeted file reads
- Verification: a second pass per finding, instructed to disprove the claim (exhaustive `rg` re-checks, direct file reads, and for two findings an empirical `cargo clippy` run before/after the proposed change)
- Verdicts: **CONFIRMED** / **PARTIAL** / **REFUTED**

---

## Summary Table

| # | Category | Finding | Verdict | Issue |
|---|---|---|---|---|
| 1 | Dependency / Duplication | Three near-identical file-lock RAII implementations built on `fs2`; std has native locking since Rust 1.89 | CONFIRMED | [#1526](https://github.com/clouatre-labs/aptu-coder/issues/1526) |
| 2 | Dead Code | `AnalyzeDirectoryContext.peer` field written but never read | CONFIRMED | [#1527](https://github.com/clouatre-labs/aptu-coder/issues/1527) |
| 3 | Dead Code | Stale `#[allow(dead_code)]` on a function with a real production caller | CONFIRMED | [#1528](https://github.com/clouatre-labs/aptu-coder/issues/1528) |
| 4 | API Surface | `edit_replace_block` is a dead pass-through; its sibling is also a trivial forward | CONFIRMED | [#1529](https://github.com/clouatre-labs/aptu-coder/issues/1529) |
| 5 | Structure | `analyze_focused.rs` mixes two call-graph-disjoint concerns in one 1219-line file | CONFIRMED | [#1530](https://github.com/clouatre-labs/aptu-coder/issues/1530) |

---

## Finding Detail

### F1 -- Duplicate file-lock RAII implementations, `fs2` dependency removable

**Files:** `crates/aptu-coder-core/src/cache_disk.rs:257-303`, `crates/aptu-coder-core/src/graph/store.rs:16,30-62`, `crates/aptu-coder/src/metrics.rs:453`, `crates/aptu-coder/src/metrics_export.rs:79-114`

Three separate implementations of the same pattern -- open-or-create a `.lock` file, take a shared or exclusive `fs2` lock, wrap the `File` in an RAII guard that releases the lock on drop -- exist across two crates: `ShardLockGuard` in `cache_disk.rs`, a second `ShardLockGuard` in `graph/store.rs`, and `MetricsLockGuard` in `metrics.rs`. `std::fs::File::lock()`, `lock_shared()`, `try_lock()`, `try_lock_shared()`, and `unlock()` have been stable since Rust 1.89.0; this workspace's MSRV is 1.98.0.

**Verification note:** the three implementations are not fully symmetric. `graph/store.rs` is gated `#[cfg(not(target_arch = "wasm32"))]`; `cache_disk.rs` has no such gating at all. The metrics path is async (`tokio::task::spawn_blocking`), single-file, exclusive-lock-only, unlike the other two. A shared helper needs a sync core that each caller wraps as needed, not a one-size-fits-all signature.

**Fix:** one shared lock-guard helper in `aptu-coder-core` built on `std::fs::File`'s native locking, used by all three call sites; remove `fs2` from all three `Cargo.toml` files.

**Estimate:** ~60-80 fewer lines, 1 fewer dependency.

---

### F2 -- Unused `peer` field on `AnalyzeDirectoryContext`

**Files:** `crates/aptu-coder/src/tools/mod.rs:86-93`, `crates/aptu-coder/src/lib.rs:276,353`

The field is written at both of its construction sites but never read in either file that receives `&AnalyzeDirectoryContext` (`analyze_directory.rs`, `server.rs`), nor in any test. It carries `#[allow(dead_code)]` with a comment citing a not-yet-built notification feature. (A nearby `.peer` read at `lib.rs:869` belongs to rmcp's own `NotificationContext`, an unrelated type -- verified during the check to rule out a false positive.)

**Fix:** remove the field and its two construction sites; re-add scoped to that feature if and when it's built.

---

### F3 -- Stale `#[allow(dead_code)]` on `migrate_legacy_metrics_dir_impl`

**Files:** `crates/aptu-coder/src/metrics_export.rs:441`

The function has a real, unconditional production caller chain: `main.rs:214` -> public `migrate_legacy_metrics_dir()` -> this impl. Verified empirically: `cargo clippy -p aptu-coder -- -D warnings` passes clean both with and without the `#[allow(dead_code)]` present.

**Fix:** delete the attribute.

---

### F4 -- `edit_replace_block` dead pass-through wrapper

**Files:** `crates/aptu-coder-core/src/edit.rs:134-158`

`edit_replace_block` (3-arg) has zero production callers -- its 9 call sites are all inside its own unit test module. The real production path (`crates/aptu-coder/src/tools/edit_replace.rs:504`) calls `edit_replace_block_with_options` directly. `edit_replace_block_with_options` is itself a pure forward to `edit_replace_block_inner` with no added logic.

**Verification note:** `aptu-coder-core` is `publish = true` -- both functions are public API of a published crate. Collapsing them is a semver-breaking change, not a purely internal cleanup.

**Fix:** collapse into one function; either bump `aptu-coder-core`'s version for the breaking change, or keep a `#[deprecated]` shim under the old name.

---

### F5 -- `analyze_focused.rs` mixes two unrelated concerns

**Files:** `crates/aptu-coder-core/src/analyze_focused.rs:47-1219`

Lines 47-807 (call-chain/focused-analysis: `analyze_focused_with_progress`, `compute_chains`, `resolve_symbol`, etc.) and lines 808-1219 (module/wildcard-import lookup: `analyze_module_file`, `analyze_import_lookup`, `resolve_wildcard_imports`, and their tree-sitter helpers) share zero call edges in either direction, and each group has its own distinct external callers.

**Verification note:** the original scope missed `resolve_wildcard_imports`, the `pub(crate)` glue function tying the wildcard-import helpers together -- it must move with them. `analyze_module_file` is a third, standalone single-file-analysis concern, so the new module should be named to cover both module-lookup and wildcard-import resolution (e.g. `module_lookup.rs`), not just "import lookup."

**Fix:** extract lines 808-1219 into a new module; pure structural move, no behavior change.

---

## Non-Findings (considered and dismissed)

- **Single-implementation traits/factories:** the codebase doesn't use Rust traits as its own abstraction layer outside test fixtures; no Factory/Manager/Provider patterns found beyond `MetricEventBuilder`, which has 12+ real call sites.
- **Test redundancy:** per-language test suffixes (`parse_and_extract`, `semantic_correctness`, etc.) each exercise a distinct tree-sitter grammar -- legitimate, not clones. No repeated non-language-suffixed test names found across 413 test functions.
- **`structural.rs` (1333 lines), `call_graph.rs` (1182 lines):** each is one cohesive `impl` block plus its error type; length reflects one concern, not several crammed together.
- **Env vars / CLI flags:** all ~13 checked (`APTU_CODER_DISK_CACHE_*`, `APTU_CODER_METRICS_EXPORT_FILE`, `APTU_CODER_BEARER_TOKEN`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `APTU_SHELL`, etc.) have direct read call sites tied to real behavior branches.
- **Remaining dependencies** (`snap`, `base64`, `lru`, `blake3`, `axum`, `rustls`, `opentelemetry*`, `nix`, `tokio-stream`, `rayon`, `ignore`, `percent-encoding`, `regex`, `toml`): all have active, correctly-scoped call sites.

---

## Regression Strategy

| Order | Issue | Risk |
|---|---|---|
| 1 | [#1528](https://github.com/clouatre-labs/aptu-coder/issues/1528) | Zero blast radius -- single-line removal |
| 2 | [#1527](https://github.com/clouatre-labs/aptu-coder/issues/1527) | Small, no readers of the removed field |
| 3 | [#1526](https://github.com/clouatre-labs/aptu-coder/issues/1526) | Cargo.toml + shared helper, touches 3 call sites -- test each crate independently |
| 4 | [#1530](https://github.com/clouatre-labs/aptu-coder/issues/1530) | Pure structural move, no logic change |
| 5 | [#1529](https://github.com/clouatre-labs/aptu-coder/issues/1529) | Breaking public API change -- needs a version bump or deprecation shim, do last |
