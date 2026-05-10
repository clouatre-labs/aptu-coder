# Observability

This document defines the observability contract for aptu-coder: what is instrumented,
what is deliberately excluded, and how to configure telemetry export.

## Span attribute policy

### Always record (bounded, no secrets possible)

These values are safe to record as span attributes and appear in all tool spans.

| Attribute | Type | Source |
|---|---|---|
| `gen_ai.system` | `"mcp"` | constant |
| `gen_ai.operation.name` | `"execute_tool"` | constant |
| `gen_ai.tool.name` | tool name string | constant per handler |
| `service.name` | `"aptu-coder"` | `env!("CARGO_PKG_VERSION")` at init |
| `service.version` | semver string | `env!("CARGO_PKG_VERSION")` at init |
| `path` | filesystem path | analyze/edit params |
| `symbol` | symbol name | `analyze_symbol` param |
| `follow_depth`, `max_depth` | integers | analyze params |
| `match_mode`, `impl_only`, `import_lookup` | bounded enums/bools | analyze params |
| `summary`, `git_ref` | bool/optional string | analyze params |
| `working_dir` | filesystem path | exec_command param |
| `exit_code` | integer | exec_command result |
| `timed_out`, `output_truncated` | booleans | exec_command result |
| `error` | boolean | error paths |
| `error.type` | error category string | error paths |
| `cache_hit`, `auto_summary`, `truncated` | booleans | span events |

### Never record

These values must not appear as span attributes, span events, or log fields emitted
by the server under any configuration, including debug level.

| Value | Reason |
|---|---|
| `command` string (exec_command param) | May contain env vars, tokens, passwords, API keys inline |
| `stdin` content | Explicit user data piped into a process |
| stdout / stderr content | Returned to client by design; duplicating into spans leaks content to observability backends |
| `content` (edit_overwrite param) | Arbitrary file content supplied by user |
| `old_text`, `new_text` (edit_replace params) | Arbitrary file content supplied by user |
| `gen_ai.tool.call.arguments` as a serialized blob | Captures all parameters including any that may contain secrets |
| `gen_ai.tool.call.result` as a serialized blob | Captures full tool output |

The OTel GenAI semantic conventions mark `gen_ai.tool.call.arguments` and
`gen_ai.tool.call.result` as likely to contain sensitive information. The spec
requires them to be opt-in, gated on
`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true`. aptu-coder does not
implement this opt-in: individual bounded parameters (path, symbol, depth) are
recorded instead of serializing the full argument object.

### PR review checklist

When adding or modifying span attributes, confirm:

- [ ] The value is bounded (enum, integer, boolean, or a path the user explicitly passed as a tool parameter)
- [ ] The value cannot contain user-supplied free-form content (file content, command output, stdin)
- [ ] The value cannot contain credentials (tokens, passwords, API keys)
- [ ] If the value is a string, it has bounded cardinality suitable for use as a metric label

## Enabling telemetry export

By default, no telemetry is exported. The tracing subscriber operates in noop mode
with zero export overhead.

To export traces via OTLP:

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 aptu-coder
```

When this variable is set, spans are exported asynchronously via `BatchSpanProcessor`.
There is no latency impact on tool call handling.

### Collector-side redaction (recommended for production)

Even with the never-record policy above, deploy an OTel Collector redaction processor
as a backstop for any future regressions:

```yaml
processors:
  redaction:
    allow_all_keys: true
    blocked_values:
      # AWS access keys
      - "((?:A3T[A-Z0-9]|AKIA|AGPA|AROA|AIPA|ANPA|ANVA|ASIA)[A-Z0-9]{16})"
      # GitHub tokens
      - "(?:ghp|gho|ghu|ghs|ghr|github_pat)_[A-Za-z0-9_]{20,}"
      # Bearer tokens
      - "Bearer [A-Za-z0-9\\-._~+/]+=*"
      # Generic 32+ char hex secrets
      - "[0-9a-f]{32,}"
```

This is a deployment concern, not a codebase change. The SDK-level never-record
policy is the primary control; the Collector processor is the safety net.

## Log level and MCP client visibility

Logs are forwarded to the MCP client (the calling agent) via the `notifications/message`
protocol. The default log level forwarded to the client is WARN.

At DEBUG level, some log events include filesystem paths (from cache and analysis
operations). These are not secrets, but they do reveal filesystem structure including
home directory paths. Do not set DEBUG level in contexts where log content is stored
in untrusted systems.

To change the log level at runtime, use the MCP `logging/setLevel` RPC.

## Span events for behavioral decisions

The following decisions are recorded as span events (not attributes) when they occur.
They carry no value payload, only their occurrence:

| Event | Meaning |
|---|---|
| `auto_summary` | Response was auto-summarized due to size |
| `cache_hit` | Result was served from in-process cache |
| `truncated` | Output was truncated to fit size limit |

## References

- [OTel GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/) (Development status)
- [OTel MCP semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/mcp/)
- [OTel Collector redaction processor](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/processor/redactionprocessor)
- Observability roadmap: [ROADMAP.md](ROADMAP.md), milestone `observability-v1`
