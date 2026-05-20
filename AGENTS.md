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

By default, the server operates with noop telemetry providers (zero overhead). Telemetry export is opt-in via environment variables:

- `OTEL_EXPORTER_OTLP_ENDPOINT` -- Set to an OTLP HTTP endpoint URL (e.g., `http://localhost:4318`) to enable export of traces, logs, and metrics via OTLP/HTTP. When unset, the OpenTelemetry SDK is initialized with noop providers incurring no cost. Exports are asynchronous and do not block tool execution.

- `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` -- Reserved per OpenTelemetry GenAI semantic conventions for opt-in capture of tool arguments and results. aptu-coder does not implement this: individual bounded parameters (path, symbol, depth) are recorded as span attributes instead of serializing the full tool call arguments or results, ensuring credentials and file content are never emitted.

- `XDG_DATA_HOME` -- Base directory for the always-on JSONL metrics channel. The server writes daily-rotated metrics to `$XDG_DATA_HOME/aptu-coder/` and retains them for 30 days. Defaults to `~/.local/share` if unset.

**Instrumentation:** Each tool invocation is wrapped in a span carrying OpenTelemetry GenAI semantic attributes (`gen_ai.system`, `gen_ai.operation.name`, `gen_ai.tool.name`). W3C Trace Context is extracted from the MCP `_meta` field, allowing MCP clients to propagate their trace context so tool spans become children in a distributed trace.

**Metrics:**
- JSONL channel: always-on, fire-and-forget, zero latency cost. See [docs/OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/OBSERVABILITY.md) for the metric record schema and 30-day retention policy.
- OpenTelemetry: optional, parallel to JSONL. Configured at server init; no runtime reconfiguration.

For span attribute policy and the never-record list, see [OBSERVABILITY.md](OBSERVABILITY.md) at the repository root.

### JSONL Metrics Analysis

Daily-rotated JSONL files live at `$XDG_DATA_HOME/aptu-coder/metrics-YYYY-MM-DD.jsonl` (default: `~/.local/share/aptu-coder/`). See [docs/OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/OBSERVABILITY.md) for the full field reference.

Key schema fields: `ts` (u64), `tool` (string), `duration_ms` (u64), `output_chars` (usize), `result` (string), `error_type` (nullable), `session_id` (nullable), `seq` (nullable), `cache_hit` (nullable), `cache_tier` (nullable), `exit_code` (nullable), `timed_out` (bool).

Five validated jq one-liners:

1. Tool call volume: `jq -r '.tool' metrics-*.jsonl | sort | uniq -c | sort -rn`
2. Avg duration by tool: `jq -r '[.tool, .duration_ms] | @tsv' metrics-*.jsonl | awk -F'\t' '{c[$1]++;s[$1]+=$2} END{for(t in c) printf "%s\t%dms avg\n",t,s[t]/c[t]}' | sort -t$'\t' -k2 -rn`
3. Error rate by tool: `jq -r 'select(.result=="error") | .tool' metrics-*.jsonl | sort | uniq -c | sort -rn`
4. Cache hit rate by day: `for f in metrics-*.jsonl; do d=${f#metrics-}; d=${d%.jsonl}; total=$(jq 'select(.cache_hit!=null)' $f | wc -l); hits=$(jq 'select(.cache_hit==true)' $f | wc -l); echo "$d: $hits/$total"; done`
5. Slowest 10 calls: `jq -r '[.tool, (.duration_ms|tostring), (.session_id//"?")] | @tsv' metrics-*.jsonl | sort -t$'\t' -k2 -rn | head -10`

## API verification (critical)

Do not rely on training data for `rmcp`, `schemars`, or `thiserror` APIs. **Read `crates/aptu-coder/src/lib.rs` before adding or modifying any tool** -- it is the authoritative reference for tool handler patterns.

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
- `analyze_module` supports `path` only -- pagination, summary, force, and verbose are not supported.
- `import_lookup=true` on `analyze_symbol` requires a non-empty `symbol` (the module path to search for); returns INVALID_PARAMS if symbol is empty. Mutually exclusive with normal call-graph lookup.
- `def_use=true` on `analyze_symbol` triggers def-use extraction; `def_use_sites` is populated in `structuredContent` only when paginating in DefUse cursor mode, not on the initial call (the handler clears it on the first response and bootstraps a cursor to page through def-use results).
- `git_ref` is supported on both `analyze_directory` and `analyze_symbol` to restrict analysis to files changed relative to a git ref.
- `working_dir` on `edit_overwrite` and `edit_replace` sets the base directory for path resolution; must be within the server CWD.
- `APTU_CODER_PROFILE` (env var) or `io.clouatre-labs/profile` (MCP `_meta`) activates a tool subset: `edit` disables all analyze_* tools (3 tools); `analyze` disables edit_* tools (5 tools); absent/unknown enables all 7 tools. By default, all 7 tools are available.

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
