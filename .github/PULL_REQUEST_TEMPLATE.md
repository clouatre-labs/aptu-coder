## Summary
<!-- 2-3 sentences: what changed and why. Be specific about the problem solved or feature added. -->

Example: "Add Python language support to the analyzer. Implements Overview and FileDetails modes using tree-sitter-python grammar, enabling analysis of Python projects alongside Rust."

## Related Issues
<!-- Link to issues this PR closes or depends on. Use "Closes #N" or "Depends on #N" format. -->

- Closes #N (if applicable)
- Depends on #N (if applicable)

## Changes
<!-- Bullet list of what was modified. Be specific about files and scope. -->

- Added `src/language/python.rs` with Python-specific query handlers
- Updated `src/language/mod.rs` to register Python language handler
- Added `Cargo.toml` dependency: tree-sitter-python
- Added integration tests in `tests/integration/python.rs`

## Test Plan
<!-- What was tested and how to verify. Include edge cases covered. -->

- Unit tests: `cargo test` (all pass)
- Integration tests: Python files with valid and invalid syntax
- Edge cases: empty files, syntax errors, deeply nested structures
- Manual verification: `cargo run -- --mode overview path/to/python/project`

## Verification Checklist
<!-- Key PR verification steps. See CONTRIBUTING.md for the complete PR requirements checklist. Last item is required by AI_POLICY.md. -->

- [ ] Tests pass: `cargo test`
- [ ] Clippy clean: `cargo clippy -- -D warnings`
- [ ] Formatted: `cargo fmt --check`
- [ ] No `unwrap()`, `expect()`, `println!`, or `eprintln!` in library code
- [ ] No hallucinated APIs (verified against Cargo.lock and crate docs)
- [ ] Commit GPG signed: `git commit -S`
- [ ] DCO signed-off: `git commit --signoff`
- [ ] No scope creep (changes match assigned issue)
- [ ] No secrets, API keys, or credentials in diff
- [ ] New span attributes comply with the never-record policy in [OBSERVABILITY.md](../OBSERVABILITY.md) (no command strings, file content, or free-form user input)
- [ ] I have reviewed every line in this PR and can explain it
