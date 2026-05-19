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
- **No `.unwrap()` in production code:** Enforced by `clippy::unwrap_used = "deny"`. Test code is exempted via `#[cfg_attr(test, allow(clippy::unwrap_used))]` at each crate root.
- **No unchecked indexing on untrusted data:** Use `.get()` with explicit error propagation instead of `[]`
- **Loops over untrusted or externally-driven data must have an explicit bound or checked max-iteration guard**
- **Minimize #[cfg] feature combinations:** Each supported combination must be covered by CI

## Automated Review Tooling

This repository uses [aptu](https://github.com/clouatre-labs/aptu) for automated issue triage and pull request review via GitHub Actions.

### Issue triage

Every new issue triggers `.github/workflows/issue-triage.yml`, which runs `aptu issue triage`. The workflow applies type and priority labels. No comment is posted to the issue.

### Pull request review

Every opened or updated pull request triggers `.github/workflows/pr-review.yml`, which runs `aptu pr review`. The review is advisory (`continue-on-error: true`) and never blocks merging. The workflow uses Gemini as the primary provider and OpenRouter Mercury-2 as the fallback.

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

1. Update version in `Cargo.toml`
2. Commit: `git commit -S --signoff -m "chore: bump version to X.Y.Z"`
3. Tag: `git tag -s vX.Y.Z -m "vX.Y.Z"`
4. Push: `git push origin main --tags`
5. Edit the release to add highlights (see below)

The workflow verifies the tag signature, builds binaries (macOS ARM64, Linux ARM64/x86_64 musl), generates GitHub artifact attestations, creates a GitHub release with auto-generated notes, publishes to crates.io, and opens a PR against the Homebrew tap.

### Release Notes

GitHub auto-generates a changelog from conventional commits. After the workflow completes, edit the release on GitHub to prepend a curated highlights section:

```markdown
## [Theme or Summary]

Brief description of what this release delivers.

### Highlights

- **Feature Name** - One-line description
- **Another Feature** - One-line description

---

[Auto-generated changelog follows]
```

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
