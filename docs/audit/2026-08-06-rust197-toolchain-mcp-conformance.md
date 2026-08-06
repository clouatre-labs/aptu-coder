# Audit: Rust 1.97.1 Toolchain, Dependency Freshness, MCP Conformance, and Codebase Quality

Date: 2026-08-06
Commit: e485d7c
Version: v0.27.0
Toolchain: rustc 1.96.0 (pinned MSRV) / rustc 1.97.1 (installed locally)

## See Also

- [ARCHITECTURE.md](../ARCHITECTURE.md)
- [DESIGN-GUIDE.md](../DESIGN-GUIDE.md)
- [REPO-STANDARDS.md](../REPO-STANDARDS.md)
- [2026-08-01-mcp-spec-2026-07-28.md](./2026-08-01-mcp-spec-2026-07-28.md) -- prior MCP audit
- [2026-07-05-usage-observability-code-size.md](./2026-07-05-usage-observability-code-size.md) -- prior code size audit

## Purpose

Point-in-time audit triggered by the release of Rust 1.97.1, covering four axes:

1. **Rust 1.97 toolchain impact** -- what stabilized, what it means for this workspace, whether the MSRV should be bumped
2. **Dependency freshness** -- latest versions of all workspace dependencies; breaking changes on the horizon
3. **MCP 2026-07-28 conformance re-assessment** -- prior audit was against v0.26.1 / rmcp 2.2.0; re-assessed against v0.27.0 / rmcp 3.1.0
4. **Codebase quality** -- new findings not covered by prior audits (test gaps, UX, lint promotion)

## Methodology

Three parallel read-only scouts with dedicated domains:

- **Rust 1.97 scout** (`aws_bedrock / claude-sonnet-5`): Rust blog + releases.rs, crates.io dependency checks, context7 rmcp docs
- **Codebase scout** (`aws_bedrock / claude-sonnet-5`): `aptu-coder` AST tools on live source; prior audit cross-reference
- **MCP scout** (`aws_bedrock / claude-sonnet-5`): context7 rmcp 3.x API surface, rust-sdk README/ROADMAP.md, brave_search for spec updates

Each scout wrote a structured JSON handoff. This document synthesizes all three, with guard-style cross-checks noted where scouts disagreed.

No code was modified during this audit.

---

## Part 1: Rust 1.97.1 Toolchain Impact

### 1.1 What stabilized in Rust 1.97

*Table 1: Rust 1.97.0 / 1.97.1 changes assessed for workspace relevance.*

