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
| `file_ext` | `string \| null` | Lowercased file extension of `params.path`: known extension key (e.g. `"rs"`), `"other"` for unrecognized extensions, `null` when the path has no extension. Only populated for `analyze_file` and `analyze_module`. |
| `language` | `string \| null` | Human-readable programming language name derived from the file extension (e.g. `"rust"` for `.rs`). `null` when the path has no extension or the extension is not recognized. Only populated for `analyze_file` and `analyze_module`. Omitted from JSONL when `null` for backward compatibility. |
| `max_depth` | `u32 \| null` | The `max_depth` param if present; `null` for `analyze_file` and `analyze_module` |
| `result` | `string` | `"ok"` on success, `"error"` on early-exit error paths |
| `error_type` | `string \| null` | On error: `invalid_params`, `parse`, or `unknown`; `null` on success |
| `error_subtype` | `string \| null` | On error: detailed subtype (e.g., `not_found`, `ambiguous` for `edit_replace`); `null` on success or for generic errors. Omitted from JSONL when `null` for backward compatibility. |
| `cache_hit` | `bool \| null` | `true` if the result was served from cache (L1 or L2); `false` if computed; `null` if caching is not applicable for this tool |
| `session_id` | `string \| null` | Session identifier in format `MILLIS-N` (13-digit Unix milliseconds + AtomicU64 counter); generated on server initialization |
| `seq` | `u32 \| null` | 0-indexed call sequence within session; incremented atomically when emitting each `MetricEvent` at handler return |
| `cache_tier` | `string \| null` | Disk cache tier hit: `l1_memory` or `l2_disk`; `null` if not applicable |
| `cache_write_failure` | `bool \| null` | `true` if cache write failed (dir, tempfile, write, or rename); `null` if not applicable |
| `exit_code` | `i32 \| null` | Process exit code for `exec_command`; `null` if not applicable or if the process was killed due to timeout |
| `timed_out` | `bool` | `true` when the child process was killed because it exceeded `timeout_secs`; `false` otherwise. Omitted from JSONL when `false` (`#[serde(skip_serializing_if)]`). |
| `output_truncated` | `bool \| null` | `true` if any truncation occurred (line cap, per-stream byte cap, or combined cap); `false` if the command completed without truncation; `null` for all non-`exec_command` tools and for `exec_command` calls emitted by older server versions |
| `filter_applied` | `string \| null` | Name of the filter rule that matched and transformed the output (e.g., `"git pull"`, `"cargo build"`); `null` when no filter fired or for non-`exec_command` tools. Omitted from JSONL when `null` (`#[serde(skip_serializing_if)]`). |
| `chars_threshold_breach` | `bool` | `true` when `output_chars > 30,000`; fires for the top ~0.33% of `exec_command` calls (p99.7 of 27,981 observed calls). Early-warning signal for responses approaching the per-stream byte-cap threshold (MAX_STDOUT_BYTES = 30,000). Omitted from JSONL when `false` (`#[serde(skip_serializing_if)]`); defaults to `false` on parse for backward compatibility. |

### Example record

```json
{"ts":1700000042000,"tool":"analyze_directory","duration_ms":87,"output_chars":1423,"param_path_depth":4,"max_depth":2,"result":"ok","error_type":null,"session_id":"1742468880123-0","seq":0}
```

### Backward compatibility

The following fields are optional (marked with `#[serde(default)]` in the Rust struct). JSONL files written by older server versions without these fields parse successfully; missing fields default to `null` or `false` as indicated:

| Field | Added in | Default when absent |
|---|---|---|
| `session_id` | early | `null` |
| `seq` | early | `null` |
| `output_truncated` | v0.14.2 | `null` (treat as unknown; does not mean truncation did not occur) |
| `chars_threshold_breach` | v0.14.2 | `false` (omitted from JSONL when false; safe to query with `// false`) |
| `filter_applied` | v0.14.2 | `null` (omitted from JSONL when null; only present for `exec_command` calls where a filter matched) |
| `cache_tier` | v0.18.x | `null` (omitted when null; `l1_memory` or `l2_disk` on a cache hit) |
| `cache_write_failure` | v0.18.x | `null` (omitted when null; `true` only when an L2 disk write failed) |
| `error_subtype` | v0.18.x | `null` (omitted when null; e.g. `not_found`, `ambiguous` for `edit_replace` errors) |
| `language` | v0.20.x | `null` (omitted when null; populated for `analyze_file` and `analyze_module` only) |
| `file_ext` | v0.20.x | `null` (omitted when null; populated for `analyze_file` and `analyze_module` only) |
| `timed_out` | v0.20.1 | `false` (omitted from JSONL when false; set when child process was killed by `timeout_secs`) |

The five jq one-liners in `AGENTS.md` do not reference `output_truncated` and are unaffected. To query truncation events across all retained JSONL files:

```bash
cd ~/.local/share/aptu-coder && jq -r 'select(.output_truncated==true) | [.tool, .output_chars, (.session_id//"?")] | @tsv' metrics-*.jsonl | sort -t$'\t' -k2 -rn
```

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

## Metrics CLI

`scripts/mcp-metrics.py` is a zero-dependency Python CLI (stdlib only, Python 3.8+) for interactive analysis of the aptu-coder JSONL metrics corpus. It reads daily-rotated files from the same XDG path used by the server (`$XDG_DATA_HOME/aptu-coder/`, defaulting to `~/.local/share/aptu-coder/`) and produces four analysis sections: tool efficiency (output size p50/p95/max, latency p50/p95), cache health (hit rate by tool and tier, estimated token savings), session patterns (top sessions by call volume and output chars, error rate by tool and error type), and an optional daily trend view. All optional JSONL fields introduced after the initial release are accessed with `dict.get` so records from any server version parse cleanly.

```
# Full summary on the default XDG path
python scripts/mcp-metrics.py

# Restrict to a date range
python scripts/mcp-metrics.py --from 2026-05-01 --to 2026-05-24

# Override the metrics directory
python scripts/mcp-metrics.py --dir /path/to/metrics/

# Machine-readable JSON output
python scripts/mcp-metrics.py --format json

# CSV output (sections separated by blank rows)
python scripts/mcp-metrics.py --format csv

# Focus on a single tool
python scripts/mcp-metrics.py --tool exec_command

# Daily trend breakdown
python scripts/mcp-metrics.py --trend

# Combine: trend as JSON, piped to jq
python scripts/mcp-metrics.py --trend --format json | jq .trend
```

The script is OTel-aligned: field names match the JSONL schema defined in the Metric Record Schema section above. No external packages are required; it runs with any Python 3.8+ interpreter.
