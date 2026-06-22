<p align="center">

<h1 align="center">aptu-coder</h1>

<p align="center">
  <a href="https://crates.io/crates/aptu-coder"><img alt="crates.io" src="https://img.shields.io/crates/v/aptu-coder.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20"></a>
  <a href="https://slsa.dev"><img alt="SLSA Level 3" src="https://img.shields.io/badge/SLSA-Level%203-green?style=for-the-badge" height="20"></a>
  <a href="https://www.bestpractices.dev/projects/12275"><img alt="OpenSSF Best Practices" src="https://img.shields.io/cii/level/12275?style=for-the-badge" height="20"></a>
</p>

<p align="center">Standalone MCP server for code structure analysis using tree-sitter. OpenSSF silver certified: fewer than 1% of open source projects reach this level.</p>

<!-- mcp-name: io.github.clouatre-labs/aptu-coder -->

> [!NOTE]
> Native agent tools (regex search, path matching, file reading) handle targeted lookups well. `aptu-coder` handles the mechanical, non-AI work: mapping directory structure, extracting symbols, and tracing call graphs. Offloading this to a dedicated tool reduces token usage and speeds up coding with better accuracy.

## Benchmarks

Auth migration task on Claude Code against [Django](https://github.com/django/django) (Python) source tree. [Full methodology](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/benchmarks/v12/methodology.md).

| Mode | Sonnet 4.6 | Haiku 4.5 |
|---|---|---|
| MCP | 112k tokens, $0.39 | 406k tokens, $0.42 |
| Native | 276k tokens, $0.95 | 473k tokens, $0.53 |
| **Savings** | **59% fewer tokens, 59% cheaper** | **14% fewer tokens, 21% cheaper** |

AeroDyn integration audit task on Claude Code against [OpenFAST](https://github.com/OpenFAST/openfast) (Fortran) source tree. [Full methodology](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/benchmarks/v13/methodology.md).

| Mode | Sonnet 4.6 | Haiku 4.5 |
|---|---|---|
| MCP | 472k tokens, $1.65 | 687k tokens, $0.72 |
| Native | 877k tokens, $2.85 | 2162k tokens, $2.21 |
| **Savings** | **46% fewer tokens, 42% cheaper** | **68% fewer tokens, 68% cheaper** |

## Overview

aptu-coder is a Model Context Protocol server that gives AI agents precise structural context about a codebase: directory trees, symbol definitions, and call graphs, without reading raw files. It supports Rust, Python, Go, Java, Kotlin, TypeScript, TSX, Fortran, JavaScript, C/C++, C#, Markdown, HTML, CSS, YAML, Astro, JSON, and TOML, and integrates with any MCP-compatible orchestrator.

## Supported Languages

All languages are enabled by default. Disable individual languages at compile time via Cargo feature flags.

| Language | Extensions | Feature flag |
|----------|------------|--------------|
| Rust | `.rs` | `lang-rust` |
| Python | `.py` | `lang-python` |
| TypeScript | `.ts` | `lang-typescript` |
| TSX | `.tsx` | `lang-tsx` |
| Go | `.go` | `lang-go` |
| Java | `.java` | `lang-java` |
| Kotlin | `.kt`, `.kts` | `lang-kotlin` |
| Fortran | `.f`, `.f77`, `.f90`, `.f95`, `.f03`, `.f08`, `.for`, `.ftn` | `lang-fortran` |
| JavaScript | `.js`, `.mjs`, `.cjs` | `lang-javascript` |
| C/C++ | `.c`, `.cc`, `.cpp`, `.cxx`, `.h`, `.hpp`, `.hxx` | `lang-cpp` |
| C# | `.cs` | `lang-csharp` |
| Markdown | `.md`, `.mdx` | `lang-markdown` |
| HTML | `.html`, `.htm` | `lang-html` (stub; no extraction) |
| CSS | `.css` | `lang-css` (tree-sitter; regex fallback when disabled) |
| YAML | `.yaml`, `.yml` | `lang-yaml` (tree-sitter; regex fallback when disabled) |
| Astro | `.astro` | always-on (regex via TypeScript frontmatter extractor) |
| JSON | `.json` | always-on (regex; first-level key extraction) |
| TOML | `.toml` | always-on (regex; section header extraction) |

## Installation

### Homebrew (macOS and Linux)

```bash
brew install clouatre-labs/tap/aptu-coder
```

Update: `brew upgrade aptu-coder`

### cargo-binstall (no Rust required)

```bash
cargo binstall aptu-coder
```

### cargo install (requires Rust toolchain)

```bash
cargo install aptu-coder
```

## Quick Start

### Build from source

```bash
cargo build --release
```

The binary is at `target/release/aptu-coder`.

### Configure MCP Client

Two transports are available. **Streamable HTTP is recommended** when using orchestrators that spawn delegates (e.g. goose coder): a single server process is shared across the orchestrator and all agents, eliminating extension-drift that occurs when each stdio subprocess gets its own isolated instance.

**Streamable HTTP (recommended for multi-agent setups)**

With Homebrew, one command starts the server on login and keeps it running:

```bash
brew services start aptu-coder
```

The Homebrew formula starts the server on port `49200` by default. Then add the extension once to `~/.config/goose/config.yaml`:

```yaml
extensions:
  aptu-coder:
    type: streamable_http
    uri: http://127.0.0.1:49200/mcp
    name: aptu-coder
    timeout: 300
```

Or for Claude Code:

```bash
claude mcp add --transport http aptu-coder http://127.0.0.1:49200/mcp
```

To use a different port, set `APTU_CODER_PORT` before restarting:

```bash
APTU_CODER_PORT=4000 brew services restart aptu-coder
```

To start directly without brew services:

```bash
aptu-coder --port 49200
# or equivalently
APTU_CODER_PORT=49200 aptu-coder
```

**stdio (single-client use)**

Suitable when only one process needs the server. The client owns the process lifecycle and spawns it automatically:

```bash
claude mcp add --transport stdio aptu-coder -- aptu-coder
```

Or add manually to `.mcp.json` at your project root (shared with your team via version control):

```json
{
  "mcpServers": {
    "aptu-coder": {
      "command": "aptu-coder",
      "args": []
    }
  }
}
```

## Tools

All optional parameters may be omitted. Shared optional parameters for `analyze_directory`, `analyze_file`, and `analyze_symbol`:

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `summary` | boolean | auto | Compact output; auto-triggers above 50K chars |
| `cursor` | string | -- | Pagination cursor from a previous response's `next_cursor` |
| `page_size` | integer | 100 | Items per page |

| Tool | Purpose | Languages |
|------|---------|-----------|
| `analyze_directory` | Directory tree with LOC, function, and class counts; respects `.gitignore` | all |
| `analyze_file` | Functions, classes, and imports with signatures and line ranges; returns graceful fallback (line count, file head, no AST) for unsupported extensions | all |
| `analyze_module` | Lightweight function and import index (~75% smaller than `analyze_file`); returns graceful fallback (empty index with note) for unsupported extensions | all |
| `analyze_symbol` | Call graph for a named symbol across a directory; callers, callees, call depth | all |
| `edit_overwrite` | Create or overwrite a file; creates parent directories | any file |
| `edit_replace` | Replace a unique exact text block; errors if zero or multiple matches; pass empty `new_text` to delete the matched block; CRLF line endings in `old_text` are normalized to LF before matching | all |
| `exec_command` | Run a shell command; returns stdout, stderr, and exit code; supports progress notifications; output capped and filtered automatically; optional `timeout_secs` kills the child on expiry (`timed_out: true` in response); heredoc syntax with a missing closing delimiter is rejected before spawn | any |

Tool parameters, constraints, and examples are available via your MCP client's tool inspector or `tools/list` response.

## Output Management

For large codebases, several mechanisms prevent context overflow.

**Pagination**

`analyze_file` and `analyze_symbol` append a `NEXT_CURSOR:` line when output is truncated. Pass the token back as `cursor` to fetch the next page. `summary=true` and `cursor` are mutually exclusive; passing both returns an error.

```
# Response ends with:
NEXT_CURSOR: eyJvZmZzZXQiOjUwfQ==

# Fetch next page:
analyze_symbol path: /my/project symbol: my_function cursor: eyJvZmZzZXQiOjUwfQ==
```

**exec_command output caps**

`exec_command` applies three independent byte-level caps to prevent large command outputs from flooding the context:

| Stream | Cap | Behavior |
|--------|-----|----------|
| stdout | 30,000 chars | Tail-preserving; keeps the last 30k chars |
| stderr | 10,000 chars | Tail-preserving; errors appear at the end |
| combined `output_text` | 50,000 chars | Safety net after interleaving |

The 30k stdout cap is data-driven: analysis of 27,981 observed `exec_command` calls shows only 0.33% exceed this limit. When any cap fires, `output_truncated: true` is set in the response and recorded in the JSONL metrics.

**exec_command output filters**

A built-in filter table suppresses per-file noise from chatty CLI tools before output reaches the model. Filters apply to stdout, stderr, and interleaved output on success only; raw output is always preserved on failure.

| Command | Behavior |
|---------|----------|
| `git pull` | Strips diff-stat noise (pipe bars, `create mode`, `delete mode`, `rename`, `mode change` lines); empty output replaced with `ok (up-to-date)` |
| `git fetch` | Strips `From` and ref-range lines; caps at 10 lines; empty output replaced with `ok fetched` |
| `git push` | Strips `remote:` progress lines and `To ` destination lines; caps at 10 lines; empty output replaced with `ok pushed` |
| `git log` | Caps at 20 lines |
| `git status` | Caps at 20 lines |
| `git show` | Strips patch hunks (`@@` headers and `+`/`-` diff lines); caps at 200 lines |
| `git commit` | Strips GPG signing and gitleaks hook output; caps at 10 lines; empty output replaced with `ok committed` |
| `git diff` | Strips ANSI escape sequences; caps at 100 lines; empty output replaced with `ok (working tree clean)` |
| `git add` | Strips gitleaks hook output; caps at 5 lines; empty output replaced with `ok staged` |
| `cargo build` | Strips `Compiling` / `Checking` / `Downloading` / `Fresh` lines; empty output replaced with `ok (build clean)` |
| `cargo test` | Strips `Compiling` / `Checking` / `Fresh` lines |

Project-local rules can be added in `.aptu/filters.toml`. Parse errors and unrecognized schema_version values (version != 1) fall back to the built-in table with a logged warning; no crash occurs. When a filter fires, `filter_applied` in `structuredContent` identifies which rule matched.

## Non-Interactive Pipelines

In single-pass subagent sessions, prompt caches are written but never reused. Benchmarks showed MCP responses writing ~2x more to cache than native-only workflows, adding cost with no quality gain. Set `DISABLE_PROMPT_CACHING=1` (or `DISABLE_PROMPT_CACHING_HAIKU=1` for Haiku-specific pipelines) to avoid this overhead.

The server's own instructions expose a 4-step recommended workflow for unknown repositories: survey the repo root with `analyze_directory` at `max_depth=2`, drill into the source package, run `analyze_module` on key files for a function/import index (or `analyze_file` when signatures and types are needed), then use `analyze_symbol` to trace call graphs. MCP clients that surface server instructions will present this workflow automatically to the agent.

## Environment Variables

### Cache and runtime

| Variable | Default | Description |
|---|---|---|
| `APTU_CODER_DIR_CACHE_CAPACITY` | `20` | LRU cache size for directory-analysis results. |
| `APTU_CODER_DISK_CACHE_DIR` | `$XDG_DATA_HOME/aptu-coder/analysis-cache` | Directory for the L2 on-disk call-graph cache used by `analyze_symbol`. |
| `APTU_CODER_DISK_CACHE_DISABLED` | unset | Set to `1` to disable the L2 disk cache entirely. |
| `APTU_CODER_EXEC_CACHE_CAPACITY` | `64` | LRU cache size for `exec_command` results. |
| `APTU_CODER_EXEC_CACHE_TTL_SECS` | `10` | TTL in seconds for `exec_command` result cache. |
| `APTU_CODER_FILE_CACHE_CAPACITY` | `100` | LRU cache size for file-analysis results. |
| `APTU_CODER_METRICS_EXPORT_FILE` | unset | Absolute path for a one-shot JSONL metrics export on shutdown. |
| `APTU_CODER_PORT` | unset | Port for streamable HTTP mode. Equivalent to `--port N`; `--port` takes precedence. When unset and `--port` is not passed, stdio mode is used. |
| `APTU_CODER_PROFILE` | unset | Tool subset: `edit` (edit tools + `exec_command` only), `analyze` (analyze tools + `exec_command` only). Also settable per-session via `io.clouatre-labs/profile` in MCP `_meta`. |
| `APTU_SHELL` | unset | Shell for `exec_command`. Defaults to `bash` then `/bin/sh`. |

### Telemetry

| Variable | Default | Description |
|---|---|---|
| `DISABLE_PROMPT_CACHING` | unset | Set to `1` to disable prompt caching (recommended for single-pass subagent sessions). |
| `DISABLE_PROMPT_CACHING_HAIKU` | unset | Set to `1` to disable prompt caching for Haiku-specific pipelines only. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset | OTLP HTTP endpoint URL (e.g., `http://localhost:4318`). When set, enables trace, log, and metric export via OTLP/HTTP; noop providers when unset. |
| `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` | unset | Reserved per OTel GenAI conventions; aptu-coder does not implement this -- bounded parameters are recorded as span attributes instead. |
| `XDG_DATA_HOME` | `~/.local/share` | Base directory for daily-rotated JSONL metrics files (`$XDG_DATA_HOME/aptu-coder/metrics-YYYY-MM-DD.jsonl`, 30-day retention). |

## Observability

The server emits two parallel, independent telemetry streams.

**JSONL metrics (always-on)** are written daily-rotated to `$XDG_DATA_HOME/aptu-coder/` (fallback: `~/.local/share/aptu-coder/`) regardless of configuration. Each record captures tool name, duration, output size, and result status. Files are retained for 30 days. See [docs/OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/OBSERVABILITY.md) for the full schema.

**OpenTelemetry export (opt-in)** is enabled when `OTEL_EXPORTER_OTLP_ENDPOINT` is set to an OTLP HTTP endpoint URL. When set, the server initializes OpenTelemetry trace, log, and meter providers and exports asynchronously via OTLP/HTTP. When unset, noop providers are used with zero runtime overhead.

Each tool invocation is wrapped in a span carrying OpenTelemetry GenAI semantic attributes (`gen_ai.system`, `gen_ai.operation.name`, `gen_ai.tool.name`). W3C Trace Context is extracted from the MCP `_meta` field on each call, allowing MCP clients to propagate their trace context so tool spans appear as children in a distributed trace.

For the span attribute policy, the never-record list, and details on what is instrumented, see [OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/OBSERVABILITY.md) at the repository root.

## Documentation

- **[AGENTS.md](https://github.com/clouatre-labs/aptu-coder/blob/main/AGENTS.md)** - Contributor reference: project structure, commands, rmcp footguns, tool parameter constraints
- **[ARCHITECTURE.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/ARCHITECTURE.md)** - Design goals, module map, data flow, language handler system, caching strategy
- **[CONTRIBUTING.md](https://github.com/clouatre-labs/aptu-coder/blob/main/CONTRIBUTING.md)** - Development workflow, commit conventions, PR checklist
- **[DESIGN-GUIDE.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/DESIGN-GUIDE.md)** - Design decisions, rationale, and replication guide for building high-performance MCP servers
- **[MCP Best Practices](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/MCP-BEST-PRACTICES.md)** - Best practices for agentic loops, orchestration patterns, MCP tool design, memory management, and safety controls
- **[OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/OBSERVABILITY.md)** - Metrics schema, JSONL format, and retention policy
- **[ROADMAP.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/ROADMAP.md)** - Development history and future direction
- **[SECURITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/SECURITY.md)** - Security policy and vulnerability reporting

## License

Apache-2.0. See [LICENSE](https://github.com/clouatre-labs/aptu-coder/blob/main/LICENSE) for details.
