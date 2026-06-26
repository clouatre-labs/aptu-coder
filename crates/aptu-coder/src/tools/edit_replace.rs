// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Extracted handler for the `edit_replace` MCP tool.
//!
//! See `tools/mod.rs` for the extraction pattern rules.

use aptu_coder_core::types::{EditReplaceOutput, EditReplaceParams};
use rmcp::model::{CallToolResult, Content, ErrorData};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::instrument;

use crate::tools::EditHandlerContext;
use crate::tools::common::{err_to_tool_result, error_meta, no_cache_meta};
use crate::validation::{validate_path, validate_path_relative_to};

/// Number of consecutive not_found or ambiguous edit_replace failures on the same
/// (session_id, canonical_path) pair before returning a stale-context directive error.
pub(crate) const EDIT_STALE_THRESHOLD: u8 = 5;

/// Maximum number of (session_id, canonical_path) entries in the failure counter map.
/// When the map reaches this size, it is cleared entirely to prevent unbounded growth.
/// The circuit breaker is advisory, so a full clear is safe: the worst case is one
/// missed trip per session per path after an eviction cycle.
pub(crate) const EDIT_FAILURE_MAP_CAP: usize = 1024;

/// Per-session circuit breaker for consecutive edit_replace failures on a single path.
///
/// ## Invariants
///
/// - Tracks consecutive `not_found` or `ambiguous` failures per `(session_id, canonical_path)`.
/// - When the map reaches `EDIT_FAILURE_MAP_CAP` entries it is cleared entirely; the circuit
///   breaker is advisory so a full eviction is safe (worst case: one missed trip per session
///   per path after an eviction).
/// - A successful edit resets the counter for that `(session_id, canonical_path)` pair via
///   [`StaleContextGuard::reset`].
/// - Owns an `Arc` clone of the shared map and a copy of the session ID string so it can be
///   constructed before `ctx` is partially moved into closures or async blocks.
struct StaleContextGuard {
    sid: String,
    counts: Arc<Mutex<HashMap<(String, String), u8>>>,
}

impl StaleContextGuard {
    fn new(sid: Option<String>, counts: Arc<Mutex<HashMap<(String, String), u8>>>) -> Self {
        Self {
            sid: sid.unwrap_or_default(),
            counts,
        }
    }

    /// Increments the failure counter for `canonical` and returns `true` when the
    /// circuit breaker threshold has been reached.
    fn increment(&mut self, canonical: &str) -> bool {
        let mut counts = self.counts.lock().expect("edit_failure_counts poisoned");
        if counts.len() >= EDIT_FAILURE_MAP_CAP {
            counts.clear();
        }
        let entry = counts
            .entry((self.sid.clone(), canonical.to_owned()))
            .or_insert(0);
        *entry = entry.saturating_add(1);
        *entry >= EDIT_STALE_THRESHOLD
    }

    /// Resets the failure counter for `canonical` after a successful edit.
    fn reset(&mut self, canonical: &str) {
        let mut counts = self.counts.lock().expect("edit_failure_counts poisoned");
        counts.remove(&(self.sid.clone(), canonical.to_owned()));
    }
}

/// Resolves and validates the target path for an edit operation.
///
/// Returns `Ok(PathBuf)` on success, or `Err(CallToolResult)` when path validation
/// fails. Directory-path errors are detected here and returned as `Err`.
fn resolve_edit_path(
    path: &str,
    working_dir: Option<&str>,
    span: &tracing::Span,
) -> Result<PathBuf, CallToolResult> {
    let resolved = if let Some(wd) = working_dir {
        match validate_path_relative_to(path, true, std::path::Path::new(wd)) {
            Ok(p) => p,
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                let mut result = CallToolResult::error(vec![Content::text(
                    "working_dir is not valid; provide an existing directory path".to_string(),
                )])
                .with_meta(Some(no_cache_meta()));
                result.structured_content = Some(serde_json::json!({
                    "workingDir": wd,
                    "error": e.message,
                }));
                return Err(result);
            }
        }
    } else {
        match validate_path(path, true) {
            Ok(p) => p,
            Err(e) => {
                span.record("error", true);
                span.record("error.type", "invalid_params");
                return Err(err_to_tool_result(e));
            }
        }
    };

    if std::fs::metadata(&resolved)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        return Err(err_to_tool_result(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is a directory; cannot edit a directory".to_string(),
            Some(error_meta(
                "validation",
                false,
                "provide a file path, not a directory",
            )),
        )));
    }

    Ok(resolved)
}

