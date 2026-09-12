//! Extracted handler logic for the `analyze_symbol` MCP tool.
//!
//! The `#[tool(...)]`-decorated method and `#[instrument]` outer decorator
//! remain in `lib.rs` as a thin shim. This module contains the free functions
//! that implement the actual logic, following the extraction pattern documented
//! in `tools/mod.rs`.

use aptu_coder_core::analyze;
use aptu_coder_core::cache::{CacheTier, CallGraphCache, StructuralGraphCache};
use aptu_coder_core::pagination::{
    CursorData, DEFAULT_PAGE_SIZE, PaginationMode, decode_cursor, encode_cursor,
};
use aptu_coder_core::schema_helpers::MAX_FOLLOW_DEPTH;
use aptu_coder_core::traversal::{
    WalkEntry, changed_files_from_git_ref, filter_entries_by_git_ref, walk_directory,
};
use aptu_coder_core::types::{AnalyzeSymbolParams, SymbolMatchMode};
use rmcp::model::{Annotations, CallToolResult, ContentBlock, ErrorData, MetaObject, TextContent};
use serde_json::Value;
use std::sync::Arc;
use tracing::instrument;

use crate::tools::common::{
    err_to_tool_result, error_meta, no_cache_meta, normalize_cursor, summary_cursor_conflict,
};

use crate::tools::symbol_focused::{apply_call_graph_pagination, handle_focused_mode};

/// Shared handler context passed to extracted `analyze_symbol` free functions.
///
/// Bundles the `CodeAnalyzer` fields needed by the handler, keeping them
/// explicit without coupling to `&self`.
pub(crate) struct AnalyzeSymbolContext {
    pub(crate) metrics_tx: crate::metrics::MetricsSender,
    pub(crate) call_graph_cache: CallGraphCache,
    pub(crate) structural_graph_cache: StructuralGraphCache,
    pub(crate) disk_cache: Arc<aptu_coder_core::cache::DiskCache>,
    pub(crate) sid: Option<String>,
    pub(crate) seq: u32,
}

/// Internal parameters for a focused analysis task.
#[derive(Clone)]
pub(crate) struct FocusedAnalysisParams {
    pub(crate) path: std::path::PathBuf,
    pub(crate) symbol: String,
    pub(crate) match_mode: SymbolMatchMode,
    pub(crate) follow_depth: u32,
    pub(crate) max_depth: Option<u32>,
    pub(crate) impl_only: Option<bool>,
    pub(crate) def_use: bool,
    pub(crate) parse_timeout_micros: Option<u64>,
}

/// Machine-readable cause for an `analyze_symbol` `invalid_params` error, used to
/// populate `MetricEvent::error_subtype` for per-cause metrics dashboards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnalyzeSymbolErrorSubtype {
    /// `path` argument points to a file instead of a directory.
    PathIsFile,
    /// `summary=true` combined with a pagination `cursor`.
    SummaryCursorConflict,
    /// `import_lookup=true` combined with `def_use=true`.
    ImportLookupDefUseConflict,
    /// `import_lookup=true` without a non-empty `symbol`.
    ImportLookupMissingSymbol,
    /// `follow_depth` exceeds `MAX_FOLLOW_DEPTH`.
    FollowDepthExceeded,
    /// The pagination `cursor` failed to decode.
    InvalidCursor,
    /// `git_ref` filtering failed (not a git repo, git unavailable, etc.).
    GitRefFilterFailed,
    /// Call-graph pagination rejected the requested cursor/offset.
    PaginationInvalid,
    /// `impl_only=true` on a directory containing no Rust source files.
    ImplOnlyRequiresRust,
}

impl AnalyzeSymbolErrorSubtype {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PathIsFile => "path_is_file",
            Self::SummaryCursorConflict => "summary_cursor_conflict",
            Self::ImportLookupDefUseConflict => "import_lookup_def_use_conflict",
            Self::ImportLookupMissingSymbol => "import_lookup_missing_symbol",
            Self::FollowDepthExceeded => "follow_depth_exceeded",
            Self::InvalidCursor => "invalid_cursor",
            Self::GitRefFilterFailed => "git_ref_filter_failed",
            Self::PaginationInvalid => "pagination_invalid",
            Self::ImplOnlyRequiresRust => "impl_only_requires_rust",
        }
    }
}

