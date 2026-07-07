// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Metrics collection and daily-rotating JSONL emission.
//!
//! Provides a channel-based pipeline: callers emit [`MetricEvent`] values via [`MetricsSender`],
//! and [`MetricsWriter`] drains the channel and appends events to a daily-rotated JSONL file
//! under the XDG data directory (`~/.local/share/aptu-coder/metrics-YYYY-MM-DD.jsonl`).
//! Files older than 30 days are deleted on startup.

// Re-export types from metrics_export so the lib.rs re-export chain stays intact.
pub use crate::metrics_export::MetricsWriter;
pub use crate::metrics_export::migrate_legacy_metrics_dir;
// Re-export helpers used by tool handlers via crate::metrics::*
pub(crate) use crate::metrics_export::{
    path_component_count, path_file_ext, path_language, unix_ms,
};

use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{KeyValue, global};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// A single metric event emitted by a tool invocation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricEvent {
    pub ts: u64,
    pub tool: &'static str,
    pub duration_ms: u64,
    pub output_chars: usize,
    pub param_path_depth: usize,
    pub max_depth: Option<u32>,
    pub result: &'static str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_subtype: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub seq: Option<u32>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_hit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_tier: Option<&'static str>,
    /// Set to Some(true) when an L2 disk cache write fails (dir, tempfile, write, or rename).
    /// Drives the cache_write_failures_total OTEL counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_failure: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_truncated: Option<bool>,
    /// True when `output_chars > 30_000`; fires for the top ~0.33% of exec_command calls
    /// (p99.7 of 27,981 observed calls). Early-warning signal for responses approaching
    /// the per-stream byte-cap threshold.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chars_threshold_breach: bool,
    /// File extension of the analyzed path, lowercased. `Some("rs")` for known extensions,
    /// `Some("other")` for unrecognized extensions, `None` when the path has no extension.
    /// Only populated for `analyze_file` and `analyze_module`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_ext: Option<&'static str>,
    /// Name of the filter rule that matched and transformed exec_command output.
    /// `None` when no filter fired or for non-`exec_command` tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_applied: Option<String>,
    /// Human-readable programming language name derived from the file extension
    /// (e.g., `Some("Rust")` for `.rs` files). `None` when the path has no extension
    /// or the extension is not recognized. Only populated for `analyze_file` and
    /// `analyze_module`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Whether the tool call used a `git_ref` parameter. Populated by `analyze_directory`
    /// and `analyze_symbol`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub git_ref_used: bool,
    /// Whether the tool call used `summary=true` or was auto-summarized.
    /// Populated by `analyze_directory`, `analyze_file`, and `analyze_symbol`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub summary_mode: bool,
    /// Whether the tool call used pagination (`cursor` was provided).
    /// Populated by `analyze_directory`, `analyze_file`, and `analyze_symbol`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_paginated: bool,
    /// Whether the `fields` parameter was provided to `analyze_file`.
    /// Populated by `analyze_file`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fields_projected: bool,
    /// Symbol matching mode used by `analyze_symbol` (e.g., "exact", "insensitive").
    /// `None` when not an `analyze_symbol` call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_mode: Option<String>,
    /// Call graph traversal depth for `analyze_symbol` (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_depth: Option<u32>,
    /// Whether `import_lookup=true` was set on `analyze_symbol`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub import_lookup: bool,
    /// Whether `def_use=true` was set on `analyze_symbol`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub def_use: bool,
    /// Whether `impl_only=true` was set on `analyze_symbol`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub impl_only: bool,
    /// Whether `stdin` was provided to `exec_command`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stdin_provided: bool,
    /// Configured timeout in milliseconds for `exec_command`. `None` means no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_configured_ms: Option<i64>,
    /// Drain timeout in milliseconds for `exec_command`. `None` means default (500ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_timeout_ms: Option<i64>,
    /// Whether a `working_dir` parameter was provided. Populated by `edit_overwrite`,
    /// `edit_replace`, and `exec_command`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub working_dir_used: bool,
}

/// Fluent builder for MetricEvent. Reduces repetitive struct literal boilerplate.
#[derive(Debug, Default)]
pub(crate) struct MetricEventBuilder {
    ts: u64,
    tool: &'static str,
    duration_ms: u64,
    output_chars: usize,
    param_path_depth: usize,
    max_depth: Option<u32>,
    result: &'static str,
    error_type: Option<String>,
    error_subtype: Option<String>,
    session_id: Option<String>,
    seq: Option<u32>,
    cache_hit: Option<bool>,
    cache_write_failure: Option<bool>,
    cache_tier: Option<&'static str>,
    exit_code: Option<i32>,
    timed_out: bool,
    output_truncated: Option<bool>,
    chars_threshold_breach: bool,
    file_ext: Option<&'static str>,
    filter_applied: Option<String>,
    language: Option<String>,
    git_ref_used: bool,
    summary_mode: bool,
    is_paginated: bool,
    fields_projected: bool,
    match_mode: Option<String>,
    follow_depth: Option<u32>,
    import_lookup: bool,
    def_use: bool,
    impl_only: bool,
    stdin_provided: bool,
    timeout_configured_ms: Option<i64>,
    drain_timeout_ms: Option<i64>,
    working_dir_used: bool,
}

