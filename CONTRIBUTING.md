# Contributing to aptu-coder

We welcome contributions! This document covers the essentials.

## Quick Start

```bash
git clone https://github.com/YOUR_USERNAME/aptu-coder.git
cd aptu-coder
cargo build
cargo test
```

## Before Submitting

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo deny check advisories licenses
```

## Commit Message Format

We follow [Conventional Commits](https://www.conventionalcommits.org/) format:

```
<type>(<scope>): <subject>

<body>

<footer>
```

### Types

- **feat**: A new feature
- **fix**: A bug fix
- **docs**: Documentation only changes
- **refactor**: A code change that neither fixes a bug nor adds a feature
- **test**: Adding missing tests or correcting existing tests
- **chore**: Changes to build process, dependencies, or tooling

### Examples

```bash
# Feature with scope
git commit -S --signoff -m "feat(analysis): add support for TypeScript generics"

# Bug fix
git commit -S --signoff -m "fix: handle empty source files without panicking"

# Breaking change
git commit -S --signoff -m "feat!: redesign tool parameter schema

BREAKING CHANGE: The path parameter is now required"

# Documentation
git commit -S --signoff -m "docs: update tool descriptions in README"
```

## Developer Certificate of Origin (DCO)

All commits must be signed off to certify you have the right to submit the code:

```bash
git commit -S --signoff -m "Your commit message"
```

This adds `Signed-off-by: Your Name <email>` to your commit, certifying you agree to the [DCO](https://developercertificate.org/).

The `-S` flag GPG-signs the commit (required by branch protection).

## Pull Request Checklist

- [ ] Tests pass (`cargo test`)
- [ ] No clippy warnings (`cargo clippy -- -D warnings`)
- [ ] Code formatted (`cargo fmt --check`)
- [ ] Dependency audit clean (`cargo deny check advisories licenses`); dependencies must also pass the 7-day freshness gate (run automatically in CI; bypass with `SKIP_PACKAGE_AGE_CHECK=true` if necessary)
- [ ] Commits GPG-signed and signed off (`git commit -S --signoff`)
- [ ] Clear PR description

## Code review

All changes go through a pull request; no direct pushes to main are permitted.

**Before requesting review:**
- Self-review the diff for correctness, test coverage, and adherence to coding standards
- Ensure CI passes: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`
- Confirm no `.unwrap()` in production code paths
- Confirm DCO sign-off is present on all commits (`git commit --signoff`)
- New `.rs` files must include the two-line SPDX header at line 1:
  ```
  // SPDX-FileCopyrightText: 2026 aptu-coder contributors
  // SPDX-License-Identifier: Apache-2.0
  ```

**Review process:**
- Address all review comments before merging; unresolved comments block merge

**Acceptance criteria:**
- All CI jobs pass
- No unresolved review comments
- Reviewer approves or all raised issues are addressed

## Code Quality Rules