impl std::fmt::Display for AnalyzeSymbolErrorSubtype {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Helper function to emit error metrics for analyze_symbol.
/// Extracts the error_type string from ErrorCode and records it on the span.
pub(crate) fn emit_error_metric(
    ctx: &AnalyzeSymbolContext,
    error_type: &str,
    error_subtype: Option<AnalyzeSymbolErrorSubtype>,
    t_start: std::time::Instant,
    param_path_depth: Option<usize>,
) {
    let dur = t_start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    tracing::Span::current().record("error", true);
    tracing::Span::current().record("error.type", error_type);
    let mut builder = crate::metrics::MetricEventBuilder::new("analyze_symbol", "error", dur)
        .error_type(Some(error_type.to_string()))
        .error_subtype(error_subtype.map(|s| s.to_string()))
        .session_id(ctx.sid.clone())
        .seq(Some(ctx.seq));
    if let Some(depth) = param_path_depth {
        builder = builder.param_path_depth(depth);
    }
    ctx.metrics_tx.send(builder.build());
}

/// Emit an invalid_params error metric and return the corresponding `ErrorData`.
pub(crate) fn err_invalid_params(
    ctx: &AnalyzeSymbolContext,
    t_start: std::time::Instant,
    message: String,
    hint: &'static str,
) -> ErrorData {
    emit_error_metric(ctx, "invalid_params", None, t_start, None);
    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        message,
        Some(error_meta("validation", false, hint)),
    )
}

/// Validate that `impl_only=true` is only used with directories containing Rust source files.
pub(crate) fn validate_impl_only(entries: &[WalkEntry]) -> Result<(), ErrorData> {
    let has_rust = entries.iter().any(|e| {
        !e.is_dir
            && e.path
                .extension()
                .and_then(|x: &std::ffi::OsStr| x.to_str())
                == Some("rs")
    });

    if !has_rust {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "impl_only=true requires Rust source files. No .rs files found in the given path. Use analyze_symbol without impl_only for cross-language analysis.".to_string(),
            Some(error_meta(
                "validation",
                false,
                "remove impl_only or point to a directory containing .rs files",
            )),
        ));
    }
    Ok(())
}

/// Validate that `import_lookup=true` is accompanied by a non-empty symbol (the module path).
pub(crate) fn validate_import_lookup(
    import_lookup: Option<bool>,
    symbol: &str,
) -> Result<(), ErrorData> {
    if import_lookup == Some(true) && symbol.is_empty() {
        return Err(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "import_lookup=true requires symbol to contain the module path to search for"
                .to_string(),
            Some(error_meta(
                "validation",
                false,
                "set symbol to the module path when using import_lookup=true",
            )),
        ));
    }
    Ok(())
}

