<p align="center">

<h1 align="center">aptu-coder</h1>

<p align="center">
  <a href="https://crates.io/crates/aptu-coder"><img alt="crates.io" src="https://img.shields.io/crates/v/aptu-coder.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20"></a>
  <a href="https://slsa.dev"><img alt="SLSA Level 3" src="https://img.shields.io/badge/SLSA-Level%203-green?style=for-the-badge" height="20"></a>
  <a href="https://www.bestpractices.dev/projects/12275"><img alt="OpenSSF Best Practices" src="https://img.shields.io/cii/level/12275?style=for-the-badge" height="20"></a>
</p>

<p align="center">A Model Context Protocol (MCP) code-intelligence server that gives AI coding agents pre-parsed symbol tables and call graphs instead of raw file bytes, cutting token usage by up to 59% in the cited benchmark conditions (see <a href="#benchmarks">Benchmarks</a>). OpenSSF silver certified: fewer than 1% of open source projects reach this level.</p>

<!-- mcp-name: io.github.clouatre-labs/aptu-coder -->

> [!NOTE]
> Native agent tools (regex search, path matching, file reading) handle targeted lookups well. `aptu-coder` handles the mechanical, non-AI work: mapping directory structure, extracting symbols, and tracing call graphs, so a coding agent's context window is spent reasoning instead of re-deriving structure on every call.

Most code-intelligence tooling for AI agents indexes a codebase into a cloud embedding or vector store and retrieves by similarity search. aptu-coder instead parses the codebase into a structural graph on-device with tree-sitter and serves it locally over MCP: no source code leaves the machine, and retrieval is exact (symbol tables and call graphs) rather than approximate (nearest-neighbor similarity).

## Benchmarks

