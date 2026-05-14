# PR Review Instructions

## Scope

Review only what the PR changes. Do not flag issues in files the PR does not touch.

## Workflow files

When reviewing `.github/workflows/` changes:

- Evaluate the full job context, not individual steps in isolation. A step that installs a binary
  and a step that executes it are part of the same job; verify both exist before flagging a
  missing publish or execution command.
- Flag `${{ expression }}` interpolation directly inside `run:` scripts as an injection risk;
  inputs should be passed via `env:` blocks.
- Verify action pins use commit SHAs, not mutable tags.
- Check that `permissions:` blocks are present and minimal.

## Rust crates

- Do not suggest adding dependencies without a clear justification.
- Do not flag missing `unwrap()` suppression in test code; `.unwrap()` is acceptable in tests.
- Verify that new tool handlers in `crates/aptu-coder/src/lib.rs` follow the patterns documented
  in `AGENTS.md` (rmcp footguns section) before flagging style issues.

## General

- One comment per distinct issue; do not duplicate findings across multiple inline comments.
- Prefer suggesting a fix (suggestion block) over describing the problem when the fix is
  unambiguous.
- Do not comment on code style that `cargo fmt` or `cargo clippy` would catch automatically;
  those are enforced by CI.
