# Observability

## Overview

This document covers two parallel telemetry streams and their implementation details.

- **JSONL Metrics** (always-on): Daily-rotated audit trail with tool name, duration, output size, and result status. Files retained for 30 days. Implementation details below.

- **OpenTelemetry Export** (opt-in): When `OTEL_EXPORTER_OTLP_ENDPOINT` is set, traces, logs, and metrics are exported asynchronously via OTLP/HTTP. Noop providers when unset; zero overhead. Parallel and independent from JSONL.

For span attribute policy, the never-record list, and trace context propagation details, see [OBSERVABILITY.md](../OBSERVABILITY.md) at the repository root.

## Channel Pattern

The metrics channel mirrors the `McpLoggingLayer` pattern exactly:

- `unbounded_channel::<MetricEvent>()` created in `main()`
- Sender stored on `CodeAnalyzer` as `MetricsSender` (newtype over `UnboundedSender<MetricEvent>`)
- Receiver moved directly into `MetricsWriter::new()` — no `Arc<TokioMutex<Option<Receiver>>>` wrapper
- Writer task spawned with `tokio::spawn(MetricsWriter::new(metrics_rx, None).run())`
- `MetricsSender::send()` discards `SendError` silently with `.ok()` — fire-and-forget, never blocks the hot path

## Metric Record Schema

Each line in the JSONL file is one JSON object:

| Field | Type | Description |
|---|---|---|
| `ts` | `u64` | Unix timestamp in milliseconds at handler return |
| `tool` | `string` | One of: `analyze_directory`, `analyze_file`, `analyze_module`, `analyze_symbol`, `edit_overwrite`, `edit_replace`, `exec_command` |
| `duration_ms` | `u64` | Wall-clock time from handler entry to return |
| `output_chars` | `usize` | Byte length (`str::len()`) of the final text returned; `0` on error paths |
| `param_path_depth` | `usize` | `Path::components().count()` on `params.path` |
| `max_depth` | `u32 \| null` | The `max_depth` param if present; `null` for `analyze_file` and `analyze_module` |
| `result` | `string` | `"ok"` on success, `"error"` on early-exit error paths |
| `error_type` | `string \| null` | On error: `invalid_params`, `parse`, or `unknown`; `null` on success |
| `session_id` | `string \| null` | Session identifier in format `MILLIS-N` (13-digit Unix milliseconds + AtomicU64 counter); generated on server initialization |
| `seq` | `u32 \| null` | 0-indexed call sequence within session; incremented atomically when emitting each `MetricEvent` at handler return |

### Example record

```json
{"ts":1700000042000,"tool":"analyze_directory","duration_ms":87,"output_chars":1423,"param_path_depth":4,"max_depth":2,"result":"ok","error_type":null,"session_id":"1742468880123-0","seq":0}
```

### Backward compatibility

The `session_id` and `seq` fields are optional (both marked with `#[serde(default)]` in the Rust struct). JSONL files written by older versions without these fields will parse successfully; missing fields default to `None`.

## Daily Rotation and 30-Day Retention

Files are named `metrics-YYYY-MM-DD.jsonl` and stored in the XDG data directory:

- Primary: `$XDG_DATA_HOME/aptu-coder/`
- Fallback: `~/.local/share/aptu-coder/`

The `MetricsWriter` checks the current UTC date on each drain iteration. When the date changes, it closes the current file handle and opens a new one.

`cleanup_old_files()` is the first operation in `MetricsWriter::run()`, called asynchronously before the drain loop begins. It removes any `metrics-*.jsonl` file whose date suffix is more than 30 days in the past. Directory entry read errors are logged at `tracing::warn!` level; individual file deletion failures are ignored.

## Testability

`MetricsWriter::new` accepts `base_dir: Option<PathBuf>` as its second argument. Pass `Some(tempdir.path().to_path_buf())` in tests to write metrics to a temporary directory instead of the XDG data dir:

```rust
let tmp = tempfile::tempdir().unwrap();
let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
let writer = MetricsWriter::new(rx, Some(tmp.path().to_path_buf()));
tokio::spawn(writer.run());
// ... send events via tx, then verify JSONL files in tmp.path()
```

This avoids `XDG_DATA_HOME` environment variable manipulation in tests.

## Risks

- **Clock skew**: `unix_ms()` uses `SystemTime::now()` which can go backward under NTP adjustment. Events may appear out of order in the JSONL file. This is acceptable for observability purposes.
- **Unbounded channel backpressure**: If the writer task falls behind (slow disk I/O), the unbounded channel will grow. This is acceptable because metrics writes are the lowest-priority operation. A future enhancement could add a bounded channel with a drop-on-full policy.
- **Date arithmetic**: The Gregorian calendar implementation in `current_date_str()` does not handle leap seconds. Off-by-one errors at year boundaries are possible but inconsequential for 30-day retention logic.
- **Hint semantics**: Per MCP Blog 2, `readOnlyHint` and `idempotentHint` are not enforced by the protocol. Clients make their own trust decisions.