async fn handle_import_lookup(
    ctx: AnalyzeSymbolContext,
    params: AnalyzeSymbolParams,
    call: crate::tools::AnalyzeSymbolCall,
) -> Result<CallToolResult, ErrorData> {
    let sid = ctx.sid.clone();
    let seq = ctx.seq;
    let param_path = call.param_path;
    let max_depth_val = call.max_depth_val;
    let t_start = call.t_start;
    let cursor = normalize_cursor(params.pagination.cursor.as_deref());

    let path_owned = std::path::PathBuf::from(&params.path);
    let symbol = params.symbol.clone();
    let git_ref = params.git_ref.clone();
    let max_depth = params.max_depth;

    let handle = tokio::task::spawn_blocking(move || {
        let path = path_owned.as_path();
        let raw_entries = match walk_directory(path, max_depth) {
            Ok(e) => e,
            Err(e) => {
                return Err(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    format!("Failed to walk directory: {e}"),
                    Some(error_meta(
                        "resource",
                        false,
                        "check path permissions and availability",
                    )),
                ));
            }
        };
        // Apply git_ref filter when requested (non-empty string only).
        let entries = if let Some(ref git_ref_val) = git_ref
            && !git_ref_val.is_empty()
        {
            let changed = match changed_files_from_git_ref(path, git_ref_val) {
                Ok(c) => c,
                Err(e) => {
                    return Err(ErrorData::new(
                        rmcp::model::ErrorCode::INVALID_PARAMS,
                        format!("git_ref filter failed: {e}"),
                        Some(error_meta(
                            "resource",
                            false,
                            "ensure git is installed and path is inside a git repository",
                        )),
                    ));
                }
            };
            filter_entries_by_git_ref(raw_entries, &changed, path)
        } else {
            raw_entries
        };
        let output = match analyze::analyze_import_lookup(path, &symbol, &entries, None) {
            Ok(v) => v,
            Err(e) => {
                return Err(ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    format!("import_lookup failed: {e}"),
                    Some(error_meta(
                        "resource",
                        false,
                        "check path and file permissions",
                    )),
                ));
            }
        };
        Ok(output)
    });

    let output = match handle.await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            let error_type_str = match e.code {
                rmcp::model::ErrorCode::INVALID_PARAMS => "invalid_params",
                rmcp::model::ErrorCode::INTERNAL_ERROR => "internal_error",
                _ => "unknown",
            };
            // The only INVALID_PARAMS cause inside this spawn_blocking closure is a
            // failed git_ref filter; internal_error/unknown causes get no subtype.
            let error_subtype = (error_type_str == "invalid_params")
                .then_some(AnalyzeSymbolErrorSubtype::GitRefFilterFailed);
            emit_error_metric(
                &ctx,
                error_type_str,
                error_subtype,
                t_start,
                Some(crate::metrics::path_component_count(&param_path)),
            );
            return Ok(err_to_tool_result(e));
        }
        Err(e) => {
            emit_error_metric(
                &ctx,
                "internal_error",
                None,
                t_start,
                Some(crate::metrics::path_component_count(&param_path)),
            );
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("spawn_blocking failed: {e}"),
                Some(error_meta("resource", false, "internal error")),
            )));
        }
    };

    let final_text = output.formatted.clone();

    // Record cache tier in span
    tracing::Span::current().record("cache_tier", "Miss");

    // Add content_hash to _meta
    let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
    let mut meta = no_cache_meta().0;
    meta.insert(
        "content_hash".to_string(),
        serde_json::Value::String(content_hash),
    );

    let mut result = CallToolResult::success(vec![ContentBlock::Text(
        TextContent::new(final_text.clone())
            .with_annotations(Annotations::default().with_priority(0.9_f32)),
    )])
    .with_meta(Some(MetaObject(meta)));
    let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
    result.structured_content = Some(structured);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("analyze_symbol", "ok", dur)
            .output_chars(final_text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .max_depth(max_depth_val)
            .session_id(sid)
            .seq(Some(seq))
            .cache_hit(Some(false))
            .cache_tier(Some(CacheTier::Miss.as_str()))
            .match_mode(
                params
                    .match_mode
                    .as_ref()
                    .map(|m| format!("{:?}", m).to_lowercase()),
            )
            .follow_depth(params.follow_depth)
            .import_lookup(params.import_lookup.unwrap_or(false))
            .def_use(params.def_use.unwrap_or(false))
            .impl_only(params.impl_only.unwrap_or(false))
            .git_ref_used(params.git_ref.is_some())
            .is_paginated(cursor.is_some())
            .summary_mode(params.output_control.summary.unwrap_or(false))
            .build(),
    );
    Ok(result)
}