/// Sends an error metric for the `edit_replace` tool.
fn send_replace_error_metric(
    ctx: &EditHandlerContext<'_>,
    t_start: std::time::Instant,
    param_path: &str,
    error_type: &str,
) {
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
            .param_path_depth(crate::metrics::path_component_count(param_path))
            .error_type(Some(error_type.to_string()))
            .session_id(ctx.sid.clone())
            .seq(Some(ctx.seq))
            .build(),
    );
}

/// Builds the diagnostic hint message for a `not_found` failure.
///
/// Returns a short message when `first_20_lines` is empty, or a longer message with
/// the nearest-matching line from the file when it has content.
fn build_not_found_message(first_20_lines: &str, old_text_for_hint: &str) -> String {
    if first_20_lines.is_empty() {
        return "old_text not found (0 matches). Re-read the file with analyze_file or analyze_module to obtain the current content, then derive old_text from the live file before retrying.".to_string();
    }

    let first_old_line = old_text_for_hint.lines().next().unwrap_or("");
    let mut best_line_idx = 1usize;
    let mut best_line = "";
    let mut best_lcp = 0usize;

    for (i, file_line) in first_20_lines.lines().enumerate() {
        let lcp = file_line
            .chars()
            .zip(first_old_line.chars())
            .take_while(|(a, b)| a == b)
            .count();
        if lcp > best_lcp {
            best_lcp = lcp;
            best_line = file_line;
            best_line_idx = i + 1;
        }
    }

    let numbered_lines: String = first_20_lines
        .lines()
        .enumerate()
        .map(|(i, line)| format!("  Line {}: {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "old_text not found (0 matches).\nThe file begins:\n{numbered_lines}\n\nNearest match: line {best_line_idx} contains \"{best_line}\" which shares {best_lcp} characters with the start of old_text.\nRe-read the file with analyze_file or analyze_module to obtain the current content, then derive old_text from the live file before retrying."
    )
}

/// Returns the stale-context directive error message string.
///
/// Used in both `not_found` and `ambiguous` circuit-breaker trip paths to avoid
/// duplicating the format string.
fn stale_context_error_msg(threshold: u8, param_path: &str) -> String {
    format!(
        "EDIT_STALE_CONTEXT: {} consecutive not_found/ambiguous failures on '{}' in this session. The file content has drifted from your context. Call analyze_file or analyze_module on this path first, then retry edit_replace with old_text taken verbatim from that response. Do not retry edit_replace on this path without re-reading first.",
        threshold, param_path,
    )
}

/// Converts an `aptu_coder_core::EditError` into a `CallToolResult` and sends the
/// appropriate metric event.
///
/// This keeps the `edit_replace` coordinator function short by moving the per-error-variant
/// span recording, metric emission, and error response construction into one place.
fn handle_edit_error(
    err: aptu_coder_core::EditError,
    span: &tracing::Span,
    t_start: std::time::Instant,
    param_path: &str,
    old_text_for_hint: &str,
    guard: &mut StaleContextGuard,
    ctx: &EditHandlerContext<'_>,
) -> CallToolResult {
    span.record("error", true);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    match err {
        aptu_coder_core::EditError::NotFound {
            path: notfound_path,
            first_20_lines,
        } => {
            span.record("error.type", "invalid_params");
            let tripped = guard.increment(&notfound_path);
            if tripped {
                ctx.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(param_path))
                        .error_type(Some("invalid_params".to_string()))
                        .error_subtype(Some("stale_context".to_string()))
                        .session_id(ctx.sid.clone())
                        .seq(Some(ctx.seq))
                        .build(),
                );
                return err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    stale_context_error_msg(EDIT_STALE_THRESHOLD, param_path),
                    Some(error_meta(
                        "validation",
                        false,
                        "re-read the file with analyze_file or analyze_module, then retry with old_text from the live content",
                    )),
                ));
            }
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .error_subtype(Some("not_found".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
                    .build(),
            );
            let message = build_not_found_message(&first_20_lines, old_text_for_hint);
            let mut meta = error_meta(
                "validation",
                false,
                "re-read the file with analyze_file or analyze_module, then derive old_text from the live content",
            );
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("path".to_string(), serde_json::json!(notfound_path));
            }
            err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                message,
                Some(meta),
            ))
        }
        aptu_coder_core::EditError::Ambiguous {
            count,
            path: ambiguous_path,
            match_lines,
        } => {
            span.record("error.type", "invalid_params");
            let tripped = guard.increment(&ambiguous_path);
            if tripped {
                ctx.metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                        .param_path_depth(crate::metrics::path_component_count(param_path))
                        .error_type(Some("invalid_params".to_string()))
                        .error_subtype(Some("stale_context".to_string()))
                        .session_id(ctx.sid.clone())
                        .seq(Some(ctx.seq))
                        .build(),
                );
                return err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    stale_context_error_msg(EDIT_STALE_THRESHOLD, param_path),
                    Some(error_meta(
                        "validation",
                        false,
                        "re-read the file with analyze_file or analyze_module, then retry with old_text from the live content",
                    )),
                ));
            }
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .error_subtype(Some("ambiguous".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
                    .build(),
            );
            let line_numbers_csv = match_lines
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            let mut meta = error_meta(
                "validation",
                false,
                "extend old_text with more surrounding context, or re-read with analyze_file to confirm the exact text",
            );
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("path".to_string(), serde_json::json!(ambiguous_path));
            }
            err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                format!(
                    "old_text matched {count} locations.\nOccurrences at lines: {line_numbers_csv}\nExtend old_text with more surrounding context to make it unique, or re-read with analyze_file to confirm the exact text."
                ),
                Some(meta),
            ))
        }
        aptu_coder_core::EditError::NotAFile(_) => {
            span.record("error.type", "invalid_params");
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(param_path))
                    .error_type(Some("invalid_params".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
                    .build(),
            );
            err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "path is a directory".to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "provide a file path, not a directory",
                )),
            ))
        }
        aptu_coder_core::EditError::Io(io_err) => {
            span.record("error.type", "internal_error");
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(param_path))
                    .error_type(Some("internal_error".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
                    .build(),
            );
            let mut meta = error_meta("resource", false, "check file path and permissions");
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("path".to_string(), serde_json::json!(param_path));
                obj.insert(
                    "ioErrorKind".to_string(),
                    serde_json::json!(format!("{:?}", io_err.kind())),
                );
                obj.insert(
                    "ioErrorSource".to_string(),
                    serde_json::json!(io_err.to_string()),
                );
            }
            err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                "I/O error editing file; check file path and permissions".to_string(),
                Some(meta),
            ))
        }
        e => {
            span.record("error.type", "internal_error");
            ctx.metrics_tx.send(
                crate::metrics::MetricEventBuilder::new("edit_replace", "error", dur)
                    .param_path_depth(crate::metrics::path_component_count(param_path))
                    .error_type(Some("internal_error".to_string()))
                    .session_id(ctx.sid.clone())
                    .seq(Some(ctx.seq))
                    .build(),
            );
            err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                e.to_string(),
                Some(error_meta(
                    "resource",
                    false,
                    "check file path and permissions",
                )),
            ))
        }
    }
}

