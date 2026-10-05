# Audit: GitHub Actions Minutes -- October 2026

Date: 2026-10-05  
Commit: c868be7  
Version: v0.39.0  
Toolchain: Rust 1.98.1 / rmcp 3.3.0 / tokio async

## Purpose

Measure GitHub Actions consumption across all 15 workflows over the last 10-30 runs per workflow and reduce waste while keeping functionality, security posture, and merge-gate guarantees identical. The repo is public, so standard-runner minutes are unbilled; the optimization target is wall-clock and queue time, with billed exposure limited to release-day macOS minutes.

## Methodology

- Repo context first: `gh repo view` (public), `gh run list`, ruleset and branch-protection reads, clean tree on `origin/main`
- Job-level durations from `gh api repos/{owner}/{repo}/actions/runs/{id}/jobs` (`completed_at - started_at`), 15 runs per workflow
- Executed-run averages separated from skipped runs (skipped jobs report near-zero duration and skew naive means)
- Frequency per workflow over 30 days via `gh run list --created`

---

## Baseline

Job averages for executed runs (15-run samples):

| Workflow | Job | Executed avg | Skipped share |
|---|---|---|---|
| CI | Coverage | 190s | 13/15 |
| CI | MCP Server Conformance | 103s | 13/15 |
| CI | Lint | 68s | 13/15 |
| CI | MSRV Check | 65s | 13/15 |
| CI | semver checks | 47s | 10/15 |
| CI | Bench Harness Unit Tests | 31s | 14/15 |
| OpenSSF Scorecard | Scorecard Analysis | 33s | 9/15 |
| Security | Security Result | 17s | 2/15 |
| REUSE Compliance | reuse | 13s | 2/15 |

PR critical path: `changes` (6s) to the parallel compute fan-out, bounded by Coverage at 190s, then `CI Result` (3s): roughly 3.5 minutes per code-touching PR.

## Findings

| # | Category | Finding | Verdict | PR |
|---|---|---|---|---|
| 1 | Scheduled compute | Weekly update-deps workflow broken and redundant with Renovate | REMOVE | [#1737](https://github.com/clouatre-labs/aptu-coder/pull/1737) |
| 2 | Duplication | Four separate Rust builds (Lint, Coverage, MSRV, Conformance) on distinct cache keys | KEEP | n/a |

### F1 -- Remove update-deps workflow (REMOVE)

**Source:** aptu-coder `update-deps.yml` (deleted in [#1737](https://github.com/clouatre-labs/aptu-coder/pull/1737))

The workflow failed 4 of its last 5 weekly runs: it pushed `HEAD` to `chore/update-cargo-lock` while the local branch remained `main`, so `gh pr create` aborted with "you must first push the current branch to a remote, or use the --head flag". No lockfile PR has been opened since August. Even a successful run could not have validated the lockfile: PRs created with `GITHUB_TOKEN` do not trigger workflows, so the claimed ci.yml validation never ran. Renovate already opens and merges dependency PRs (#1734, #1735, #1736), making the workflow redundant. Removed entirely rather than patched (KISS). This also closes the F4 trim item from [2026-10-01-ci-adoption-from-goose.md](2026-10-01-ci-adoption-from-goose.md), which flagged the same workflow's scheduled compute.

**Accepted trade-off:** weekly automated `cargo update` no longer runs; Renovate PRs remain the dependency-update path.

### F2 -- Duplicate Rust builds across CI jobs (KEEP)

**Source:** aptu-coder ci.yml (Lint, Coverage, MSRV Check, MCP Server Conformance)

Four jobs each compile the workspace with distinct `Swatinem/rust-cache` shared keys: measured duplication of roughly 4x compile work per code PR. On the public repo this compute is unbilled, the four jobs run in parallel so wall-clock is unaffected, and consolidating them would serialize the critical path and lengthen it. Deliberately left alone.

---

## Already Optimized

Waste patterns checked and not present:

- No idle or polling runners: the only readiness loop is the conformance suite polling its own locally started server, bounded at 60s
- No stub or echo jobs propping up required checks: `CI Result` is the sole required status check, produced by the in-workflow aggregate job; the ruleset requires only `DCO` (external app)
- No trigger-level `paths:` filters gating required checks: ci.yml runs on all PRs and gates expensive jobs internally via `dorny/paths-filter`
- `concurrency` with `cancel-in-progress` present on PR-triggered workflows
- Renovate-actor skips, main-only cache saves, SHA-pinned actions with version comments, ARM-only runners throughout
- No draft-PR or bot-PR full-matrix exposure: expensive jobs skip the renovate actor via the changes gate

## Outcome

Weekly scheduled compute drops by one job (48 lines of workflow removed); per-PR wall-clock is unchanged by design. The pipeline's only measured defect was the broken workflow itself; no gating, security, or merge-gate behavior changed.
