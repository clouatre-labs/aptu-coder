# AGENTS.md

## Project structure

Rust workspace with two crates:

- `crates/aptu-coder-core` -- parsing, analysis, formatting, graph, pagination, types
- `crates/aptu-coder` -- MCP server, tool handlers, logging, metrics

Seven MCP tools: `analyze_directory`, `analyze_file`, `analyze_module`, `analyze_symbol` (analyze_* family); `edit_overwrite`, `edit_replace` (edit_* family); `exec_command` (exec_* family).
Rust edition 2024, async with tokio, MCP protocol 2025-11-25 via `rmcp`. Supported languages are listed in `crates/aptu-coder-core/src/lang.rs`.

## CI runners

All CI jobs run on `ubuntu-24.04-arm` (ARM64). Build, test, lint, and release jobs all target this image.

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

Workspace lints enforced in CI (deny): undocumented_unsafe_blocks, unwrap_used (test code exempted via cfg_attr).
Dependency freshness: new Cargo.lock entries must be >=7 days old; bypass with SKIP_PACKAGE_AGE_CHECK=true.

## Observability

Two parallel telemetry channels; neither blocks tool execution.

- **JSONL (always-on):** daily-rotated files at `$XDG_DATA_HOME/aptu-coder/metrics-YYYY-MM-DD.jsonl`; 30-day retention. See [docs/OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/OBSERVABILITY.md) for schema and span attribute policy.
- **OpenTelemetry (opt-in):** set `OTEL_EXPORTER_OTLP_ENDPOINT` to enable OTLP/HTTP export; noop providers when unset. W3C Trace Context extracted from MCP `_meta` so tool spans appear as children in the calling agent's distributed trace.

### JSONL Metrics Analysis

Files: `$XDG_DATA_HOME/aptu-coder/metrics-YYYY-MM-DD.jsonl` (default: `~/.local/share/aptu-coder/`). Full schema in [docs/OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/OBSERVABILITY.md).

Five validated jq one-liners (run from `~/.local/share/aptu-coder/`). Always `cd` there first -- globs expand against CWD and silently match nothing from a different directory.

1. Tool call volume: `jq -r '.tool' metrics-*.jsonl | sort | uniq -c | sort -rn`
2. Avg duration by tool: `jq -r '[.tool, .duration_ms] | @tsv' metrics-*.jsonl | awk -F'\t' '{c[$1]++;s[$1]+=$2} END{for(t in c) printf "%s\t%dms avg\n",t,s[t]/c[t]}' | sort -t$'\t' -k2 -rn`
3. Error rate by tool: `jq -r 'select(.result=="error") | .tool' metrics-*.jsonl | sort | uniq -c | sort -rn`
4. Cache hit rate by day: `for f in metrics-*.jsonl; do d=${f#metrics-}; d=${d%.jsonl}; total=$(jq 'select(.cache_hit!=null)' $f | wc -l); hits=$(jq 'select(.cache_hit==true)' $f | wc -l); echo "$d: $hits/$total"; done`
5. Slowest 10 calls: `jq -r '[.tool, (.duration_ms|tostring), (.session_id//"?")] | @tsv' metrics-*.jsonl | sort -t$'\t' -k2 -rn | head -10`

For richer analysis, use `python scripts/mcp-metrics.py --help`.

## API verification (critical)

Do not rely on training data for `rmcp`, `schemars`, or `thiserror` APIs. **Post-M17, `crates/aptu-coder/src/lib.rs` is shim-only** (MCP wiring, `#[tool(...)]` decorators, thin forwarding calls). Tool handler logic lives in `crates/aptu-coder/src/tools/<tool>.rs` -- read the relevant handler file before adding or modifying any tool. Path validation logic lives in `src/validation.rs`; shell detection in `src/shell.rs`; exec output filtering (built-in rules, project-local `.aptu/filters.toml`, `schema_version` enforcement) in `src/filters.rs`. Read the relevant module before touching those subsystems.

## Integration tests

Integration tests for the `aptu-coder` crate live in `crates/aptu-coder/tests/`. A shared MCP harness in `tests/common/mod.rs` provides `make_test_analyzer()` and `call_tool_raw(tool_name, params)` -- use these in new integration tests rather than duplicating the server setup. `call_tool_raw` spins up a real in-process MCP server over a duplex pipe, runs the full initialize/call/shutdown handshake, and returns the raw JSON response.

## rmcp footguns

Patterns contributors consistently get wrong:

- Use `Content`, not `RawContent` (does not exist)
- Every `#[tool(...)]` requires `output_schema = schema_for_type::<T>()` and `title = "..."`
- Tool methods take `_context: RequestContext<RoleServer>` as second parameter
- `#[tool_router]` goes on `impl CodeAnalyzer`; `#[tool_handler]` goes on `impl ServerHandler for CodeAnalyzer` -- they are separate impls
- Apply `.with_meta(Some(no_cache_meta()))` on every `CallToolResult::success(...)` response
- Transport entry point: `let (stdin, stdout) = stdio(); let service = serve_server(analyzer, (stdin, stdout)).await?; service.waiting().await?`