#[allow(clippy::too_many_arguments)]
impl MetricEventBuilder {
    #[must_use]
    pub(crate) fn new(tool: &'static str, result: &'static str, duration_ms: u64) -> Self {
        Self {
            ts: unix_ms(),
            tool,
            result,
            duration_ms,
            ..Self::default()
        }
    }

    #[must_use]
    pub(crate) fn output_chars(mut self, v: usize) -> Self {
        self.output_chars = v;
        self
    }
    #[must_use]
    pub(crate) fn param_path_depth(mut self, v: usize) -> Self {
        self.param_path_depth = v;
        self
    }
    #[must_use]
    pub(crate) fn max_depth(mut self, v: Option<u32>) -> Self {
        self.max_depth = v;
        self
    }
    #[must_use]
    pub(crate) fn error_type(mut self, v: Option<String>) -> Self {
        self.error_type = v;
        self
    }
    #[must_use]
    pub(crate) fn error_subtype(mut self, v: Option<String>) -> Self {
        self.error_subtype = v;
        self
    }
    #[must_use]
    pub(crate) fn session_id(mut self, v: Option<String>) -> Self {
        self.session_id = v;
        self
    }
    #[must_use]
    pub(crate) fn seq(mut self, v: Option<u32>) -> Self {
        self.seq = v;
        self
    }
    #[must_use]
    pub(crate) fn cache_hit(mut self, v: Option<bool>) -> Self {
        self.cache_hit = v;
        self
    }
    #[must_use]
    pub(crate) fn cache_tier(mut self, v: Option<&'static str>) -> Self {
        self.cache_tier = v;
        self
    }
    #[must_use]
    pub(crate) fn cache_write_failure(mut self, v: Option<bool>) -> Self {
        self.cache_write_failure = v;
        self
    }
    #[must_use]
    pub(crate) fn exit_code(mut self, v: Option<i32>) -> Self {
        self.exit_code = v;
        self
    }
    #[must_use]
    pub(crate) fn timed_out(mut self, v: bool) -> Self {
        self.timed_out = v;
        self
    }
    #[must_use]
    pub(crate) fn output_truncated(mut self, v: Option<bool>) -> Self {
        self.output_truncated = v;
        self
    }
    #[must_use]
    pub(crate) fn chars_threshold_breach(mut self, v: bool) -> Self {
        self.chars_threshold_breach = v;
        self
    }
    #[must_use]
    pub(crate) fn file_ext(mut self, v: Option<&'static str>) -> Self {
        self.file_ext = v;
        self
    }
    #[must_use]
    pub(crate) fn filter_applied(mut self, v: Option<String>) -> Self {
        self.filter_applied = v;
        self
    }
    #[must_use]
    pub(crate) fn language(mut self, v: Option<String>) -> Self {
        self.language = v;
        self
    }
    #[must_use]
    pub(crate) fn git_ref_used(mut self, v: bool) -> Self {
        self.git_ref_used = v;
        self
    }
    #[must_use]
    pub(crate) fn summary_mode(mut self, v: bool) -> Self {
        self.summary_mode = v;
        self
    }
    #[must_use]
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn is_paginated(mut self, v: bool) -> Self {
        self.is_paginated = v;
        self
    }
    #[must_use]
    pub(crate) fn fields_projected(mut self, v: bool) -> Self {
        self.fields_projected = v;
        self
    }
    #[must_use]
    pub(crate) fn match_mode(mut self, v: Option<String>) -> Self {
        self.match_mode = v;
        self
    }
    #[must_use]
    pub(crate) fn follow_depth(mut self, v: Option<u32>) -> Self {
        self.follow_depth = v;
        self
    }
    #[must_use]
    pub(crate) fn import_lookup(mut self, v: bool) -> Self {
        self.import_lookup = v;
        self
    }
    #[must_use]
    pub(crate) fn def_use(mut self, v: bool) -> Self {
        self.def_use = v;
        self
    }
    #[must_use]
    pub(crate) fn impl_only(mut self, v: bool) -> Self {
        self.impl_only = v;
        self
    }
    #[must_use]
    pub(crate) fn stdin_provided(mut self, v: bool) -> Self {
        self.stdin_provided = v;
        self
    }
    #[must_use]
    pub(crate) fn timeout_configured_ms(mut self, v: Option<i64>) -> Self {
        self.timeout_configured_ms = v;
        self
    }
    #[must_use]
    pub(crate) fn drain_timeout_ms(mut self, v: Option<i64>) -> Self {
        self.drain_timeout_ms = v;
        self
    }
    #[must_use]
    pub(crate) fn working_dir_used(mut self, v: bool) -> Self {
        self.working_dir_used = v;
        self
    }
    #[must_use]
    pub(crate) fn build(self) -> MetricEvent {
        MetricEvent {
            ts: self.ts,
            tool: self.tool,
            duration_ms: self.duration_ms,
            output_chars: self.output_chars,
            param_path_depth: self.param_path_depth,
            max_depth: self.max_depth,
            result: self.result,
            error_type: self.error_type,
            error_subtype: self.error_subtype,
            session_id: self.session_id,
            seq: self.seq,
            cache_hit: self.cache_hit,
            cache_write_failure: self.cache_write_failure,
            cache_tier: self.cache_tier,
            exit_code: self.exit_code,
            timed_out: self.timed_out,
            output_truncated: self.output_truncated,
            chars_threshold_breach: self.chars_threshold_breach,
            file_ext: self.file_ext,
            filter_applied: self.filter_applied,
            language: self.language,
            git_ref_used: self.git_ref_used,
            summary_mode: self.summary_mode,
            is_paginated: self.is_paginated,
            fields_projected: self.fields_projected,
            match_mode: self.match_mode,
            follow_depth: self.follow_depth,
            import_lookup: self.import_lookup,
            def_use: self.def_use,
            impl_only: self.impl_only,
            stdin_provided: self.stdin_provided,
            timeout_configured_ms: self.timeout_configured_ms,
            drain_timeout_ms: self.drain_timeout_ms,
            working_dir_used: self.working_dir_used,
        }
    }
}

