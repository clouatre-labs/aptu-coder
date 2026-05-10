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

aptu-coder is a Model Context Protocol server that gives AI agents precise structural context about a codebase: directory trees, symbol definitions, and call graphs, without reading raw files. It supports Rust, Python, Go, Java, Kotlin, TypeScript, TSX, Fortran, JavaScript, C/C++, and C#, and integrates with any MCP-compatible orchestrator.

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

After installation via brew or cargo, register with the Claude Code CLI:

```bash
claude mcp add --transport stdio aptu-coder -- aptu-coder
```

If you built from source, use the binary path directly:

```bash
claude mcp add --transport stdio aptu-coder -- /path/to/repo/target/release/aptu-coder
```

stdio is intentional: this server runs locally and processes files directly on disk. The low-latency, zero-network-overhead transport matches the use case. Streamable HTTP adds a network hop with no benefit for a local tool.

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
| `force` | boolean | false | Bypass output size warning |
| `verbose` | boolean | false | Full output with section headers and imports |

| Tool | Purpose | Languages |
|------|---------|-----------|
| `analyze_directory` | Directory tree with LOC, function, and class counts; respects `.gitignore` | all |
| `analyze_file` | Functions, classes, and imports with signatures and line ranges | all |
| `analyze_module` | Lightweight function and import index (~75% smaller than `analyze_file`) | all |
| `analyze_symbol` | Call graph for a named symbol across a directory; callers, callees, call depth | all |
| `edit_overwrite` | Create or overwrite a file; creates parent directories | any file |
| `edit_replace` | Replace a unique exact text block; errors if zero or multiple matches | all |
| `exec_command` | Run a shell command; returns stdout, stderr, exit code, and timeout status; supports progress notifications | any |

Tool parameters, constraints, and examples are available via your MCP client's tool inspector or `tools/list` response.

## Output Management

For large codebases, two mechanisms prevent context overflow:

**Pagination**

`analyze_file` and `analyze_symbol` append a `NEXT_CURSOR:` line when output is truncated. Pass the token back as `cursor` to fetch the next page. `summary=true` and `cursor` are mutually exclusive; passing both returns an error.

```
# Response ends with:
NEXT_CURSOR: eyJvZmZzZXQiOjUwfQ==

# Fetch next page:
analyze_symbol path: /my/project symbol: my_function cursor: eyJvZmZzZXQiOjUwfQ==
```

## Non-Interactive Pipelines

In single-pass subagent sessions, prompt caches are written but never reused. Benchmarks showed MCP responses writing ~2x more to cache than native-only workflows, adding cost with no quality gain. Set `DISABLE_PROMPT_CACHING=1` (or `DISABLE_PROMPT_CACHING_HAIKU=1` for Haiku-specific pipelines) to avoid this overhead.

The server's own instructions expose a 4-step recommended workflow for unknown repositories: survey the repo root with `analyze_directory` at `max_depth=2`, drill into the source package, run `analyze_module` on key files for a function/import index (or `analyze_file` when signatures and types are needed), then use `analyze_symbol` to trace call graphs. MCP clients that surface server instructions will present this workflow automatically to the agent.

## Environment Variables

### Cache and runtime

| Variable | Default | Description |
|---|---|---|
| `APTU_CODER_DIR_CACHE_CAPACITY` | `20` | LRU cache size for directory-analysis results. |
| `APTU_CODER_EXEC_CACHE_CAPACITY` | `64` | LRU cache size for `exec_command` results. |
| `APTU_CODER_EXEC_CACHE_TTL_SECS` | `10` | TTL in seconds for `exec_command` result cache. |
| `APTU_CODER_FILE_CACHE_CAPACITY` | `100` | LRU cache size for file-analysis results. |
| `APTU_CODER_METRICS_EXPORT_FILE` | unset | Absolute path for a one-shot JSONL metrics export on shutdown. |
| `APTU_CODER_PROFILE` | unset | Tool subset: `edit` (edit tools + `exec_command` only), `analyze` (analyze tools + `exec_command` only). Also settable per-session via `io.clouatre-labs/profile` in MCP `_meta`. |
| `APTU_SHELL` | unset | Shell for `exec_command`. Defaults to `bash` then `/bin/sh`. |

### Telemetry

| Variable | Default | Description |
|---|---|---|
| `DISABLE_PROMPT_CACHING` | unset | Set to `1` to disable prompt caching (recommended for single-pass subagent sessions). |
| `DISABLE_PROMPT_CACHING_HAIKU` | unset | Set to `1` to disable prompt caching for Haiku-specific pipelines only. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset | OTLP HTTP endpoint URL (e.g., `http://localhost:4318`). When set, enables trace, log, and metric export via OTLP/HTTP; noop providers when unset. |
| `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` | unset | Reserved per OTel GenAI conventions; aptu-coder does not implement this -- bounded parameters are recorded as span attributes instead. |
| `XDG_DATA_HOME` | `~/.local/share` | Base directory for daily-rotated JSONL metrics files (`$XDG_DATA_HOME/aptu-coder/metrics/`, 30-day retention). |

## Observability

The server emits two parallel, independent telemetry streams.

**JSONL metrics (always-on)** are written daily-rotated to `$XDG_DATA_HOME/aptu-coder/metrics/` (fallback: `~/.local/share/aptu-coder/metrics/`) regardless of configuration. Each record captures tool name, duration, output size, and result status. Files are retained for 30 days. See [docs/OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/OBSERVABILITY.md) for the full schema.

**OpenTelemetry export (opt-in)** is enabled when `OTEL_EXPORTER_OTLP_ENDPOINT` is set to an OTLP HTTP endpoint URL. When set, the server initializes OpenTelemetry trace, log, and meter providers and exports asynchronously via OTLP/HTTP. When unset, noop providers are used with zero runtime overhead.

Each tool invocation is wrapped in a span carrying OpenTelemetry GenAI semantic attributes (`gen_ai.system`, `gen_ai.operation.name`, `gen_ai.tool.name`). W3C Trace Context is extracted from the MCP `_meta` field on each call, allowing MCP clients to propagate their trace context so tool spans appear as children in a distributed trace.

For the span attribute policy, the never-record list, and details on what is instrumented, see [OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/OBSERVABILITY.md) at the repository root.

## Documentation

- **[ARCHITECTURE.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/ARCHITECTURE.md)** - Design goals, module map, data flow, language handler system, caching strategy
- **[MCP Best Practices](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/MCP-BEST-PRACTICES.md)** - Best practices for agentic loops, orchestration patterns, MCP tool design, memory management, and safety controls
- **[OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/OBSERVABILITY.md)** - Metrics schema, JSONL format, and retention policy
- **[ROADMAP.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/ROADMAP.md)** - Development history and future direction
- **[DESIGN-GUIDE.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/DESIGN-GUIDE.md)** - Design decisions, rationale, and replication guide for building high-performance MCP servers
- **[CONTRIBUTING.md](https://github.com/clouatre-labs/aptu-coder/blob/main/CONTRIBUTING.md)** - Development workflow, commit conventions, PR checklist
- **[SECURITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/SECURITY.md)** - Security policy and vulnerability reporting

## License

Apache-2.0. See [LICENSE](https://github.com/clouatre-labs/aptu-coder/blob/main/LICENSE) for details.