This project follows a Rust adaptation of the [NASA/JPL Power of 10](https://en.wikipedia.org/wiki/The_Power_of_10:_Rules_for_Developing_Safety-Critical_Code) to ensure code reliability and maintainability. The following rules are mechanically enforced via CI:

- **Zero warnings:** `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test`, and `cargo deny check` must all pass
- **Every unsafe block requires a SAFETY comment:** Enforced by `clippy::undocumented_unsafe_blocks = deny` in workspace lints
- Workspace lint denies (`unwrap_used`, `expect_used`), test-code exemptions, and the shared MCP test harness are documented in [AGENTS.md](AGENTS.md) — see that file rather than this one
- **No unchecked indexing on untrusted data:** Use `.get()` with explicit error propagation instead of `[]`
- **Loops over untrusted or externally-driven data must have an explicit bound or checked max-iteration guard**
- **Minimize #[cfg] feature combinations:** Each supported combination must be covered by CI

## Automated Review Tooling

This repository uses [aptu](https://github.com/clouatre-labs/aptu) for automated issue triage and pull request review. Both are dispatched externally by the `aptu-github-app`, not triggered directly by GitHub issue/PR events.

### Issue triage

`.github/workflows/aptu-triage.yml` runs on `repository_dispatch` (type `aptu-triage`) and applies type and priority labels. No comment is posted to the issue.

### Pull request review

`.github/workflows/aptu-review.yml` runs on `repository_dispatch` (type `aptu-review`). The review is advisory (`continue-on-error: true`) and never blocks merging. The AI provider and model are set by the dispatch payload, defaulting to OpenRouter; see the workflow file for current defaults.

### Copilot code review

GitHub Copilot code review is intentionally disabled for this repository. The `copilot_code_review` rule has been removed from the `Protect main branch` ruleset (id 13342306). To keep it disabled, ensure the rule is absent from that ruleset; do not re-add it.

## Branch Protection

See [docs/REPO-STANDARDS.md](docs/REPO-STANDARDS.md) for ruleset configuration, required status checks, signed-commit enforcement, branch protection rationale, and the repo merge strategy.

## License

By contributing, you agree your contributions are licensed under [Apache-2.0](LICENSE).

## Releasing

Releases are automated via GitHub Actions. Maintainers with push access to `main`:

### GPG Setup

Commits and tags must be GPG-signed. Follow the [GitHub docs on signing commits](https://docs.github.com/en/authentication/managing-commit-signature-verification/signing-commits) to generate a key, configure Git, and add the public key to your account.

### Release Steps

1. Update version in `Cargo.toml` (and `Cargo.lock` via `cargo update -w --offline`)
2. Open a PR (`chore(release): bump workspace version to X.Y.Z`), wait for checks, squash-merge
3. On the merged `main`: `git tag -s vX.Y.Z -m "vX.Y.Z" && git push origin vX.Y.Z`
4. Watch the run: `gh run watch $(gh run list --workflow=release.yml --limit 1 --json databaseId -q '.[0].databaseId')`
5. Edit the release to add curated notes (see below)

The workflow verifies the tag signature, then follows a **draft-first flow** (required because this repository has owner-enforced immutable releases — assets and the tag are locked at publish time):

1. **create-release** creates a *draft* release and outputs its release ID
2. **build-and-attest** builds per-target, archives (`aptu-coder-X.Y.Z-<target>.tar.gz`), generates a SHA256 checksum, signs with a cosign bundle, attests build provenance, and uploads all three assets to the draft
3. **generate-mcpb** builds `.mcpb` bundles from the draft tarballs and uploads them to the draft
4. **publish-release** flips `draft=false` — this makes the release immutable: assets and the tag are locked from this point on
5. **update-homebrew** and **publish-registry** download by tag and update the Homebrew tap / MCP Registry
6. **publish** publishes the workspace to crates.io

Because the tag is permanently reserved once an immutable release publishes on it, **never delete and re-create a release tag** — if a release is broken, cut `X.Y.Z+1` instead. A tag name used by an immutable release cannot be reused even after the release is deleted. Use `release-repair.yml` to rebuild assets for an existing tag (it no-ops asset mutation if the release is already published and immutable; Homebrew/server.json updates still run).

### Release Notes

The workflow generates initial notes on the draft. After the release publishes, edit it on GitHub (title and body remain editable on immutable releases) with a curated section following the house style — see [v0.35.3](https://github.com/clouatre-labs/aptu-coder/releases/tag/v0.35.3) for the reference format:

- Opening paragraph summarizing the release theme
- `### Features` / `### Fixes` / `### Documentation` / `### Maintenance` sections, each entry formatted as **`type(scope): title`** (#PR): one-line description
- `## Full Changelog` with a `compare/v<prev>...v<current>` link

### Dry Run

Test the release workflow without publishing or creating a release:

```bash
gh workflow run release.yml -f dry_run=true -f version=X.Y.Z
```

Note: `act` can also run Linux jobs locally, but `aarch64-apple-darwin` builds always require real GitHub runners.

### Versioning

We follow [SemVer](https://semver.org/): MAJOR (breaking), MINOR (features), PATCH (fixes).

## AI Agent Contributions

This section covers workflows for using **GitHub Copilot coding agent** to implement issues.

### Authoring Issues for Agents

A good agent-assignable issue includes:

- **Self-contained scope** with explicit deliverables and acceptance criteria checkboxes
- **Tool interfaces** with exact signatures (types, annotations, return types)
- **Key crates** with verified API surface (not based on training data)
- **Design notes** for non-obvious decisions
- **"Not In Scope"** section to prevent scope creep
- **Dependency chain** (`Depends on: #N`) for ordering

### Assigning Copilot

**REST API:**
```bash
gh api repos/{owner}/{repo}/issues/{number} --method PATCH -f "assignees[]=copilot-swe-agent[bot]"
```

**UI:** Issue sidebar → Assignees → select `copilot-swe-agent[bot]`.

- Assign issues only after their dependencies have merged (wave-based)
- One Copilot assignment per issue — it opens a PR autonomously

### PR Review Checklist

- [ ] Acceptance criteria checkboxes from the issue are satisfied
- [ ] `cargo fmt --check && cargo clippy -- -D warnings && cargo test` all pass
- [ ] No scope creep beyond the issue deliverables
- [ ] No hallucinated APIs (methods verified against installed crate versions)
- [ ] Conventional commit message with DCO sign-off (`Signed-off-by:`)

### Iteration Pattern

1. Comment `@copilot` on the PR with specific, actionable feedback
2. Agent pushes follow-up commits addressing the feedback
3. Re-review after each iteration
4. If the agent cannot resolve after two iterations: close the PR, amend the issue with clarifications, and re-assign