## Adding a language

Follow an existing handler in `crates/aptu-coder-core/src/languages/`. The extension map is in `crates/aptu-coder-core/src/lang.rs`; the `LanguageInfo` registry with queries is in `crates/aptu-coder-core/src/languages/mod.rs`.

## Tool parameters

Canonical parameter lists live in the `types` module (`crates/aptu-coder-core/src/types.rs`). Key non-obvious constraints:

- `summary=true` and `cursor` are mutually exclusive; passing both returns INVALID_PARAMS.
- `impl_only=true` restricts `analyze_symbol` callers to `impl Trait for Type` blocks; returns INVALID_PARAMS for non-Rust directories.
- `analyze_module` supports `path` only -- pagination and summary are not supported.
- `import_lookup=true` on `analyze_symbol` requires a non-empty `symbol` (the module path to search for); returns INVALID_PARAMS if symbol is empty. Mutually exclusive with normal call-graph lookup.
- `def_use=true` on `analyze_symbol` triggers def-use extraction; `def_use_sites` is populated in `structuredContent` only when paginating in DefUse cursor mode, not on the initial call (the handler clears it on the first response and bootstraps a cursor to page through def-use results).
- `git_ref` is supported on both `analyze_directory` and `analyze_symbol` to restrict analysis to files changed relative to a git ref.
- `working_dir` on `edit_overwrite` and `edit_replace` sets the base directory for path resolution (default: server CWD). The resolved target path must be within working_dir.
- `edit_replace` accepts empty `new_text` to delete the matched block. CRLF line endings in `old_text` are normalized to LF before matching. A stale-context circuit breaker fires after 5 consecutive `not_found` or `ambiguous` failures on the same (session_id, path) pair, returning a directive error; the map is capped at 1024 entries.
- `exec_command` accepts an optional `timeout_secs` (integer >= 0; 0 or omitted means no limit). When the child process exceeds the limit it is killed; `timed_out: true` is set in the response and `exit_code` is null. Heredoc syntax with a missing closing delimiter is rejected before spawn.
- `exec_command` accepts an optional `drain_timeout_secs` (integer >= 0; 0 or omitted means 500ms default, negative = INVALID_PARAMS, positive = drain window in milliseconds). Controls how long the post-exit drain waits for a background subprocess holding pipes open before returning `output_truncated: true`.
- `analyze_symbol` uses an L2 on-disk call-graph cache in addition to the L1 in-memory LRU. Configure with `APTU_CODER_DISK_CACHE_DIR` (default: `$XDG_DATA_HOME/aptu-coder/analysis-cache`); disable with `APTU_CODER_DISK_CACHE_DISABLED=1`.
- `call_frequency` on `analyze_symbol` is filtered out when the `Functions` field is not in the projected fields set.
- `APTU_CODER_PROFILE` (env var) or `io.clouatre-labs/profile` (MCP `_meta`) activates a tool subset: `edit` disables all analyze_* tools (4 tools: analyze_directory, analyze_file, analyze_module, analyze_symbol); `analyze` disables edit_* tools (5 tools: edit_overwrite, edit_replace, exec_command, and aliases); absent/unknown enables all 7 tools. By default, all 7 tools are available.

Escalate to `analyze_symbol` when: (1) you need all callers of a function, (2) you need the full call chain for a symbol, (3) you need all files importing a module path (use `import_lookup=true`).

## Do not

- Add dependencies without justification in the PR description
- Use `unsafe` code without a `// SAFETY:` comment (every unsafe block requires documentation, enforced by `clippy::undocumented_unsafe_blocks = "deny"`)
- Implement features not specified in the assigned issue
- Modify files outside the scope of the assigned issue
- Assume any API exists based on training data; verify against installed crate versions
- Reference host-specific tools or clients in tool descriptions or server instructions (e.g. Claude Code's Grep, Glob, Read)
- Use `gh release create` to tag releases; always create a GPG-signed annotated tag and push it to trigger the release workflow
- Never revert `release.yml` `update-homebrew` to full formula regeneration; it must update URLs and SHA256s in-place so that structural changes in `clouatre-labs/homebrew-tap/Formula/aptu-coder.rb` survive releases
- Remove `DISABLE_PROMPT_CACHING=1` from server instructions; caching data never read again is detrimental
- Use relative links in `README.md`; all links must be absolute (`https://github.com/clouatre-labs/aptu-coder/blob/main/...`) so they resolve correctly when README is rendered on crates.io, docs.rs, and other mirrors
