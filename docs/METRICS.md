# Observability

## Overview

This document covers two parallel telemetry streams and their implementation details.

- **JSONL Metrics** (always-on): Daily-rotated audit trail with tool name, duration, output size, and result status. Files retained for 30 days. Implementation details below.

- **OpenTelemetry Export** (opt-in): When `OTEL_EXPORTER_OTLP_ENDPOINT` is set, traces, logs, and metrics are exported asynchronously via OTLP/HTTP. Noop providers when unset; zero overhead. Parallel and independent from JSONL.

For span attribute policy, the never-record list, and trace context propagation details, see [OBSERVABILITY.md](https://github.com/clouatre-labs/aptu-coder/blob/main/OBSERVABILITY.md).

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
| `filter_applied` | `string \| null` | The filter rule name that caused output suppression via `.aptu/filters.toml`; `null` when no filter was applied. Omitted from JSONL when `null`. |
| `timed_out` | `bool` | `true` when the child process was killed because it exceeded `timeout_secs`; `false` otherwise. Omitted from JSONL when `false` (`#[serde(skip_serializing_if)]`). |
| `output_truncated` | `bool \| null` | `true` if any truncation occurred (line cap, per-stream byte cap, or combined cap); `false` if the command completed without truncation; `null` for all non-`exec_command` tools and for `exec_command` calls emitted by older server versions |
| `chars_threshold_breach` | `bool` | `true` when `output_chars > 30,000`; fires for the top ~0.33% of `exec_command` calls (p99.7 of 27,981 observed calls). Early-warning signal for responses approaching the per-stream byte-cap threshold (MAX_STDOUT_BYTES = 30,000). Omitted from JSONL when `false` (`#[serde(skip_serializing_if)]`); defaults to `false` on parse for backward compatibility. |
| `stdout_bytes_raw` | `u64 \| null` | Raw stdout bytes read before any truncation; populated only when `output_truncated=true`, `timed_out=false`, and no drain-abort occurred. Omitted from JSONL when `null` (`#[serde(skip_serializing_if)]`). |
| `stderr_bytes_raw` | `u64 \| null` | Raw stderr bytes read before any truncation; populated only when `output_truncated=true`, `timed_out=false`, and no drain-abort occurred. Omitted from JSONL when `null` (`#[serde(skip_serializing_if)]`). |
| `git_ref_used` | `bool` | `true` when the `git_ref` parameter was supplied on `analyze_directory` or `analyze_symbol`. Omitted from JSONL when `false`. |
| `summary_mode` | `bool` | `true` when `summary=true` was set on `analyze_directory`, `analyze_file`, or `analyze_symbol`. Omitted from JSONL when `false`. |
| `is_paginated` | `bool` | `true` when a `cursor` was supplied on `analyze_directory`, `analyze_file`, or `analyze_symbol` (i.e., this is a continuation page). Omitted from JSONL when `false`. |
| `fields_projected` | `bool` | `true` when the `fields` projection parameter was supplied on `analyze_file`. Omitted from JSONL when `false`. |
| `match_mode` | `string \| null` | The `match_mode` value passed to `analyze_symbol` (e.g. `"exact"`, `"contains"`); `null` when not set (defaults to `exact` in the handler). Omitted from JSONL when `null`. |
| `follow_depth` | `u32 \| null` | The `follow_depth` value passed to `analyze_symbol`; `null` when the parameter was not explicitly supplied. Omitted from JSONL when `null`. |
| `import_lookup` | `bool` | `true` when `import_lookup=true` was set on `analyze_symbol`. Omitted from JSONL when `false`. |
| `def_use` | `bool` | `true` when `def_use=true` was set on `analyze_symbol`. Omitted from JSONL when `false`. |
| `impl_only` | `bool` | `true` when `impl_only=true` was set on `analyze_symbol`. Omitted from JSONL when `false`. |
| `stdin_provided` | `bool` | `true` when the `stdin` parameter was supplied to `exec_command` (presence-only; content is never recorded). Omitted from JSONL when `false`. |
| `timeout_configured_ms` | `i64 \| null` | `timeout_secs * 1000` when `timeout_secs` was supplied to `exec_command`; `null` when the parameter was not set (no limit). Omitted from JSONL when `null`. |
| `drain_timeout_ms` | `i64 \| null` | The raw `drain_timeout_secs` value passed to `exec_command` (stored as-is; represents milliseconds in the server parameter despite the naming); `null` when not set (defaults to 500 ms in the handler). Omitted from JSONL when `null`. |
| `working_dir_used` | `bool` | `true` when the `working_dir` parameter was supplied to `exec_command`, `edit_overwrite`, or `edit_replace`. Omitted from JSONL when `false`. |
| `l1_eviction_count` | `u64 \| null` | Number of L1 in-memory LRU evictions that have occurred in the cache since process start, at the time of metric emission. Process-lifetime counter; resets on restart. Omitted from JSONL when `null`. Only populated for `analyze_symbol` calls that use the call-graph cache. |
| `l2_entry_count` | `u64 \| null` | Approximate number of entries currently tracked in the L2 disk cache, at the time of metric emission. Incremented on successful `put()`; approximate (does not account for manual deletions). Omitted from JSONL when `null`. Only populated for `analyze_symbol` calls. |
| `l2_size_bytes` | `u64 \| null` | Approximate total compressed size in bytes of L2 disk cache entries, at the time of metric emission. Incremented on successful `put()` by the compressed entry size; approximate (does not account for evictions or manual deletions). Omitted from JSONL when `null`. Only populated for `analyze_symbol` calls. |

### cache_tier values

The `cache_tier` field encodes where a result was found (or not found):

| Value | Meaning |
|---|---|
| `l1_memory` | Result served from the in-process LRU cache (L1). |
| `l2_disk` | Result served from the on-disk cache (L2); L1 was a miss. |
| `l1_only_miss` | Both L1 and the tool path were checked; no L2 disk cache was available (disabled or not configured). |
| `l1_l2_miss` | Both L1 and L2 were checked; neither held a matching entry. Full computation was performed. |
| `miss` | Legacy value emitted by older server versions; semantically equivalent to `l1_l2_miss`. |

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
| `stdout_bytes_raw` | v0.21.x | `null` (omitted when null; populated only on `exec_command` with `output_truncated=true`, `timed_out=false`, and no drain-abort) |
| `stderr_bytes_raw` | v0.21.x | `null` (omitted when null; populated only on `exec_command` with `output_truncated=true`, `timed_out=false`, and no drain-abort) |
| `filter_applied` | v0.14.2 | `null` (omitted from JSONL when null; only present for `exec_command` calls where a filter matched) |
| `cache_tier` | v0.18.x | `null` (omitted when null; `l1_memory` or `l2_disk` on a cache hit) |
| `cache_write_failure` | v0.18.x | `null` (omitted when null; `true` only when an L2 disk write failed) |
| `error_subtype` | v0.18.x | `null` (omitted when null; e.g. `not_found`, `ambiguous` for `edit_replace` errors) |
| `language` | v0.20.x | `null` (omitted when null; populated for `analyze_file` and `analyze_module` only) |
| `file_ext` | v0.20.x | `null` (omitted when null; populated for `analyze_file` and `analyze_module` only) |
| `timed_out` | v0.20.1 | `false` (omitted from JSONL when false; set when child process was killed by `timeout_secs`) |
| `git_ref_used` | v0.23.0 | `false` (omitted when false; `true` only when `git_ref` was supplied) |
| `summary_mode` | v0.23.0 | `false` (omitted when false; `true` only when `summary=true` was set) |
| `is_paginated` | v0.23.0 | `false` (omitted when false; `true` only when a `cursor` was supplied) |
| `fields_projected` | v0.23.0 | `false` (omitted when false; `true` only when `fields` was supplied on `analyze_file`) |
| `match_mode` | v0.23.0 | `null` (omitted when null; present only when explicitly set on `analyze_symbol`) |
| `follow_depth` | v0.23.0 | `null` (omitted when null; present only when explicitly set on `analyze_symbol`) |
| `import_lookup` | v0.23.0 | `false` (omitted when false; `true` only when `import_lookup=true` was set) |
| `def_use` | v0.23.0 | `false` (omitted when false; `true` only when `def_use=true` was set) |
| `impl_only` | v0.23.0 | `false` (omitted when false; `true` only when `impl_only=true` was set) |
| `stdin_provided` | v0.23.0 | `false` (omitted when false; `true` only when `stdin` was supplied to `exec_command`) |
| `timeout_configured_ms` | v0.23.0 | `null` (omitted when null; present only when `timeout_secs` was supplied) |
| `drain_timeout_ms` | v0.23.0 | `null` (omitted when null; present only when `drain_timeout_secs` was supplied) |
| `working_dir_used` | v0.23.0 | `false` (omitted when false; `true` only when `working_dir` was supplied) |
| `l1_eviction_count` | v0.25.0 | `null` (omitted when null; process-lifetime L1 LRU eviction counter; only for `analyze_symbol`) |
| `l2_entry_count` | v0.25.0 | `null` (omitted when null; approximate L2 entry count; only for `analyze_symbol`) |
| `l2_size_bytes` | v0.25.0 | `null` (omitted when null; approximate L2 compressed size in bytes; only for `analyze_symbol`) |

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