| Change | Source | Applies? | Notes |
|---|---|---|---|
| v0 symbol mangling becomes default on stable | [blog.rust-lang.org/2026/07/09](https://blog.rust-lang.org/2026/07/09/Rust-1.97.0/) | Yes | Behavioral; no code change needed. May affect old debugger symbol demangling. |
| `linker_messages` lint enabled by default | [blog.rust-lang.org/2026/07/09](https://blog.rust-lang.org/2026/07/09/Rust-1.97.0/) | Yes | Could introduce new CI warnings from lld/ld on ARM64. Must verify after MSRV bump. |
| `cargo build.warnings` config stable | [blog.rust-lang.org/2026/07/09](https://blog.rust-lang.org/2026/07/09/Rust-1.97.0/) | Yes | Offers cache-friendly alternative to `RUSTFLAGS=-D warnings`; optional CI improvement. |
| `Result<T, Uninhabited>` / `ControlFlow<Uninhabited, T>` `must_use` equivalence | [releases.rs/docs/1.97.0](https://releases.rs/docs/1.97.0/) | No | Workspace has no `Result<_, Infallible>` patterns; thiserror enums are always inhabited. |
| `let chains` (if let && condition) | [releases.rs/docs/1.88.0](https://releases.rs/docs/1.88.0/) | No | **Not a 1.97 feature.** Stabilized in 1.88.0 (edition 2024). Already available under the current MSRV 1.96.0. |
| 1.97.1 patch: LLVM miscompilation fix | [blog.rust-lang.org/2026/07/16](https://blog.rust-lang.org/2026/07/16/Rust-1.97.1/) | Yes | Correctness fix. 1.97.0 increased the likelihood of triggering a known LLVM bug present since 1.87. Local toolchain already 1.97.1. |

**Assessment**: No stabilized 1.97 language feature is required to compile or correctly run this workspace. No source code changes are triggered by the toolchain upgrade.

### 1.2 MSRV bump recommendation

Current: `rust-version = "1.96.0"` in `Cargo.toml`. Local toolchain: `rustc 1.97.1`.

**Recommendation: bump to 1.97.1** (not 1.97.0).

Rationale:

1. The installed local compiler is 1.97.1; CI will diverge from the pinned MSRV at the next clean install unless the pin is updated.
2. 1.97.0 introduced v0 symbol mangling and the `linker_messages` lint; these should be validated under CI explicitly, which requires the MSRV to reflect what CI actually runs.
3. 1.97.1 fixes a real LLVM miscompilation (present since 1.87). Pinning to 1.97.1 over 1.97.0 avoids re-opening that window.
4. No source code changes are required alongside the version string bump.

**Issue label**: `chore(rust)` | **Complexity**: trivial | **PR group**: `rust197-msrv`

### 1.3 CI impact of `linker_messages`

After the MSRV bump, the first CI run on `ubuntu-24.04-arm` should be inspected for new `linker_messages` warnings from lld or the system ld. If noisy, add to `[lints.rust]` in the affected crate's `Cargo.toml`:

```toml
linker_messages = "allow"
```

No preemptive change is needed; the lint must be observed first.

### 1.4 `cargo build.warnings` as a CI optimization (optional)

The new stable `build.warnings = "deny"` in `[build]` of `.cargo/config.toml` achieves the same effect as `cargo clippy -- -D warnings` without invalidating the incremental build cache when unrelated files change. This is a CI workflow optimization, not a correctness change. A dedicated spike PR would be needed to evaluate it.

---

## Part 2: Dependency Freshness

*Table 2: Workspace dependency status, confirmed against Cargo.lock (v0.27.0) and crates.io / GitHub.*

| Crate | Workspace pin | Cargo.lock resolved | Latest confirmed | Breaking? | Action |
|---|---|---|---|---|---|
| `rmcp` | `"3"` | `3.1.0` | `3.1.0` | n/a | None |
| `schemars` | `"1"` | `1.2.2` | `1.2.2` | n/a | None |
| `thiserror` | `"2.0.18"` | `2.0.19` | `2.0.19` | No | Cosmetic: raise pin floor to `"2.0.19"` |
| `opentelemetry` stack | `"0.32"` | `0.32.0` | `0.32.0` | n/a | None |
| `petgraph` | `"0.8"` | `0.8.3` | `0.8.3` | No | Tracking: 0.9 is a breaking multi-crate rewrite (unreleased); open tracking issue |
| `axum` | `"0.8"` | `0.8.9` | `0.8.9` | No | Tracking: 0.9 is in progress (unreleased); no action now |
| `tree-sitter` core | `"0.26.6"` | `0.26.11` | `0.26.11` | No | Cosmetic: raise floor pin to `"0.26.11"` |
| `tree-sitter-rust` grammar | `"0.24.2"` | `0.24.2` | unconfirmed | -- | **Flag**: grammar is 5 minor versions behind core (0.24 vs 0.26); dedicated grammar audit needed |
| `tokio` | `"1"` | `1.53.1` | unconfirmed | n/a | None (floor pin tracks minor) |
| `rayon` | `"1"` | `1.12.0` | unconfirmed | n/a | None (floor pin tracks minor) |
| `tracing-opentelemetry` | `"0.33"` | `0.33.x` | -- | No | Normal versioning; stays one minor ahead of otel core |

### 2.1 Notable observations

**thiserror pin drift**: Workspace pins `"2.0.18"` but Cargo.lock already resolves `2.0.19`. The pin floor should be raised to match the lock file. Cosmetic, no functional change.

**tree-sitter grammar lag**: `tree-sitter-rust` is pinned at `0.24.2` while the core `tree-sitter` library has advanced to `0.26.11`. Grammar crates have independent versioning, but a 5-minor-version gap between the grammar and core could lead to API incompatibilities if core changes its ABI. A dedicated grammar audit pass should check each of the 12 grammar crates (`tree-sitter-rust`, `tree-sitter-go`, `tree-sitter-cpp`, `tree-sitter-c-sharp`, `tree-sitter-java`, `tree-sitter-kotlin-ng`, `tree-sitter-python`, `tree-sitter-typescript`, `tree-sitter-javascript`, `tree-sitter-fortran`, `tree-sitter-md`, `tree-sitter-css`, `tree-sitter-yaml`) against their own crates.io latest.

**petgraph 0.9 horizon**: The 0.9 release (multi-crate split into `petgraph-core`, `petgraph-graph`, etc.) is in progress on the trunk branch but not yet published to crates.io. `crates/aptu-coder-core/src/graph/` uses `petgraph 0.8` with `serde-1` feature. This will require a migration pass when 0.9 ships. A tracking issue should be opened now.

**axum 0.9 horizon**: Similar horizon note; the 0.8.x branch is current. No action until 0.9 ships.

---

## Part 3: MCP 2026-07-28 Conformance Re-Assessment

### 3.1 Change since prior audit (v0.26.1 / rmcp 2.2.0 -> v0.27.0 / rmcp 3.1.0)

*Table 3: Prior audit findings, updated status.*

| Finding | Description | Prior status | Current status |
|---|---|---|---|
| C01 | McpLoggingLayer dead code | REMOVE | **DONE** (#1351) |
| C02 | `initialize`/`on_initialized` handlers | BLOCKED | Still BLOCKED (see §3.3) |
| C03 | Hardcoded protocol version string | BLOCKED | **DONE** (#1352) -- but see MC-1 |
| C04 | `server/discover` RPC | OPPORTUNITY | **DONE** (#1354) |
| C05 | `resultType` field on `CallToolResult` | BLOCKED | **N/A** -- reclassified (see §3.4) |
| C06 | `Mcp-Method`/`Mcp-Name` headers | BLOCKED | Still BLOCKED |
| C07 | `ttlMs`/`cacheScope` on list results | BLOCKED | Still BLOCKED |
| C08 | W3C Trace Context | CLEAN | CLEAN |
| C09 | Stateless HTTP transport | CLEAN | CLEAN |
| C10 | No Sampling/Roots/DynReg | CLEAN | CLEAN |
| C11 | HTTP+SSE transport not in use | CLEAN | CLEAN |
| C12 | No legacy error codes | CLEAN | CLEAN |
| C13 | `no_cache_meta()` aptu-specific | CLEAN | CLEAN |

### 3.2 rmcp 3.1.0 API surface (confirmed via context7 / docs.rs)

rmcp 3.x targets MCP 2026-07-28 while retaining 2025-11-25 backward compatibility. Key confirmed facts:

- `ProtocolVersion::LATEST` remains `V_2025_11_25` in rmcp 3.1.0 (KNOWN_VERSIONS includes V_2026_07_28, but LATEST has not been bumped)
- `ServerHandler::initialize` and `on_initialized` are still present with non-deprecated signatures
- `ServerHandler::discover` and `DiscoverResult` are now present (SEP-2575 partially)
- `InputRequiredResult` / `RequestStateCodec` are present (SEP-2322 MRTR)
- `ListToolsResult` struct: fields `{meta, next_cursor, tools}` -- no `ttl_ms`/`cache_scope` first-class fields
- `Mcp-Method`/`Mcp-Name` header constants: not documented in the public `http_header` module
- Conformance suite (ROADMAP.md): Server 97.5% (39/40), Client 90.6% (29/32)
- `schemars` 1.2.2 confirmed to emit JSON Schema 2020-12 by default (SEP-2106 satisfied)
- Legacy (pre-Streamable-HTTP) transport removed from rmcp 3.x; `NeverSessionManager` still correct for stateless serving

### 3.3 C02 -- STILL BLOCKED: `initialize`/`on_initialized` handler removal

**Status**: Blocked on rmcp. `ServerHandler` in rmcp 3.1.0 still requires/exposes both methods.

Stateless serving per SEP-2567 is handled transport-side: once a client negotiates protocol version >= 2026-07-28, rmcp automatically omits `Mcp-Session-Id` and the standalone GET/DELETE stream. The application-level `initialize`/`on_initialized` overrides in `crates/aptu-coder/src/lib.rs` (lines 690-734, 759-768) still compile and run correctly.

Monitor: modelcontextprotocol/rust-sdk PRs #973/#943 and issue #977 (conformance epic).

### 3.4 C05 -- RECLASSIFIED: `resultType` is N/A for aptu-coder

MRTR (`InputRequiredResult`) exists in rmcp 3.x, but all seven aptu-coder tools are single-round-trip: they receive input, execute, and return a complete result with no need to request additional client input. `resultType: "complete"` would be the only value ever emitted. This is N/A rather than blocked; close C05 in issue #998 tracking.

### 3.5 MC-1 -- OPPORTUNITY: ProtocolVersion::LATEST advertises 2025-11-25

*Severity*: medium informational

The C03 fix (#1352) correctly replaced the hardcoded string with `ProtocolVersion::LATEST`. However, `LATEST` in rmcp 3.1.0 resolves to `V_2025_11_25`, not `V_2026_07_28`. The server therefore advertises `2025-11-25` as its default protocol version even though rmcp 3.1.0 can negotiate `2026-07-28` when a client requests it explicitly (via version negotiation).

**No immediate code change required.** This will self-resolve when rmcp bumps `LATEST` to `V_2026_07_28`. Track against rust-sdk#869/#977.

If desired before rmcp bumps LATEST, the server could explicitly advertise `V_2026_07_28` in `get_info()` -- but this risks incompatibility with clients that only speak 2025-11-25. Do not change without a two-client integration test.

### 3.6 MC-2 -- CLEAN: JSON Schema 2020-12 confirmed

`schemars` 1.2.2 (Cargo.lock) defaults to JSON Schema 2020-12 per docs.rs. SEP-2106 is satisfied. Update the prior audit's `json_schema_status` field from "unconfirmed" to "2020-12 confirmed".

### 3.7 MCP Resources conformance (new surface -- #1367/#1368)

The knowledge graph MCP resources were not present at the time of the prior audit. Two conformance gaps found:

**MC-3 (low)**: `list_resources_impl` and `list_resource_templates_impl` both accept `Option<PaginatedRequestParams>` but ignore the cursor entirely. The top-level resource catalog is currently small (graph query templates only), so this is a non-issue today. Should be documented as a known limitation or fixed if the resource catalog grows.

**MC-4 (blocked)**: `resultType` is absent from `ReadResourceResponse` / `ListResourcesResult` construction in `resources.rs`. Blocked on rmcp exposing a typed way to set it; same status as the tool-level C05 which is now N/A for tools but would be applicable for resources once the API exists.

### 3.8 SEP status summary

*Table 4: SEP disposition as of rmcp 3.1.0 / v0.27.0.*

| SEP | Title | rmcp status | aptu action |
|---|---|---|---|
| SEP-2575 | Remove initialize handshake; server/discover | Pending | Blocked |
| SEP-2567 | Remove Mcp-Session-Id; sessionless | Transport-side done; trait pending | Done (already stateless) |
| SEP-2322 | MRTR + resultType | Implemented in rmcp | N/A for aptu tools |
| SEP-2243 | Mcp-Method/Mcp-Name headers | rmcp-internal; constants not public | Blocked |
| SEP-2549 | ttlMs/cacheScope; subscriptions/listen | Partial; no first-class struct fields | Blocked |
| SEP-2577 | Deprecate Roots, Sampling, Logging | Implemented | Done (never used; logging removed) |
| SEP-2133 | Extensions framework | Implemented for Tasks ext | N/A (no custom extension use case) |
| SEP-2106 | JSON Schema 2020-12 | Implemented via schemars 1.x | Done (confirmed) |
| SEP-2663 | Tasks extension | Implemented | N/A (all tools synchronous) |

---

## Part 4: Codebase Quality

### 4.1 New findings

#### CB-1 -- Test gap (medium): Knowledge graph MCP resources have no integration-test coverage

**File**: `crates/aptu-coder/src/tools/resources.rs`

The MCP resource surface (`list_resources_impl`, `list_resource_templates_impl`, `read_resource_impl`) added in #1367/#1368 is only exercised by unit tests inside `resources.rs` itself (`test_parse_graph_uri_*`, `test_read_resource_impl_cold_cache_miss`, etc.). No file under `crates/aptu-coder/tests/` exercises these functions through the real MCP protocol handshake using `make_test_analyzer()` / `call_tool_raw`.

**Action**: Add an integration test that (1) runs `analyze_symbol` on a small fixture directory to warm the graph cache, (2) calls `list_resources` / `list_resource_templates` via the MCP harness, (3) reads a graph resource URI end-to-end via `read_resource` and asserts on the paginated JSON payload. Test must cover both the cold-cache-miss path and a warm-cache read.

**Issue**: new | **PR group**: `test-coverage-knowledge-graph`

---

#### CB-2 -- UX (low): `analyze_symbol` description omits L2 disk cache and graph integration

**File**: `crates/aptu-coder/src/lib.rs`, `analyze_symbol` `#[tool]` description block

The description covers call-graph modes, `import_lookup`, `def_use`, `match_mode`, and `git_ref`, but says nothing about:
- The L2 on-disk call-graph cache (`APTU_CODER_DISK_CACHE_DIR` / `APTU_CODER_DISK_CACHE_DISABLED`)
- The fact that calling `analyze_symbol` populates the structural graph consumed by the `aptu-coder://` MCP resource templates (blast-radius, import-closure, subgraph)

A caller reading only the tool description has no signal that running `analyze_symbol` first is what makes graph resources non-cold.

**Action**: Extend the description with a clause covering the L2 disk cache and graph-population side effect.

**PR group**: `docs-tool-descriptions`

---

#### CB-3 -- UX (low): `exec_command` description omits `drain_timeout_secs`

**File**: `crates/aptu-coder/src/lib.rs`, `exec_command` `#[tool]` description block

The description covers output caps, `working_dir`, `stdin`, and heredoc rejection, but omits `drain_timeout_secs`. A caller cannot discover this parameter's purpose without reading the JSON schema field description.

**Action**: Add a short clause, e.g. `drain_timeout_secs controls how long to wait for background processes holding stdout/stderr open after the child exits (default 500ms)`.

**PR group**: `docs-tool-descriptions`

---

#### CB-4 -- Lint promotion evidence (low): warn-lint violations not visible in current source

**File**: `Cargo.toml` (issue #1225)

Manual source inspection across `cache.rs`, `edit.rs`, `call_graph.rs`, `structural.rs`, and all `pub fn` signatures in `analyze.rs`, `analyze_focused.rs`:

- No un-annotated `needless_pass_by_value` violations found (all existing `#[allow]` sites have inline justification comments)
- No `large_enum_variant` candidates found (all inspected enum variants hold Strings or small primitives, none approaching 200 bytes)
- No `redundant_clone` patterns found (all `Arc::clone` sites are reference-count bumps, not deep clones)
- 11 of 12 sampled `pub fn` in `cache.rs`, `cache_disk.rs`, `call_graph.rs` already carry `#[must_use]`

**Gap**: `DiskCache::drain_write_failures` and `DiskCache::is_degraded` in `cache_disk.rs` (see CB-5) are the only confirmed `must_use_candidate` violations.

**Action**: Run `cargo clippy -p aptu-coder-core -p aptu-coder -- -D warnings` locally to get an authoritative zero-violation confirmation before promoting any lint from `warn` to `deny` in issue #1225. This audit's finding provides supporting evidence, not a replacement for a CI run.

**PR group**: `lint-promotion-1225`

---

#### CB-5 -- Lint / idiom (low): `DiskCache::drain_write_failures` and `DiskCache::is_degraded` missing `#[must_use]`

**File**: `crates/aptu-coder-core/src/cache_disk.rs` (lines ~42-48)

Both methods return values whose only purpose is inspection. Sibling method `cache_stats` on the same `impl` block carries `#[must_use]`. Discarding the return value of `drain_write_failures` (a drained counter) or `is_degraded` (a health boolean) at a call site is very likely a silent bug.

**Action**: Add `#[must_use]` to both methods. Verify no existing call site discards the return value.

**PR group**: `lint-promotion-1225`

---

#### CB-6 -- Docs (low): AGENTS.md understates working_dir confinement on edit tools

**File**: `AGENTS.md`

AGENTS.md describes `working_dir` on `edit_overwrite` / `edit_replace` as "a path-resolution convenience only; path confinement is the operator responsibility." In practice, `validate_path_relative_to` (called from `edit_overwrite.rs:28` with `root=working_dir`) canonicalizes the resolved parent and rejects it if it does not `start_with(root)` -- the code actively enforces confinement, not merely documents it as a convention.

`exec_command`'s `working_dir` intentionally has no such confinement (general shell escape hatch by design).

**Action**: Clarify AGENTS.md to state that `working_dir` on edit tools is confinement-enforced via `canonicalize + starts_with`, and distinguish this from `exec_command`'s `working_dir` which has no equivalent confinement.

**PR group**: `docs-clarification`

---

### 4.2 Open issues from prior audits -- status update

*Table 5: Prior audit findings not re-created, status as of this audit.*

| ID | Title | Status | Notes |
|---|---|---|---|
| O1 | `analyze_symbol` emits no error metrics | Likely resolved | Source shows 10 `emit_error_metric` call sites + dedicated integration test file `analyze_symbol_error_metrics_tests.rs`. Flagged as unclear; confirm by reading the original issue. |
| O2/O3/O4 | 19 unmetricated parameters | Still open | No evidence of closure from this audit pass. |
| O5 | Cache L1 evictions / L2 size | Still open | `eviction_count()` and `cache_stats()` accessors exist in source; unclear whether metrics.rs emits them as JSONL. |
| S1 | Split `analyze.rs` | Still open | `analyze.rs` 1,055 LOC + `analyze_focused.rs` 1,088 LOC; partial split has occurred but splitting not complete. |
| S2 | Split `parser.rs` | Still open | `parser.rs` 1,038 LOC + `parser_elements.rs` 918 LOC; partial split done. |
| S3-S7 | Other code splitting | Still open | S3 (`tests.rs`), S4 (`metrics.rs`), S5 (tool files), S6 (`shell_write.rs`), S7 (large functions). |

---

## Part 5: PR Grouping and Sequencing

*Table 6: Recommended PRs, priority order.*

| # | PR title | Issue # | Findings | Priority | Blocked on | Complexity |
|---|---|---|---|---|---|---|
| 1 | `chore(rust): bump workspace rust-version to 1.97.1` | new | §1.2 | High | none | trivial |
| 2 | `test(resources): integration tests for MCP resource surface` | new | CB-1 | High | none | simple |
| 3 | `fix(cache): add #[must_use] to DiskCache::drain_write_failures and is_degraded` | new | CB-5 | Low | none | trivial |
| 4 | `docs(tools): update analyze_symbol and exec_command descriptions` | new | CB-2, CB-3 | Low | none | trivial |
| 5 | `docs(agents): clarify working_dir confinement on edit tools` | new | CB-6 | Low | none | trivial |
| 6 | `chore(deps): raise thiserror floor pin to 2.0.19; raise tree-sitter floor pin to 0.26.11` | new | §2.1 | Low | none | trivial |
| 7 | `chore(deps): grammar crate version audit` (spike) | new | §2.1 | Medium | none | medium |
| 8 | `chore(deps): open petgraph 0.9 tracking issue` | new | §2.1 | Low | petgraph 0.9 release | n/a |
| 9 | `chore(mcp): update #998 with rmcp 3.1.0 assessment + C05 reclassification` | #998 | §3.3, §3.4 | Medium | none | trivial |
| 10 | `chore(mcp): implement list_resources / list_resource_templates pagination` | new | MC-3 | Low | none | simple |

PRs 1, 2, 3, 4, 5, 6 have no dependencies on each other and can be opened in parallel. PR 7 (grammar audit) should precede any grammar version bumps. PR 9 updates issue #998 rather than closing it; #998 closes when rmcp resolves C02/C06/C07.

### Merge order within the PR set

```text
PR 1 (rust-version bump) -> verify CI passes -> merge
PR 6 (dep pin cosmetics) -> merge alongside PR 1 or after
PR 3, 4, 5 -> trivial; merge in any order
PR 2 (integration tests) -> merge before any resource API changes
PR 7 (grammar audit) -> spike; output determines if grammar PRs follow
PR 9 (issue update) -> update, not a code PR
PR 10 (resource pagination) -> after PR 2 is merged
```

---

## Summary

*Table 7: Finding counts by category.*

| Category | Count | Severity breakdown |
|---|---|---|
| Rust 1.97 impact | 6 changes assessed | 1 actionable (MSRV bump), 2 monitoring, 3 no-action |
| Dependency updates | 10 crates checked | 2 cosmetic pin raises, 2 horizon tracking, 6 no-action |
| MCP: completed since prior audit | 4 findings | C01, C03, C04 done; C05 reclassified N/A |
| MCP: still blocked | 3 findings | C02, C06, C07 |
| MCP: new informational | 4 findings | MC-1 (LATEST=2025-11-25), MC-2 (JSON Schema clean), MC-3 (resource pagination), MC-4 (resultType blocked) |
| Codebase: new actionable | 6 findings | CB-1 through CB-6 |
| Codebase: not-findings | 13 items | Documented below |

**The workspace is in good health.** No breaking changes from Rust 1.97.1. No open breaking dependency updates. MCP 2026-07-28 alignment is complete on all unblocked findings; the three remaining blocked findings (C02, C06, C07) require rmcp to ship the corresponding API surface. The primary new technical debt is the missing integration-test coverage for the knowledge graph MCP resources (CB-1), added in the most recent feature cycle.

## Not-Findings

*Table 8: Items checked and confirmed clean.*

| Item | Result |
|---|---|
| `extern crate` declarations | None; edition 2024 paths throughout |
| `async fn` in trait definitions | None; async-trait removal complete |
| `-> impl Trait` return positions | None |
| `push_str(&format!(...))` allocation anti-pattern in formatters | None; `writeln!`/`write!` into buffer already used |
| `double .clone().clone()` chains | None |
| Arc-backed field clones in lib.rs | Cheap `Arc::clone` reference-count bumps; not deep clones |
| `.unwrap()` / `.expect()` in non-test code | All have `#[allow(clippy::expect_used)]` with justification comment; all `.unwrap()` confined to `#[cfg(test)]` |
| Stale-context circuit breaker -- end-to-end test coverage | Covered in `edit_replace_errors.rs` (7 tests) |
| `replace_all` edge cases | Covered in `edit_replace_errors.rs` |
| `drain_timeout_secs` input validation | `exec_command.rs:200-209` rejects negative values with `INVALID_PARAMS` |
| `git_ref` shell injection | `traversal.rs` validates: rejects empty, leading `-`, forbidden chars |
| Cold-cache graph resource message accuracy | `resources.rs` returns `RESOURCE_NOT_FOUND` with "call analyze_symbol... first" message, matches AGENTS.md |
| working_dir confinement on edit tools | `validate_path_relative_to` enforces `canonicalize + starts_with`; stronger than AGENTS.md wording suggests (see CB-6) |

---

## Open Issues Cross-Reference

| Issue | Title | Relation to This Audit |
|---|---|---|
| [#998](https://github.com/clouatre-labs/aptu-coder/issues/998) | chore: migrate to MCP 2026-07-28 once rmcp v2 ships | Update with rmcp 3.1.0 status; C05 reclassified N/A; C02/C06/C07 still blocked |
| [#1225](https://github.com/clouatre-labs/aptu-coder/issues/1225) | Lint promotion | CB-4/CB-5 provide supporting evidence; run full clippy before promoting |
| new | test(resources): integration tests for graph MCP resources | CB-1 |
| new | fix(cache): #[must_use] on DiskCache methods | CB-5 |
| new | docs(tools): analyze_symbol and exec_command description updates | CB-2, CB-3 |
| new | docs(agents): clarify working_dir confinement | CB-6 |
| new | chore(rust): bump rust-version to 1.97.1 | §1.2 |
| new | chore(deps): grammar crate version audit | §2.1 |
| new | chore(deps): petgraph 0.9 tracking | §2.1 |
| new | chore(mcp): resources/list and resources/templates/list pagination | MC-3 |