/// Decode the pagination cursor from `params` and return `(offset, cursor_mode)`.
///
/// Returns `Err(CallToolResult)` on a malformed cursor so the caller can
/// propagate the error immediately.
fn decode_call_graph_cursor(
    params: &AnalyzeSymbolParams,
) -> Result<(usize, PaginationMode), CallToolResult> {
    let cursor = normalize_cursor(params.pagination.cursor.as_deref());

    let cursor_mode = cursor
        .map(|s| {
            decode_cursor(s)
                .map(|c| c.mode)
                .unwrap_or(PaginationMode::Callers)
        })
        .unwrap_or(PaginationMode::Callers);

    let offset = if let Some(cursor_str) = cursor {
        decode_cursor(cursor_str)
            .map_err(|e| {
                err_to_tool_result(ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    e.to_string(),
                    Some(error_meta("validation", false, "invalid cursor format")),
                ))
            })?
            .offset
    } else {
        0
    };

    Ok((offset, cursor_mode))
}

/// Handle the call-graph mode (default) including def_use pagination.
///
/// The `def_use` flag is a parameter within the pagination logic, not a
/// separate top-level branch. It remains here inside the call-graph handler.
async fn handle_call_graph(
    ctx: AnalyzeSymbolContext,
    params: AnalyzeSymbolParams,
    call: crate::tools::AnalyzeSymbolCall,
) -> Result<CallToolResult, ErrorData> {
    let sid = ctx.sid.clone();
    let seq = ctx.seq;
    let ct = call.ct;
    let param_path = call.param_path;
    let max_depth_val = call.max_depth_val;
    let t_start = call.t_start;
    let cursor = normalize_cursor(params.pagination.cursor.as_deref());

    // Call handler for analysis and progress tracking
    let (graph_cache_tier, mut output) = match handle_focused_mode(&ctx, &params, ct, t_start).await
    {
        Ok(v) => v,
        Err(e) => {
            let error_type_str = match e.code {
                rmcp::model::ErrorCode::INVALID_PARAMS => "invalid_params",
                rmcp::model::ErrorCode::INTERNAL_ERROR => "internal_error",
                _ => "unknown",
            };
            // handle_focused_mode now emits its own subtype-tagged metric at each
            // invalid_params failure site (git_ref filter, impl_only, output size
            // limit); only non-invalid_params causes need to be recorded here.
            if error_type_str != "invalid_params" {
                emit_error_metric(
                    &ctx,
                    error_type_str,
                    None,
                    t_start,
                    Some(crate::metrics::path_component_count(&param_path)),
                );
            }
            return Ok(err_to_tool_result(e));
        }
    };

    // Surface cache tier in structuredContent for observability and testing.
    output.cache_tier = Some(graph_cache_tier.as_str().to_owned());

    let page_size = params.pagination.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    let (offset, cursor_mode) = match decode_call_graph_cursor(&params) {
        Ok(v) => v,
        Err(e) => {
            emit_error_metric(
                &ctx,
                "invalid_params",
                Some(AnalyzeSymbolErrorSubtype::InvalidCursor),
                t_start,
                Some(crate::metrics::path_component_count(&param_path)),
            );
            return Ok(e);
        }
    };

    let use_summary = params.output_control.summary == Some(true);

    let mut callee_cursor = match apply_call_graph_pagination(
        &mut output,
        &params,
        cursor_mode,
        offset,
        page_size,
        use_summary,
    ) {
        Ok(v) => v,
        Err(e) => {
            emit_error_metric(
                &ctx,
                "invalid_params",
                Some(AnalyzeSymbolErrorSubtype::PaginationInvalid),
                t_start,
                Some(crate::metrics::path_component_count(&param_path)),
            );
            return Ok(e);
        }
    };

    // When callers are exhausted and callees exist, bootstrap callee pagination
    // by emitting a {mode:callees, offset:0} cursor. This makes PaginationMode::Callees
    // reachable; without it the branch was dead code. Suppressed in summary mode
    // because summary and pagination are mutually exclusive.
    if callee_cursor.is_none()
        && cursor_mode == PaginationMode::Callers
        && !output.outgoing_chains.is_empty()
        && !use_summary
        && let Ok(cursor) = encode_cursor(&CursorData {
            mode: PaginationMode::Callees,
            offset: 0,
        })
    {
        callee_cursor = Some(cursor);
    }

    // When callees are exhausted and def_use_sites exist, bootstrap defuse cursor
    // by emitting a {mode:defuse, offset:0} cursor. This makes PaginationMode::DefUse
    // reachable. Suppressed in summary mode because summary and pagination are mutually exclusive.
    // Also bootstrap directly from Callers mode when there are no outgoing chains
    // (e.g. SymbolNotFound path or symbols with no callees) so def-use pagination
    // is reachable even without a Callees phase.
    if callee_cursor.is_none()
        && matches!(
            cursor_mode,
            PaginationMode::Callees | PaginationMode::Callers
        )
        && !output.def_use_sites.is_empty()
        && !use_summary
        && let Ok(cursor) = encode_cursor(&CursorData {
            mode: PaginationMode::DefUse,
            offset: 0,
        })
    {
        // Only bootstrap from Callers when callees are empty (otherwise
        // the Callees bootstrap above takes priority).
        if cursor_mode == PaginationMode::Callees || output.outgoing_chains.is_empty() {
            callee_cursor = Some(cursor);
        }
    }

    // Update next_cursor in output
    output.next_cursor.clone_from(&callee_cursor);

    // Build final text output with pagination cursor if present
    let mut final_text = output.formatted.clone();
    if let Some(cursor) = callee_cursor {
        final_text.push('\n');
        final_text.push_str("NEXT_CURSOR: ");
        final_text.push_str(&cursor);
    }

    // Record cache tier in span
    tracing::Span::current().record("cache_tier", graph_cache_tier.as_str());

    // Add content_hash to _meta
    let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
    let mut meta = no_cache_meta().0;
    meta.insert(
        "content_hash".to_string(),
        serde_json::Value::String(content_hash),
    );

    let mut result = CallToolResult::success(vec![ContentBlock::Text(
        TextContent::new(final_text.clone())
            .with_annotations(Annotations::default().with_priority(0.9_f32)),
    )])
    .with_meta(Some(MetaObject(meta)));
    // Only include def_use_sites in structuredContent when in DefUse mode.
    // In Callers/Callees modes, clearing the vec prevents large def-use
    // payloads from leaking into paginated non-def-use responses.
    if cursor_mode != PaginationMode::DefUse {
        output.def_use_sites = Vec::new();
    }
    let structured = serde_json::to_value(&output).unwrap_or(Value::Null);
    result.structured_content = Some(structured);
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    // Collect cache stats for metrics
    let (l1_eviction_count, (l2_entry_count, l2_size_bytes)) = (
        Some(ctx.call_graph_cache.eviction_count()),
        ctx.disk_cache.cache_stats(),
    );

    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("analyze_symbol", "ok", dur)
            .output_chars(final_text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .max_depth(max_depth_val)
            .session_id(sid)
            .seq(Some(seq))
            .cache_hit(Some(graph_cache_tier.is_hit()))
            .cache_tier(Some(graph_cache_tier.as_str()))
            .l1_eviction_count(l1_eviction_count)
            .l2_entry_count(Some(l2_entry_count))
            .l2_size_bytes(Some(l2_size_bytes))
            .match_mode(
                params
                    .match_mode
                    .as_ref()
                    .map(|m| format!("{:?}", m).to_lowercase()),
            )
            .follow_depth(params.follow_depth)
            .import_lookup(params.import_lookup.unwrap_or(false))
            .def_use(params.def_use.unwrap_or(false))
            .impl_only(params.impl_only.unwrap_or(false))
            .git_ref_used(params.git_ref.is_some())
            .is_paginated(cursor.is_some())
            .summary_mode(params.output_control.summary.unwrap_or(false))
            .build(),
    );
    Ok(result)
}

