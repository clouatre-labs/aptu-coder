# Audit: CI Adoption from block/goose -- October 2026

Date: 2026-10-01  
Commit: 199a99a  
Version: v0.38.0  
Toolchain: Rust 1.98.0 / rmcp 3.3.0 / tokio async

## See Also

- [REPO-STANDARDS.md](../REPO-STANDARDS.md) -- contribution and CI standards
- [block/goose mcp-conformance.yml](https://github.com/block/goose/blob/main/.github/workflows/mcp-conformance.yml) -- reference implementation
- [modelcontextprotocol/conformance](https://github.com/modelcontextprotocol/conformance) -- official conformance suite

## Purpose

Cross-repo review of block/goose CI to identify patterns worth adopting into aptu-coder while keeping CI fast, lean, and KISS. The repo rule is add-one-remove-one: any CI addition must be offset by a removal or trim so total compute stays flat. All findings were validated against live sources (`gh api` on block/goose and modelcontextprotocol/conformance, dorny/paths-filter documentation) before being turned into issues.

## Methodology

- Discovery: two parallel read-only scouts, one cataloging goose's ~40 workflow files, one inventorying all 15 aptu-coder workflows
- Validation: direct `gh api` reads of the referenced goose workflows, the modelcontextprotocol/conformance README (server suite, `--spec-version`, `--expected-failures` flags), and dorny/paths-filter docs
- Verdicts: ADOPT / NOT ADOPT, each with the concrete cost or benefit

---

## Summary Table

| # | Category | Finding | Verdict | Issue |
|---|---|---|---|---|
| 1 | Conformance | MCP 2026-07-28 server conformance check via official suite with expected-failures baseline | ADOPT | [#1717](https://github.com/clouatre-labs/aptu-coder/issues/1717) |
| 2 | Duplication | publish-registry.yml and publish-registry-manual.yml are ~90% duplicated | REMOVE (consolidate) | [#1718](https://github.com/clouatre-labs/aptu-coder/issues/1718) |
| 3 | Gating | pytest-bench runs unconditionally on every push and PR | TRIM | [#1719](https://github.com/clouatre-labs/aptu-coder/issues/1719) |
| 4 | Scheduled compute | Weekly update-deps runs a full cold cargo test that the opened PR re-validates anyway | TRIM | [#1720](https://github.com/clouatre-labs/aptu-coder/issues/1720) |

The two trims and the consolidation offset the conformance addition, keeping scheduled and per-PR compute flat.

---

## Already At Parity

Goose patterns aptu-coder already has; no action taken:

- `dorny/paths-filter` change gate with a `changes` job (ci.yml:45); goose's biggest single win is already in place
- `Swatinem/rust-cache` with save restricted to main
- `concurrency` with `cancel-in-progress` per head ref
- MSRV job driven from `rust-version` in Cargo.toml
- SHA-pinned third-party actions with version comments
- Per-workflow `permissions: contents: read` minimum
- cargo-deny (advisories and licenses); cargo-machete was considered and rejected because cargo-deny plus the package-age check already cover the dependency surface and machete would duplicate maintenance for marginal signal
- ARM-only runners (`ubuntu-26.04-arm`), so regular CI billing exposure is zero on the public repo; only release-day macOS minutes are billable

---

## Finding Detail

### F1 -- MCP 2026-07-28 server conformance job (ADOPT)

**Source:** block/goose `mcp-conformance.yml`; official `@modelcontextprotocol/conformance` npm suite

Goose builds its binaries once, uploads them as a 1-day-retention artifact, and fans out a matrix of (spec-version x suite-version) conformance runs, each tolerating known failures via an in-repo YAML baseline passed as `--expected-failures`. Verified that the same suite supports server testing: `npx @modelcontextprotocol/conformance server --url <url> --spec-version 2026-07-28 --expected-failures <path>`.

aptu-coder is an MCP server on rmcp; its CI verifies behavior only through its own tests, which cannot catch protocol-level drift after a rmcp upgrade. The 2026-07-28 spec work is recent and already invested in (docs/audit/2026-08-01-mcp-spec-2026-07-28.md), so conformance is the verification lock for that investment.

KISS adaptation: one job in ci.yml (no new workflow, no matrix), single spec-version and single suite-version, gated by the existing paths-filter `changes` output, wired into the CI Result aggregator. Requires starting the server with the rmcp streamable-HTTP transport before invoking the suite. The baseline YAML records tolerated failures explicitly with reasons instead of skipping tests.

**Cost:** one additional ARM job on code-touching PRs. **Offset:** F3 and F4.

### F2 -- Consolidate publish-registry workflows (REMOVE)

**Source:** aptu-coder `publish-registry.yml` (119 lines) vs `publish-registry-manual.yml` (116 lines)

Both contain near-verbatim copies of the MCPB SHA loop, server.json jq construction, and the same pinned mcp-publisher install; they differ only in version sourcing (explicit input vs auto-detect from the latest release). Fixes must land twice; supply-chain pins can drift between the two files. Merge into one workflow_dispatch workflow with an optional version input and a dry-run default, and delete the manual variant. Zero billing impact; pure maintenance and consistency win.

### F3 -- Gate pytest-bench behind the changes filter (TRIM)

**Source:** aptu-coder ci.yml:355-374

`pytest-bench` runs unconditionally on every push and PR while every sibling compute job is gated by `needs.changes.outputs.code`. Docs-only PRs pay for a job their changes cannot affect. Gate it with the exact sibling condition and confirm the CI Result aggregator treats skipped jobs as success.

### F4 -- Drop redundant cargo test from weekly update-deps (TRIM)

**Source:** aptu-coder update-deps.yml:29-32

The weekly lockfile job runs a full `cargo test` with no cache; at 7-day cache eviction it is guaranteed a cold full-workspace build every Monday, for a result recomputed by ci.yml on the PR it opens. Remove the test step and keep `cargo update` plus PR creation unchanged.

---

## Explicitly Not Adopted

Validated as heavyweight for a two-crate repo:

- Multi-spec conformance matrix (3 spec x suite combos); one combination suffices
- Build-once artifact handoff between jobs; only relevant with multiple consumers
- Splitting fmt, clippy, and test into three parallel jobs; goose does this, but splitting our combined jobs adds jobs and runner minutes for no wall-clock gain at this repo size
- cargo-machete as a standalone workflow; superseded by existing cargo-deny coverage
- TLS-feature matrices, wasm32 target job, Windows main-only build; we do not ship those variants
- Scorecard/attest/Sigstore additions beyond the existing scorecard.yml; already present where warranted
- quarantine.yml, hermit/just toolchain layering, V8 marker repair; maintainer-scale or monorepo-specific machinery

---

## Resolution

All four findings are tracked as issues and intended for the next CI maintenance window. Resolution order: F3 and F4 first (immediate savings), then F2 (consolidation), then F1 (addition, landing against a leaner baseline).