Paired comprehension tasks on Claude Code (MCP mode vs native shell search). Methodology: [Django](https://github.com/django/django) auth migration ([v12](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/benchmarks/v12/methodology.md)) and [OpenFAST](https://github.com/OpenFAST/openfast) AeroDyn integration audit ([v13](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/benchmarks/v13/methodology.md)).

| Task | Mode | Sonnet 4.6 | Haiku 4.5 | Savings |
|---|---|---|---|---|
| Django (Python) | MCP | 112k tokens, $0.39 | 406k tokens, $0.42 | **59% fewer tokens, 59% cheaper** (Sonnet) |
| Django (Python) | Native | 276k tokens, $0.95 | 473k tokens, $0.53 | 14% fewer tokens, 21% cheaper (Haiku) |
| OpenFAST (Fortran) | MCP | 472k tokens, $1.65 | 687k tokens, $0.72 | **46% fewer tokens, 42% cheaper** (Sonnet) |
| OpenFAST (Fortran) | Native | 877k tokens, $2.85 | 2162k tokens, $2.21 | **68% fewer tokens, 68% cheaper** (Haiku) |

A real end-to-end multi-agent run against this repository itself: five refactoring issues (#1578–#1582) fixed by the [coder skill](https://github.com/clouatre-labs/agentic-coder-skill) in one orchestrated session — 23 subagent spawns (5 scout, 1 guard, 10 build, 7 check), ~1h42m, three human interventions — producing PRs #1584–#1588, all merged with CI green. [Case study](https://github.com/clouatre-labs/agentic-coder-skill/blob/main/docs/examples/2026-09-aptu-coder-5-issues.md).

### Scope of the benchmark claims

The token-savings figures above come from single, focused comprehension tasks where the agent used aptu-coder instead of shell search. Agentic coding loops have bash and ripgrep and can search the repo directly. The [v19 benchmark program](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/audit/2026-09-30-v19-pivot.md) (N=20 paired sessions, pre-registered claim gate) measured aptu-coder in those loops: no measurable answer-quality gain over bash + ripgrep on structural fan-in tasks (pooled paired F1 diff mean -0.071, 95% CI [-0.179, +0.038]; gate NOT-SUPPORTED); tool activation itself was reliable when tools were surfaced directly. Recommended uses: premise grounding for multi-agent workflows, and non-agentic MCP consumers (IDE assistants, CI jobs, scoring pipelines).

## Overview

aptu-coder is a comprehension layer for coding agents: it gives an agent harness precise structural context about a codebase, directory trees, symbol definitions, and call graphs, without the agent reading raw files or re-deriving structure on every call. That prevents context starvation on large or unfamiliar codebases while keeping the result set small enough to fit the model's context window. It supports 18 languages (see [Supported Languages](#supported-languages)) and integrates with any MCP-compatible orchestrator.

Structural context is built once and reused: an on-disk cache keyed by blake3 content hashes keeps results correct across concurrent writes. For consumers that hold source text without an on-disk path, `aptu-coder-core` also exposes `analyze_str` as a public Rust library API (see [ARCHITECTURE.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/ARCHITECTURE.md)).

Further reading on the design philosophy, from the author's [blog](https://clouatre.ca/blog/): [The Agentic SDLC Governance Stack](https://clouatre.ca/blog/ai-sdlc-governance-stack/), [Context Engineering for Multi-Agent Reliability](https://clouatre.ca/blog/context-engineering-multi-agent-reliability/), and [Orchestrating AI Agents: Subagent Architecture](https://clouatre.ca/blog/orchestrating-ai-agents-subagent-architecture/).

## Supported Languages

Astro, C/C++, C#, CSS, Fortran, Go, HTML, Java, JavaScript, JSON, Kotlin, Markdown, Python, Rust, TOML, TSX, TypeScript, YAML. The full extension map lives in [`crates/aptu-coder-core/src/lang.rs`](https://github.com/clouatre-labs/aptu-coder/blob/main/crates/aptu-coder-core/src/lang.rs).

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

**stdio (default)** — the client spawns and owns the server process; this works with orchestrators that spawn delegates (goose, pi) as well as single clients:

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

**Streamable HTTP (optional)** — runs a single shared server process instead of one per client. Only worth it if you specifically want a daemon-style setup:

```bash
aptu-coder --port 49200            # or: brew services start aptu-coder
claude mcp add --transport http aptu-coder http://127.0.0.1:49200/mcp
```

For goose, add to `~/.config/goose/config.yaml`:

```yaml
extensions:
  aptu-coder:
    type: streamable_http
    uri: http://127.0.0.1:49200/mcp
    name: aptu-coder
    timeout: 300
```

## Tools

All optional parameters may be omitted. Shared optional parameters for `analyze_directory`, `analyze_file`, and `analyze_module`:

All optional parameters may be omitted. `analyze_directory`, `analyze_file`, and `analyze_module` accept `summary` (compact output, auto-triggers above 50K chars) and `cursor` (pagination token). Page size is server-owned; there is no client `page_size` parameter (passing one returns INVALID_PARAMS).

- `analyze_directory` -- directory tree with LOC, function, and class counts; respects `.gitignore`
- `analyze_file` -- functions, classes, and imports with signatures and line ranges; graceful fallback for unsupported extensions
- `analyze_module` -- lightweight function and import index (~75% smaller than `analyze_file`)
- `edit_overwrite` -- create or overwrite a file; creates parent directories
- `edit_replace` -- replace a unique exact text block (or all occurrences with `replace_all`); batch form `edits[]` applies multiple replacements atomically; optional `expected_content_hash` rejects stale edits
- `exec_command` -- run a shell command; output is capped, filtered, and returned with exit code; cancelled requests kill the command

Tool parameters, constraints, and examples are available via your MCP client's tool inspector or `tools/list` response.

Tool parameters, constraints, and examples are available via your MCP client's tool inspector or `tools/list` response.

## Output Management

For large codebases, several mechanisms prevent context overflow.

**Pagination**

`analyze_file` appends a `NEXT_CURSOR:` line when output is truncated. Pass the token back as `cursor` to fetch the next page. `summary=true` and `cursor` are mutually exclusive; passing both returns an error.

```
# Response ends with:
NEXT_CURSOR: eyJvZmZzZXQiOjUwfQ==

# Fetch next page:
analyze_file path: /my/project/src/lib.rs cursor: eyJvZmZzZXQiOjUwfQ==
```

**exec_command output caps**

`exec_command` applies three independent byte-level caps to prevent large command outputs from flooding the context:

stdout keeps the last 30,000 chars, stderr the last 10,000 (tail-preserving, so errors appear at the end), and the combined `output_text` is capped at 50,000 chars as a safety net after interleaving.

The 30k stdout cap is data-driven: analysis of 27,981 observed `exec_command` calls shows only 0.33% exceed this limit. When any cap fires, `output_truncated: true` is set in the response and recorded in the JSONL metrics.

**Recovering capped output:** when output is capped, the response includes MCP resource links (`aptu-overflow://slot-{seq}/stdout`, `/stderr`, `/interleaved`) pointing to the full raw capture for that invocation (last-run-wins); read them via `resources/read`. If a filter capped the output, a hint line points to the pre-filter capture file.

A server-owned post-exit drain window (500 ms) waits for any background subprocess still holding the pipe open. If it expires before the pipe closes, `exit_code` is `null` and `output_truncated: true` is set.

**exec_command output filters**

A built-in filter table suppresses per-file noise from chatty CLI tools before output reaches the model. Filters apply to stdout, stderr, and interleaved output on success only; raw output is always preserved on failure.

Built-in rules cover the usual suspects: `git pull`/`fetch`/`push` strip diff-stat, remote-progress, and ref noise; `git log`/`status` cap at 20 lines; `git show` strips patch hunks; `git diff` strips ANSI escapes; `git commit`/`add` strip hook output; `cargo build`/`test` strip `Compiling`/`Checking`/`Fresh` progress lines. Clean output collapses to a short `ok` confirmation.

Project-local rules can be added in `.aptu/filters.toml`. Parse errors and unrecognized schema_version values (version != 1) fall back to the built-in table with a logged warning; no crash occurs. When a filter fires, `filter_applied` in `structuredContent` identifies which rule matched.

## Non-Interactive Pipelines

In single-pass subagent sessions, prompt caches are written but never reused. Benchmarks showed MCP responses writing ~2x more to cache than native-only workflows, adding cost with no quality gain. Set `DISABLE_PROMPT_CACHING=1` (or `DISABLE_PROMPT_CACHING_HAIKU=1` for Haiku-specific pipelines) to avoid this overhead.

The server's own instructions are deliberately minimal, a single short paragraph: start with `analyze_directory` for a package/module map, then use `analyze_module`/`analyze_file` for per-file detail, with a pointer to `docs/METRICS.md` for JSONL metrics and jq recipes. MCP clients that surface server instructions will present this guidance automatically to the agent.

## Environment Variables

### Cache and runtime

| Variable | Default | Description |
|---|---|---|
| `APTU_CODER_DIR_CACHE_CAPACITY` | `20` | LRU cache size for directory-analysis results. |
| `APTU_CODER_DISK_CACHE_DIR` | `$XDG_DATA_HOME/aptu-coder/analysis-cache` | Directory for the L2 analysis disk cache. Created automatically if it does not exist. |
| `APTU_CODER_DISK_CACHE_DISABLED` | unset | Set to `1` to disable the L2 analysis disk cache entirely. |
| `APTU_CODER_FILE_CACHE_CAPACITY` | `100` | LRU cache size for file-analysis results. |
| `APTU_CODER_METRICS_EXPORT_FILE` | unset | Absolute path for a one-shot JSONL metrics export on shutdown. |
| `APTU_CODER_PORT` | unset | Port for streamable HTTP mode. Equivalent to `--port N`; `--port` takes precedence. When unset and `--port` is not passed, stdio mode is used. |
| `APTU_CODER_BEARER_TOKEN` | unset | Bearer token for Streamable HTTP transport authentication. A warning is logged if the value is fewer than 32 characters. |
| `APTU_SHELL` | unset | Shell for `exec_command`. Defaults to `bash` then `/bin/sh`. |

## Observability

For the JSONL schema, OTel configuration, and span attribute policy, see:

- [OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/OBSERVABILITY.md) (repository root)
- [docs/METRICS.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/METRICS.md)

## Documentation

- **[Blog](https://clouatre.ca/blog/)** - Design philosophy and agent-harness engineering behind aptu-coder
- **[AGENTS.md](https://github.com/clouatre-labs/aptu-coder/blob/main/AGENTS.md)** - Contributor reference: project structure, commands, rmcp footguns, tool parameter constraints
- **[ARCHITECTURE.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/ARCHITECTURE.md)** - Design goals, module map, data flow, language handler system, caching strategy
- **[CONTRIBUTING.md](https://github.com/clouatre-labs/aptu-coder/blob/main/CONTRIBUTING.md)** - Development workflow, commit conventions, PR checklist
- **[DESIGN-GUIDE.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/DESIGN-GUIDE.md)** - Design decisions, rationale, and replication guide for building high-performance MCP servers
- **[MCP Best Practices](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/MCP-BEST-PRACTICES.md)** - Best practices for agentic loops, orchestration patterns, MCP tool design, memory management, and safety controls
- **[METRICS.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/METRICS.md)** - Metrics schema, JSONL format, and retention policy
- **[ROADMAP.md](https://github.com/clouatre-labs/aptu-coder/blob/main/docs/ROADMAP.md)** - Development history and future direction
- **[SECURITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/SECURITY.md)** - Security policy and vulnerability reporting

## License

Apache-2.0. See [LICENSE](https://github.com/clouatre-labs/aptu-coder/blob/main/LICENSE) for details.