/// Emit an INVALID_PARAMS error, recording it on the span, and return early.
fn invalid_params(
    span: &tracing::Span,
    msg: impl Into<String>,
    hint: &'static str,
) -> Result<CallToolResult, ErrorData> {
    span.record("error", true);
    span.record("error.type", "invalid_params");
    Ok(err_to_tool_result(ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        msg.into(),
        Some(error_meta("validation", false, hint)),
    )))
}

/// Main handler for the `analyze_symbol` tool.
///
/// Validates common preconditions, then dispatches to `handle_import_lookup`
/// or `handle_call_graph` based on the `import_lookup` flag.
#[instrument(skip(ctx, params, call))]
pub(crate) async fn analyze_symbol_handler(
    ctx: AnalyzeSymbolContext,
    params: AnalyzeSymbolParams,
    call: crate::tools::AnalyzeSymbolCall,
) -> Result<CallToolResult, ErrorData> {
    let span = &call.span;
    let t_start = call.t_start;
    let cursor = normalize_cursor(params.pagination.cursor.as_deref());

    if std::path::Path::new(&params.path).is_file() {
        emit_error_metric(
            &ctx,
            "invalid_params",
            Some(AnalyzeSymbolErrorSubtype::PathIsFile),
            t_start,
            None,
        );
        return invalid_params(
            span,
            format!(
                "'{}' is a file; analyze_symbol requires a directory path",
                params.path
            ),
            "pass a directory path, not a file",
        );
    }

    if summary_cursor_conflict(params.output_control.summary, cursor) {
        emit_error_metric(
            &ctx,
            "invalid_params",
            Some(AnalyzeSymbolErrorSubtype::SummaryCursorConflict),
            t_start,
            None,
        );
        return invalid_params(
            span,
            "summary=true is incompatible with a pagination cursor; use one or the other",
            "remove cursor or set summary=false",
        );
    }

    if params.import_lookup == Some(true) && params.def_use == Some(true) {
        emit_error_metric(
            &ctx,
            "invalid_params",
            Some(AnalyzeSymbolErrorSubtype::ImportLookupDefUseConflict),
            t_start,
            None,
        );
        return invalid_params(
            span,
            "import_lookup=true and def_use=true are mutually exclusive; use one or the other",
            "remove import_lookup or set def_use=false",
        );
    }

    if let Err(e) = validate_import_lookup(params.import_lookup, &params.symbol) {
        emit_error_metric(
            &ctx,
            "invalid_params",
            Some(AnalyzeSymbolErrorSubtype::ImportLookupMissingSymbol),
            t_start,
            None,
        );
        return Ok(err_to_tool_result(e));
    }

    if let Some(depth) = params.follow_depth
        && depth > MAX_FOLLOW_DEPTH
    {
        emit_error_metric(
            &ctx,
            "invalid_params",
            Some(AnalyzeSymbolErrorSubtype::FollowDepthExceeded),
            t_start,
            None,
        );
        return invalid_params(
            span,
            format!("follow_depth={depth} exceeds the maximum of {MAX_FOLLOW_DEPTH}"),
            "reduce follow_depth to 3 or lower",
        );
    }

    if params.import_lookup == Some(true) {
        handle_import_lookup(ctx, params, call).await
    } else {
        handle_call_graph(ctx, params, call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aptu_coder_core::cache::{CallGraphCache, DiskCache, StructuralGraphCache};

    /// Builds a minimal `AnalyzeSymbolContext` backed by an unbounded metrics channel,
    /// returning the context and the receiving end so tests can inspect emitted events.
    fn test_context() -> (
        AnalyzeSymbolContext,
        tokio::sync::mpsc::UnboundedReceiver<crate::metrics::MetricEvent>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = AnalyzeSymbolContext {
            metrics_tx: crate::metrics::MetricsSender(tx),
            call_graph_cache: CallGraphCache::new(1),
            structural_graph_cache: StructuralGraphCache::new(1),
            disk_cache: Arc::new(DiskCache::new(std::path::PathBuf::new(), true)),
            sid: None,
            seq: 0,
        };
        (ctx, rx)
    }

    fn test_call(param_path: String) -> crate::tools::AnalyzeSymbolCall {
        crate::tools::AnalyzeSymbolCall {
            ct: tokio_util::sync::CancellationToken::new(),
            param_path,
            max_depth_val: None,
            span: tracing::Span::none(),
            t_start: std::time::Instant::now(),
        }
    }

    fn test_params(path: &str, summary: Option<bool>, cursor: Option<&str>) -> AnalyzeSymbolParams {
        serde_json::from_value(serde_json::json!({
            "path": path,
            "symbol": "",
            "summary": summary,
            "cursor": cursor,
        }))
        .expect("valid AnalyzeSymbolParams JSON")
    }

    #[tokio::test]
    async fn analyze_symbol_handler_path_is_file_sets_path_is_file_subtype() {
        // Arrange: path points to a real file rather than a directory.
        let (ctx, mut rx) = test_context();
        let file_path = format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
        let params = test_params(&file_path, None, None);
        let call = test_call(file_path.clone());

        // Act
        let _ = analyze_symbol_handler(ctx, params, call).await;

        // Assert
        let event = rx.try_recv().expect("expected an error metric event");
        assert_eq!(event.error_type.as_deref(), Some("invalid_params"));
        assert_eq!(event.error_subtype.as_deref(), Some("path_is_file"));
    }

    #[tokio::test]
    async fn analyze_symbol_handler_summary_and_cursor_sets_summary_cursor_conflict_subtype() {
        // Arrange: path does not exist (so the is_file check passes through), but
        // summary=true is combined with a pagination cursor.
        let (ctx, mut rx) = test_context();
        let path = "/definitely-nonexistent-dir-abcxyz123".to_string();
        let params = test_params(&path, Some(true), Some("some-cursor"));
        let call = test_call(path.clone());

        // Act
        let _ = analyze_symbol_handler(ctx, params, call).await;

        // Assert
        let event = rx.try_recv().expect("expected an error metric event");
        assert_eq!(event.error_type.as_deref(), Some("invalid_params"));
        assert_eq!(
            event.error_subtype.as_deref(),
            Some("summary_cursor_conflict")
        );
    }

    #[tokio::test]
    async fn analyze_symbol_handler_impl_only_without_rust_sets_impl_only_requires_rust_subtype() {
        // Arrange: directory contains no .rs files, so impl_only=true fails validation.
        let (ctx, mut rx) = test_context();
        let dir = tempfile::TempDir::new().expect("create temp dir");
        std::fs::write(dir.path().join("lib.py"), "def foo(): pass").expect("write temp file");
        let path = dir.path().to_str().expect("valid utf8 path").to_string();
        let params: AnalyzeSymbolParams = serde_json::from_value(serde_json::json!({
            "path": path,
            "symbol": "foo",
            "impl_only": true,
        }))
        .expect("valid AnalyzeSymbolParams JSON");
        let call = test_call(path.clone());

        // Act
        let _ = analyze_symbol_handler(ctx, params, call).await;

        // Assert
        let event = rx.try_recv().expect("expected an error metric event");
        assert_eq!(event.error_type.as_deref(), Some("invalid_params"));
        assert_eq!(
            event.error_subtype.as_deref(),
            Some("impl_only_requires_rust")
        );
    }

    #[tokio::test]
    async fn analyze_symbol_handler_invalid_git_ref_sets_git_ref_filter_failed_subtype() {
        // Arrange: git_ref containing whitespace is rejected before git is invoked.
        let (ctx, mut rx) = test_context();
        let dir = tempfile::TempDir::new().expect("create temp dir");
        std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").expect("write temp file");
        let path = dir.path().to_str().expect("valid utf8 path").to_string();
        let params: AnalyzeSymbolParams = serde_json::from_value(serde_json::json!({
            "path": path,
            "symbol": "foo",
            "git_ref": "bad ref",
        }))
        .expect("valid AnalyzeSymbolParams JSON");
        let call = test_call(path.clone());

        // Act
        let _ = analyze_symbol_handler(ctx, params, call).await;

        // Assert
        let event = rx.try_recv().expect("expected an error metric event");
        assert_eq!(event.error_type.as_deref(), Some("invalid_params"));
        assert_eq!(
            event.error_subtype.as_deref(),
            Some("git_ref_filter_failed")
        );
    }
}
