# AGENTS.md

## Project structure

Rust workspace with two crates:

- `crates/aptu-coder-core` -- parsing, analysis, formatting, graph, pagination, types
- `crates/aptu-coder` -- MCP server, tool handlers, logging, metrics, MCP Resources surface (list_resources, list_resource_templates, read_resource in crates/aptu-coder/src/tools/resources.rs)

MCP tool surface and parameter schemas are documented by the tools themselves; do not duplicate them here. Tool handler logic lives in `crates/aptu-coder/src/tools/<tool>.rs`. Path validation lives in `src/validation.rs`; shell detection in `src/shell.rs`; exec output filtering (built-in rules, project-local `.aptu/filters.toml`, `schema_version` enforcement) in `src/filters.rs`. Read the relevant handler or module before touching those subsystems.
Rust edition 2024, async with tokio, latest MCP protocol via `rmcp`. Supported languages are listed in `crates/aptu-coder-core/src/lang.rs`.
All CI jobs run on `ubuntu-26.04-arm` (ARM64).

## Commands

```
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
cargo deny check advisories licenses
cargo bench
cargo install --path crates/aptu-coder --profile release   # local install; binary lands in ~/.cargo/bin/
```

Workspace lints enforced in CI (deny): undocumented_unsafe_blocks, unwrap_used, expect_used. Both unwrap_used and expect_used have test-code exemptions via cfg_attr in each crate's lib.rs.
Dependency freshness: new Cargo.lock entries must be >=7 days old; bypass with SKIP_PACKAGE_AGE_CHECK=true.

## API & compatibility policy

Do not rely on training data for `rmcp`, `schemars`, or `thiserror` APIs. `crates/aptu-coder/src/lib.rs` is shim-only (MCP wiring, `#[tool(...)]` decorators, thin forwarding calls); handler logic lives in `crates/aptu-coder/src/tools/<tool>.rs`.
Keep the parameter surface strict: the tool schema is the contract -- no legacy fields, no silently ignored inputs, no deprecation windows. Breaking changes ship directly (alpha software).
Do not reference host-specific tools, clients, or environment variables in tool descriptions or server instructions.

## Observability

Two parallel telemetry channels; neither blocks tool execution.

- **JSONL (always-on):** daily-rotated files at `$XDG_DATA_HOME/aptu-coder/metrics-YYYY-MM-DD.jsonl`; 30-day retention.
- **OpenTelemetry (opt-in):** set `OTEL_EXPORTER_OTLP_ENDPOINT` to enable OTLP/HTTP export; noop providers when unset. W3C Trace Context extracted from MCP `_meta` so tool spans appear as children in the calling agent's distributed trace.

Schema, jq analysis recipes, and `scripts/mcp-metrics.py` usage live in [docs/METRICS.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/METRICS.md).

## Integration tests

Integration tests for the `aptu-coder` crate live in `crates/aptu-coder/tests/`. A shared MCP harness in `tests/common/mod.rs` provides `make_test_analyzer()` and `call_tool_raw(tool_name, params)` -- use these in new integration tests rather than duplicating the server setup.

## rmcp footguns

Patterns contributors consistently get wrong:

- Use `ContentBlock`, not `Content` or `RawContent`
- Every `#[tool(...)]` requires `output_schema = schema_for_type::<T>()` and `title = "..."`
- Tool methods take `_context: RequestContext<RoleServer>` as second parameter
- `#[tool_router]` goes on `impl CodeAnalyzer`; `#[tool_handler]` goes on `impl ServerHandler for CodeAnalyzer` -- they are separate impls
- Apply `.with_meta(Some(no_cache_meta()))` on every `CallToolResult::success(...)` response

## Adding a language

Follow an existing handler in `crates/aptu-coder-core/src/languages/`. The extension map is in `crates/aptu-coder-core/src/lang.rs`; the `LanguageInfo` registry with queries is in `crates/aptu-coder-core/src/languages/mod.rs`.

## Releases & CI

- Cut releases via a version-bump PR merged to `main`, then a GPG-signed annotated tag (`git tag -s vX.Y.Z`) pushed to trigger the release workflow; never `gh release create`
- The release pipeline is draft-first (required by owner-enforced immutable releases): create draft → upload all assets to the draft → publish last. Assets and the tag lock permanently at publish; never delete and re-create a release tag — cut `X.Y.Z+1` instead. See CONTRIBUTING.md "Releasing" for the full validated methodology
- Never revert `release.yml` `update-homebrew` to full formula regeneration; it must update URLs and SHA256s in-place so that structural changes in `clouatre-labs/homebrew-tap/Formula/aptu-coder.rb` survive releases
- `README.md` links must be absolute (`https://github.com/clouatre-labs/aptu-coder/blob/main/...`), never relative, so they resolve on crates.io, docs.rs, and other mirrors

## Do not

- Add dependencies without justification in the PR description
- Use `unsafe` code without a `// SAFETY:` comment (enforced by `clippy::undocumented_unsafe_blocks = "deny"`)
- Implement features not specified in the assigned issue
- Modify files outside the scope of the assigned issue
- Assume any API exists based on training data; verify against installed crate versions
- Embed metrics analysis recipes in this file; [docs/METRICS.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/METRICS.md) is the single source