/// Sender half of the metrics channel; cloned and passed to tools for event emission.
#[derive(Clone)]
pub struct MetricsSender(pub tokio::sync::mpsc::UnboundedSender<MetricEvent>);

impl MetricsSender {
    pub fn send(&self, event: MetricEvent) {
        let _ = self.0.send(event);
    }
}

/// Accumulated metrics for a single tool.
#[derive(Default, Debug)]
pub(crate) struct ToolMetrics {
    pub(crate) count: u64,
    pub(crate) duration_ms: u64,
    pub(crate) output_chars: u64,
}

/// RAII guard that releases an exclusive lock on a metrics .lock file when dropped.
/// Lock release happens implicitly when the underlying `std::fs::File` is closed.
#[allow(dead_code)]
pub(crate) struct MetricsLockGuard(pub(crate) std::fs::File);

/// Record a metric event to OTel metrics if the global meter provider is available.
///
/// Records:
/// - Histogram: mcp.server.operation.duration (in milliseconds)
/// - Counter: mcp.server.tool.calls (incremented by 1)
///
/// Labels: gen_ai.tool.name, error.type (or "none" if no error)
///
/// Instruments are initialized once via OnceLock to avoid rebuilding them on every call.
pub(crate) fn record_otel_metrics(event: &MetricEvent) {
    // Skip OTEL recording for "received" events (duration_ms=0 would pollute latency histograms)
    if event.result == "received" {
        return;
    }

    static DURATION_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
    static CALL_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
    static CACHE_HITS_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
    static CACHE_WRITE_FAILURES_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();

    let histogram = DURATION_HISTOGRAM.get_or_init(|| {
        global::meter("aptu-coder")
            .f64_histogram("mcp.server.operation.duration")
            .with_unit("s")
            .with_boundaries(vec![
                0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
            ])
            .build()
    });

    let counter = CALL_COUNTER.get_or_init(|| {
        global::meter("aptu-coder")
            .u64_counter("mcp.server.tool.calls")
            .build()
    });

    let cache_hits_counter = CACHE_HITS_COUNTER.get_or_init(|| {
        global::meter("aptu-coder")
            .u64_counter("mcp.server.tool.cache_hits_total")
            .with_description("Number of tool responses served from cache (l1_memory or l2_disk)")
            .build()
    });

    let cache_write_failures_counter = CACHE_WRITE_FAILURES_COUNTER.get_or_init(|| {
        global::meter("aptu-coder")
            .u64_counter("mcp.server.tool.cache_write_failures_total")
            .with_description(
                "Number of L2 disk cache write failures (dir, tempfile, write, rename)",
            )
            .build()
    });

    let error_type = event.error_type.as_deref().unwrap_or("success");
    let attributes = [
        KeyValue::new("gen_ai.tool.name", event.tool),
        KeyValue::new("error.type", error_type.to_string()),
        KeyValue::new("mcp.method.name", "tools/call"),
        KeyValue::new("mcp.protocol.version", "2025-11-25"),
        KeyValue::new("network.transport", "pipe"),
    ];

    histogram.record(event.duration_ms as f64 / 1000.0, &attributes);
    counter.add(1, &attributes);

    if event.cache_hit == Some(true) {
        let tier = event.cache_tier.unwrap_or("unknown");
        cache_hits_counter.add(
            1,
            &[
                KeyValue::new("gen_ai.tool.name", event.tool),
                KeyValue::new("cache_tier", tier),
            ],
        );
    }

    if event.cache_write_failure == Some(true) {
        cache_write_failures_counter.add(1, &[KeyValue::new("gen_ai.tool.name", event.tool)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metric_event_serialization() {
        let event = MetricEvent {
            ts: 1_700_000_000_000,
            tool: "analyze_directory",
            duration_ms: 100,
            output_chars: 500,
            param_path_depth: 1,
            max_depth: None,
            result: "ok",
            error_type: None,
            error_subtype: None,
            session_id: Some("1742468880123-42".to_string()),
            seq: Some(5),
            cache_hit: None,
            cache_write_failure: None,
            cache_tier: None,
            exit_code: Some(0),
            timed_out: false,
            output_truncated: None,
            chars_threshold_breach: false,
            file_ext: None,
            ..Default::default()
        };
        let serialized = serde_json::to_string(&event).unwrap();
        assert!(serialized.contains(r#""ts":1700000000000"#));
        assert!(serialized.contains(r#""tool":"analyze_directory""#));
        assert!(serialized.contains(r#""session_id":"1742468880123-42""#));
        assert!(serialized.contains(r#""exit_code":0"#));
    }

    #[test]
    fn test_metric_event_serialization_error() {
        let event = MetricEvent {
            ts: 1_700_000_000_000,
            tool: "edit_replace",
            duration_ms: 10,
            output_chars: 0,
            param_path_depth: 2,
            max_depth: None,
            result: "error",
            error_type: Some("invalid_params".to_string()),
            session_id: None,
            seq: None,
            cache_hit: None,
            cache_write_failure: None,
            exit_code: None,
            timed_out: false,
            cache_tier: None,
            output_truncated: None,
            chars_threshold_breach: false,
            file_ext: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""error_type":"invalid_params""#));
    }

    #[test]
    fn test_metric_event_error_subtype_some_serializes() {
        let event = MetricEvent {
            ts: 1_700_000_000_000,
            tool: "edit_replace",
            duration_ms: 10,
            output_chars: 0,
            param_path_depth: 2,
            max_depth: None,
            result: "error",
            error_type: Some("invalid_params".to_string()),
            error_subtype: Some("not_found".to_string()),
            session_id: None,
            seq: None,
            cache_hit: None,
            cache_write_failure: None,
            exit_code: None,
            timed_out: false,
            cache_tier: None,
            output_truncated: None,
            chars_threshold_breach: false,
            file_ext: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""error_subtype":"not_found""#));
    }

    #[test]
    fn test_metric_event_error_subtype_ambiguous() {
        let event = MetricEvent {
            ts: 1_700_000_000_000,
            tool: "edit_replace",
            duration_ms: 10,
            output_chars: 0,
            param_path_depth: 2,
            max_depth: None,
            result: "error",
            error_type: Some("invalid_params".to_string()),
            error_subtype: Some("ambiguous".to_string()),
            session_id: None,
            seq: None,
            cache_hit: None,
            cache_write_failure: None,
            exit_code: None,
            timed_out: false,
            cache_tier: None,
            output_truncated: None,
            chars_threshold_breach: false,
            file_ext: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""error_subtype":"ambiguous""#));
    }

    #[test]
    fn test_metric_event_new_fields_round_trip() {
        let event = MetricEvent {
            ts: 1_700_000_000_000,
            tool: "analyze_file",
            duration_ms: 100,
            output_chars: 500,
            param_path_depth: 2,
            max_depth: Some(3),
            result: "ok",
            error_type: None,
            error_subtype: None,
            session_id: Some("1742468880123-42".to_string()),
            seq: Some(5),
            cache_hit: None,
            cache_write_failure: None,
            exit_code: None,
            timed_out: false,
            cache_tier: None,
            output_truncated: None,
            chars_threshold_breach: false,
            file_ext: None,
            filter_applied: None,
            language: None,
            git_ref_used: false,
            summary_mode: false,
            is_paginated: false,
            fields_projected: false,
            match_mode: None,
            follow_depth: None,
            import_lookup: false,
            def_use: false,
            impl_only: false,
            stdin_provided: false,
            timeout_configured_ms: None,
            drain_timeout_ms: None,
            working_dir_used: false,
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let json_str = r#"{"ts":1700000000000,"tool":"analyze_file","duration_ms":100,"output_chars":500,"param_path_depth":2,"max_depth":3,"result":"ok","session_id":"1742468880123-42","seq":5}"#;
        assert_eq!(serialized, json_str);
    }
}