#[instrument(skip(params, ctx, span, t_start), fields(path = %params.path))]
pub(crate) async fn edit_replace(
    params: EditReplaceParams,
    ctx: EditHandlerContext<'_>,
    span: &tracing::Span,
    t_start: std::time::Instant,
) -> Result<CallToolResult, ErrorData> {
    span.record("gen_ai.tool.name", "edit_replace");
    span.record("path", &params.path);

    let param_path = params.path.clone();
    let resolved_path = match resolve_edit_path(&param_path, params.working_dir.as_deref(), span) {
        Ok(p) => p,
        Err(result) => {
            send_replace_error_metric(&ctx, t_start, &param_path, "invalid_params");
            return Ok(result);
        }
    };
    let old_text = params.old_text.clone();
    let new_text = params.new_text.clone();
    let old_text_for_hint = old_text.clone();
    let handle = tokio::task::spawn_blocking(move || {
        aptu_coder_core::edit_replace_block(&resolved_path, &old_text, &new_text)
    });

    let mut guard = StaleContextGuard::new(ctx.sid.clone(), Arc::clone(ctx.edit_failure_counts));

    let output: EditReplaceOutput = match handle.await {
        Ok(Ok(v)) => v,
        Ok(Err(edit_err)) => {
            return Ok(handle_edit_error(
                edit_err,
                span,
                t_start,
                &param_path,
                &old_text_for_hint,
                &mut guard,
                &ctx,
            ));
        }
        Err(e) => {
            span.record("error", true);
            span.record("error.type", "internal_error");
            send_replace_error_metric(&ctx, t_start, &param_path, "internal_error");
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                e.to_string(),
                Some(error_meta(
                    "resource",
                    false,
                    "check file path and permissions",
                )),
            )));
        }
    };

    let text = format!(
        "Edited {}: {} bytes -> {} bytes",
        output.path, output.bytes_before, output.bytes_after
    );
    let mut result =
        CallToolResult::success(vec![Content::text(text.clone())]).with_meta(Some(no_cache_meta()));
    let structured = match serde_json::to_value(&output).map_err(|e| {
        ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            format!("serialization failed: {e}"),
            Some(error_meta("internal", false, "report this as a bug")),
        )
    }) {
        Ok(v) => v,
        Err(e) => return Ok(err_to_tool_result(e)),
    };
    result.structured_content = Some(structured);
    ctx.cache
        .invalidate_file(&std::path::PathBuf::from(&param_path));
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    guard.reset(&output.path);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("edit_replace", "ok", dur)
            .output_chars(text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .session_id(ctx.sid)
            .seq(Some(ctx.seq))
            .build(),
    );
    Ok(result)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[test]
    fn build_not_found_message_empty_file() {
        let msg = build_not_found_message("", "fn foo() {}");
        assert!(msg.contains("0 matches"));
        assert!(msg.contains("analyze_file or analyze_module"));
        assert!(!msg.contains("The file begins"));
    }

    #[test]
    fn build_not_found_message_with_content() {
        let file_content = "fn foo() {\n    let x = 1;\n}\n";
        let old_text = "fn foo()";
        let msg = build_not_found_message(file_content, old_text);
        assert!(msg.contains("The file begins"));
        assert!(msg.contains("Nearest match"));
        assert!(msg.contains("fn foo()"));
    }

    #[test]
    fn stale_context_guard_below_threshold_returns_false() {
        let counts: Arc<Mutex<HashMap<(String, String), u8>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let mut guard = StaleContextGuard::new(Some("session1".to_string()), Arc::clone(&counts));
        for _ in 0..(EDIT_STALE_THRESHOLD - 1) {
            let tripped = guard.increment("some/path.rs");
            assert!(!tripped);
        }
    }

    #[test]
    fn stale_context_guard_at_threshold_trips() {
        let counts: Arc<Mutex<HashMap<(String, String), u8>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let mut guard = StaleContextGuard::new(Some("session1".to_string()), Arc::clone(&counts));
        let mut tripped = false;
        for _ in 0..EDIT_STALE_THRESHOLD {
            tripped = guard.increment("some/path.rs");
        }
        assert!(tripped);
    }

    #[test]
    fn stale_context_guard_clears_at_capacity() {
        let counts: Arc<Mutex<HashMap<(String, String), u8>>> =
            Arc::new(Mutex::new(HashMap::new()));
        {
            let mut c = counts.lock().unwrap();
            for i in 0..EDIT_FAILURE_MAP_CAP {
                c.insert((format!("sid{i}"), format!("path{i}")), 1);
            }
        }
        let mut guard = StaleContextGuard::new(Some("newsid".to_string()), Arc::clone(&counts));
        let tripped = guard.increment("new/path.rs");
        assert!(!tripped);
        let c = counts.lock().unwrap();
        assert_eq!(c.len(), 1);
    }
}
