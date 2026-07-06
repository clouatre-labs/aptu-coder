//! Extracted handler logic for the `analyze_symbol` MCP tool.
//!
//! The `#[tool(...)]`-decorated method and `#[instrument]` outer decorator
//! remain in `lib.rs` as a thin shim. This module contains the free functions
//! that implement the actual logic, following the extraction pattern documented
//! in `tools/mod.rs`.

use aptu_coder_core::analyze;
use aptu_coder_core::cache::{CacheTier, CallGraphCache, CallGraphCacheKey};
use aptu_coder_core::formatter::format_focused_paginated;
use aptu_coder_core::formatter_defuse::format_focused_paginated_defuse;
use aptu_coder_core::pagination::{
    CursorData, DEFAULT_PAGE_SIZE, PaginationMode, decode_cursor, encode_cursor, paginate_slice,
};
use aptu_coder_core::traversal::{
    WalkEntry, changed_files_from_git_ref, filter_entries_by_git_ref, walk_directory,
};
use aptu_coder_core::types::{AnalyzeSymbolParams, SymbolMatchMode};
use rmcp::model::{CallToolResult, Content, ErrorData, Meta};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tracing::instrument;

use crate::tools::common::{
    err_to_tool_result, error_meta, no_cache_meta, summary_cursor_conflict,
};
use crate::{SIZE_LIMIT, err_to_tool_result_from_pagination};

/// Shared handler context passed to extracted `analyze_symbol` free functions.
///
/// Bundles the `CodeAnalyzer` fields needed by the handler, keeping them
/// explicit without coupling to `&self`.
pub(crate) struct AnalyzeSymbolContext {
    pub(crate) metrics_tx: crate::metrics::MetricsSender,
    pub(crate) call_graph_cache: CallGraphCache,
    pub(crate) disk_cache: Arc<aptu_coder_core::cache::DiskCache>,
    pub(crate) sid: Option<String>,
    pub(crate) seq: u32,
}

/// Internal parameters for a focused analysis task.
#[derive(Clone)]
struct FocusedAnalysisParams {
    path: std::path::PathBuf,
    symbol: String,
    match_mode: SymbolMatchMode,
    follow_depth: u32,
    max_depth: Option<u32>,
    impl_only: Option<bool>,
    def_use: bool,
    parse_timeout_micros: Option<u64>,
}

/// Helper function to emit error metrics for analyze_symbol.
/// Extracts the error_type string from ErrorCode and records it on the span.
fn emit_error_metric(
    ctx: &AnalyzeSymbolContext,
    error_type: &str,
    t_start: std::time::Instant,
    param_path_depth: Option<usize>,
) {
    let dur = t_start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    tracing::Span::current().record("error", true);
    tracing::Span::current().record("error.type", error_type);
    let mut builder = crate::metrics::MetricEventBuilder::new("analyze_symbol", "error", dur)
        .error_type(Some(error_type.to_string()))
        .session_id(ctx.sid.clone())
        .seq(Some(ctx.seq));
    if let Some(depth) = param_path_depth {
        builder = builder.param_path_depth(depth);
    }
    ctx.metrics_tx.send(builder.build());
}

/// Emit an invalid_params error metric and return the corresponding `ErrorData`.
fn err_invalid_params(
    ctx: &AnalyzeSymbolContext,
    t_start: std::time::Instant,
    message: String,
    hint: &'static str,
) -> ErrorData {
    emit_error_metric(ctx, "invalid_params", t_start, None);
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

/// Helper function for paginating focus chains (callers or callees).
/// Returns (items, re-encoded_cursor_option).
fn paginate_focus_chains(
    chains: &[aptu_coder_core::graph::InternalCallChain],
    mode: PaginationMode,
    offset: usize,
    page_size: usize,
) -> Result<
    (
        Vec<aptu_coder_core::graph::InternalCallChain>,
        Option<String>,
    ),
    ErrorData,
> {
    let paginated = paginate_slice(chains, offset, page_size, mode).map_err(|e| {
        ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            e.to_string(),
            Some(error_meta("transient", true, "retry the request")),
        )
    })?;

    if paginated.next_cursor.is_none() && offset == 0 {
        return Ok((paginated.items, None));
    }

    let next = if let Some(raw_cursor) = paginated.next_cursor {
        let decoded = decode_cursor(&raw_cursor).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                e.to_string(),
                Some(error_meta("validation", false, "invalid cursor format")),
            )
        })?;
        Some(
            encode_cursor(&CursorData {
                mode,
                offset: decoded.offset,
            })
            .map_err(|e| {
                ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    e.to_string(),
                    Some(error_meta("transient", true, "retry the request")),
                )
            })?,
        )
    } else {
        None
    };

    Ok((paginated.items, next))
}

/// Run focused analysis with auto-summary retry on SIZE_LIMIT overflow.
async fn run_focused_with_auto_summary(
    ctx: &AnalyzeSymbolContext,
    params: &AnalyzeSymbolParams,
    analysis_params: &FocusedAnalysisParams,
    counter: Arc<std::sync::atomic::AtomicUsize>,
    ct: tokio_util::sync::CancellationToken,
    entries: Arc<Vec<WalkEntry>>,
) -> Result<analyze::FocusedAnalysisOutput, ErrorData> {
    let use_summary_for_task = params.output_control.summary == Some(true);

    let config_initial = analyze::FocusedAnalysisConfig {
        focus: analysis_params.symbol.clone(),
        match_mode: analysis_params.match_mode.clone(),
        follow_depth: analysis_params.follow_depth,
        max_depth: analysis_params.max_depth,
        ast_recursion_limit: None,
        use_summary: use_summary_for_task,
        impl_only: analysis_params.impl_only,
        def_use: analysis_params.def_use,
        parse_timeout_micros: analysis_params.parse_timeout_micros,
    };

    let t_start = std::time::Instant::now();

    let mut output = tokio::task::spawn_blocking({
        let path = analysis_params.path.clone();
        let entries = entries.clone();
        let counter = counter.clone();
        let ct = ct.clone();
        let config = config_initial.clone();
        move || {
            analyze::analyze_focused_with_progress_with_entries(
                &path, &config, &counter, &ct, &entries,
            )
        }
    })
    .await
    .map_err(|e| {
        emit_error_metric(ctx, "internal_error", t_start, None);
        ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            format!("analysis task panicked: {e}"),
            None,
        )
    })?
    .map_err(|e| {
        emit_error_metric(ctx, "internal_error", t_start, None);
        ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            format!("analysis failed: {e}"),
            None,
        )
    })?;

    if params.output_control.summary.is_none() && output.formatted.len() > SIZE_LIMIT {
        tracing::debug!(
            auto_summary = true,
            message = "output exceeded size limit, retrying with summary"
        );
        let counter2 = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let config_retry = analyze::FocusedAnalysisConfig {
            use_summary: true,
            ..config_initial
        };
        let summary_result = tokio::task::spawn_blocking({
            let path = analysis_params.path.clone();
            let entries = entries.clone();
            move || {
                analyze::analyze_focused_with_progress_with_entries(
                    &path,
                    &config_retry,
                    &counter2,
                    &ct,
                    &entries,
                )
            }
        })
        .await
        .ok()
        .and_then(|r| r.ok());

        if let Some(summary_output) = summary_result {
            output.formatted = summary_output.formatted;
        } else {
            let estimated_tokens = output.formatted.len() / 4;
            let message = format!(
                "Output exceeds 50K chars ({} chars, ~{} tokens). Use summary=true or narrow your scope.",
                output.formatted.len(),
                estimated_tokens
            );
            return Err(err_invalid_params(
                ctx,
                t_start,
                message,
                "use summary=true or narrow scope",
            ));
        }
    } else if output.formatted.len() > SIZE_LIMIT && params.output_control.summary == Some(false) {
        let estimated_tokens = output.formatted.len() / 4;
        let message = format!(
            "Output exceeds 50K chars ({} chars, ~{} tokens). Use one of:\n\
             - summary=true to get compact summary\n\
             - Narrow your scope (smaller directory, specific file)",
            output.formatted.len(),
            estimated_tokens
        );
        return Err(err_invalid_params(
            ctx,
            t_start,
            message,
            "use summary=true or narrow scope",
        ));
    }

    Ok(output)
}

/// Core analysis logic for focused mode (`analyze_symbol`).
/// Returns `(CacheTier, FocusedAnalysisOutput)`.
#[instrument(skip(ctx, params, ct))]
async fn handle_focused_mode(
    ctx: &AnalyzeSymbolContext,
    params: &AnalyzeSymbolParams,
    ct: tokio_util::sync::CancellationToken,
) -> Result<(CacheTier, analyze::FocusedAnalysisOutput), ErrorData> {
    let path = Path::new(&params.path);
    let raw_entries = match walk_directory(path, params.max_depth) {
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
    let filtered_entries = if let Some(ref git_ref) = params.git_ref
        && !git_ref.is_empty()
    {
        let changed = changed_files_from_git_ref(path, git_ref).map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                format!("git_ref filter failed: {e}"),
                Some(error_meta(
                    "resource",
                    false,
                    "ensure git is installed and path is inside a git repository",
                )),
            )
        })?;
        filter_entries_by_git_ref(raw_entries, &changed, path)
    } else {
        raw_entries
    };
    let entries = Arc::new(filtered_entries);

    if params.impl_only == Some(true) {
        validate_impl_only(&entries)?;
    }

    // Build cache key for this call-graph request.
    let cache_key = CallGraphCacheKey::from_entries(
        path,
        &entries,
        params.git_ref.as_deref(),
        params.follow_depth.unwrap_or(1),
        &params.match_mode.clone().unwrap_or_default(),
        params.impl_only.unwrap_or(false),
        None,
    );

    // Check L1 cache first.
    if let Some(cached) = ctx.call_graph_cache.get(&cache_key) {
        return Ok((CacheTier::L1Memory, (*cached).clone()));
    }

    // Compute L2 disk cache key by streaming CallGraphCacheKey fields through blake3.
    // Same pattern as analyze_directory: root_path + git_ref + follow_depth + match_mode
    // + impl_only + per-file mtimes.
    let disk_key = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(path.as_os_str().to_string_lossy().as_bytes());
        if let Some(ref git_ref) = params.git_ref {
            hasher.update(git_ref.as_bytes());
        }
        hasher.update(&params.follow_depth.unwrap_or(1).to_le_bytes());
        let match_mode_str =
            match serde_json::to_string(&params.match_mode.clone().unwrap_or_default()) {
                Ok(s) => s,
                Err(e) => {
                    // Serialization of a unit-like enum should never fail; if it does,
                    // an empty string would produce a non-unique cache key, so warn loudly.
                    tracing::warn!(
                        error = %e,
                        "analyze_symbol: failed to serialize match_mode for disk cache key; \
                         falling back to empty string (cache key may collide)"
                    );
                    String::new()
                }
            };
        hasher.update(match_mode_str.as_bytes());
        hasher.update(&[u8::from(params.impl_only.unwrap_or(false))]);
        // Stream sorted per-file (path, mtime_nanos) pairs for freshness.
        let mut sorted_entries: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();
        sorted_entries.sort_by(|a, b| a.path.cmp(&b.path));
        for entry in &sorted_entries {
            // `path` is always a canonical absolute path (validated upstream by
            // validate_path before handle_focused_mode is called), so strip_prefix
            // succeeds for every entry under it. The unwrap_or fallback retains the
            // full absolute path, which is still unique and safe for hashing.
            let rel = entry.path.strip_prefix(path).unwrap_or(&entry.path);
            hasher.update(rel.as_os_str().to_string_lossy().as_bytes());
            let mtime_nanos = entry
                .mtime
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            hasher.update(&mtime_nanos.to_le_bytes());
        }
        hasher.finalize()
    };

    // Check L2 disk cache.
    if let Some(cached) = ctx
        .disk_cache
        .get::<analyze::FocusedAnalysisOutput>("analyze_symbol", &disk_key)
    {
        let arc = Arc::new(cached.clone());
        ctx.call_graph_cache.put(cache_key, arc);
        return Ok((CacheTier::L2Disk, cached));
    }

    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let analysis_params = FocusedAnalysisParams {
        path: path.to_path_buf(),
        symbol: params.symbol.clone(),
        match_mode: params.match_mode.clone().unwrap_or_default(),
        follow_depth: params.follow_depth.unwrap_or(1),
        max_depth: params.max_depth,
        impl_only: params.impl_only,
        def_use: params.def_use.unwrap_or(false),
        parse_timeout_micros: None,
    };

    let mut output =
        run_focused_with_auto_summary(ctx, params, &analysis_params, counter, ct, entries).await?;

    if params.impl_only == Some(true) {
        let filter_line = format!(
            "FILTER: impl_only=true ({} of {} callers shown)\n",
            output.impl_trait_caller_count, output.unfiltered_caller_count
        );
        output.formatted = format!("{}{}", filter_line, output.formatted);

        if output.impl_trait_caller_count == 0 {
            output.formatted.push_str(
                "\nNOTE: No impl-trait callers found. The symbol may be a plain function or struct, not a trait method. Remove impl_only to see all callers.\n"
            );
        }
    }

    // Store in L1 cache for subsequent calls.
    ctx.call_graph_cache
        .put(cache_key, Arc::new(output.clone()));

    // Spawn L2 write-behind; drain failure counter after write completes.
    {
        let dc = ctx.disk_cache.clone();
        let k = disk_key;
        let v = output.clone();
        let handle = tokio::task::spawn_blocking(move || {
            dc.put("analyze_symbol", &k, &v);
            dc.drain_write_failures()
        });
        let metrics_tx = ctx.metrics_tx.clone();
        let sid = ctx.sid.clone();
        tokio::spawn(async move {
            if let Ok(failures) = handle.await
                && failures > 0
            {
                tracing::warn!(
                    tool = "analyze_symbol",
                    failures,
                    "L2 disk cache write failed"
                );
                metrics_tx.send(
                    crate::metrics::MetricEventBuilder::new("analyze_symbol", "ok", 0)
                        .session_id(sid)
                        .cache_write_failure(Some(true))
                        .build(),
                );
            }
        });
    }

    Ok((CacheTier::Miss, output))
}

/// Handle the `import_lookup` mode: scan for files importing `params.symbol` as a module path.
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
            emit_error_metric(
                &ctx,
                error_type_str,
                t_start,
                Some(crate::metrics::path_component_count(&param_path)),
            );
            return Ok(err_to_tool_result(e));
        }
        Err(e) => {
            emit_error_metric(
                &ctx,
                "internal_error",
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

    let mut result = CallToolResult::success(vec![
        Content::text(final_text.clone()).with_priority(0.9_f32),
    ])
    .with_meta(Some(Meta(meta)));
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
            .match_mode(params.match_mode.as_ref().map(|m| format!("{:?}", m).to_lowercase()))
            .follow_depth(params.follow_depth)
            .import_lookup(params.import_lookup.unwrap_or(false))
            .def_use(params.def_use.unwrap_or(false))
            .impl_only(params.impl_only.unwrap_or(false))
            .git_ref_used(params.git_ref.is_some())
            .is_paginated(params.pagination.cursor.is_some())
            .summary_mode(params.output_control.summary.unwrap_or(false))
            .build(),
    );
    Ok(result)
}

/// Apply call-graph pagination to `output` for the given `cursor_mode`.
///
/// Mutates `output.formatted` and `output.def_use_sites` in place and returns
/// the raw next-cursor string (before bootstrap logic) or an early-exit error.
fn apply_call_graph_pagination(
    output: &mut analyze::FocusedAnalysisOutput,
    params: &AnalyzeSymbolParams,
    cursor_mode: PaginationMode,
    offset: usize,
    page_size: usize,
    use_summary: bool,
) -> Result<Option<String>, CallToolResult> {
    match cursor_mode {
        PaginationMode::Callers => {
            let (paginated_items, paginated_next) = paginate_focus_chains(
                &output.prod_chains,
                PaginationMode::Callers,
                offset,
                page_size,
            )
            .map_err(err_to_tool_result)?;

            if !use_summary
                && (paginated_next.is_some() || offset > 0 || !output.outgoing_chains.is_empty())
            {
                let base_path = Path::new(&params.path);
                output.formatted = format_focused_paginated(
                    &paginated_items,
                    output.prod_chains.len(),
                    PaginationMode::Callers,
                    &params.symbol,
                    &output.prod_chains,
                    &output.test_chains,
                    &output.outgoing_chains,
                    output.def_count,
                    offset,
                    Some(base_path),
                    false,
                );
                Ok(paginated_next)
            } else {
                Ok(None)
            }
        }
        PaginationMode::Callees => {
            let (paginated_items, paginated_next) = paginate_focus_chains(
                &output.outgoing_chains,
                PaginationMode::Callees,
                offset,
                page_size,
            )
            .map_err(err_to_tool_result)?;

            if paginated_next.is_some() || offset > 0 {
                let base_path = Path::new(&params.path);
                output.formatted = format_focused_paginated(
                    &paginated_items,
                    output.outgoing_chains.len(),
                    PaginationMode::Callees,
                    &params.symbol,
                    &output.prod_chains,
                    &output.test_chains,
                    &output.outgoing_chains,
                    output.def_count,
                    offset,
                    Some(base_path),
                    false,
                );
                Ok(paginated_next)
            } else {
                Ok(None)
            }
        }
        PaginationMode::Default => Err(err_to_tool_result(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "invalid cursor: unknown pagination mode".to_string(),
            Some(error_meta(
                "validation",
                false,
                "use a cursor returned by a previous analyze_symbol call",
            )),
        ))),
        PaginationMode::DefUse => {
            let total_sites = output.def_use_sites.len();
            let (paginated_sites, paginated_next) = paginate_slice(
                &output.def_use_sites,
                offset,
                page_size,
                PaginationMode::DefUse,
            )
            .map(|r| (r.items, r.next_cursor))
            .map_err(err_to_tool_result_from_pagination)?;

            if !use_summary {
                let base_path = Path::new(&params.path);
                output.formatted = format_focused_paginated_defuse(
                    &paginated_sites,
                    total_sites,
                    &params.symbol,
                    offset,
                    Some(base_path),
                    false,
                );
            }
            output.def_use_sites = paginated_sites;
            Ok(paginated_next)
        }
    }
}

/// Decode the pagination cursor from `params` and return `(offset, cursor_mode)`.
///
/// Returns `Err(CallToolResult)` on a malformed cursor so the caller can
/// propagate the error immediately.
fn decode_call_graph_cursor(
    params: &AnalyzeSymbolParams,
) -> Result<(usize, PaginationMode), CallToolResult> {
    let cursor_mode = params
        .pagination
        .cursor
        .as_deref()
        .map(|s| {
            decode_cursor(s)
                .map(|c| c.mode)
                .unwrap_or(PaginationMode::Callers)
        })
        .unwrap_or(PaginationMode::Callers);

    let offset = if let Some(ref cursor_str) = params.pagination.cursor {
        match decode_cursor(cursor_str).map_err(|e| {
            err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                e.to_string(),
                Some(error_meta("validation", false, "invalid cursor format")),
            ))
        }) {
            Ok(v) => v.offset,
            Err(e) => return Err(e),
        }
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

    // Call handler for analysis and progress tracking
    let (graph_cache_tier, mut output) = match handle_focused_mode(&ctx, &params, ct).await {
        Ok(v) => v,
        Err(e) => {
            let error_type_str = match e.code {
                rmcp::model::ErrorCode::INVALID_PARAMS => "invalid_params",
                rmcp::model::ErrorCode::INTERNAL_ERROR => "internal_error",
                _ => "unknown",
            };
            emit_error_metric(
                &ctx,
                error_type_str,
                t_start,
                Some(crate::metrics::path_component_count(&param_path)),
            );
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

    let mut result = CallToolResult::success(vec![
        Content::text(final_text.clone()).with_priority(0.9_f32),
    ])
    .with_meta(Some(Meta(meta)));
    // Only include def_use_sites in structuredContent when in DefUse mode.
    // In Callers/Callees modes, clearing the vec prevents large def-use
    // payloads from leaking into paginated non-def-use responses.
    if cursor_mode != PaginationMode::DefUse {
        output.def_use_sites = Vec::new();
    }
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
            .cache_hit(Some(graph_cache_tier != CacheTier::Miss))
            .cache_tier(Some(graph_cache_tier.as_str()))
            .match_mode(params.match_mode.as_ref().map(|m| format!("{:?}", m).to_lowercase()))
            .follow_depth(params.follow_depth)
            .import_lookup(params.import_lookup.unwrap_or(false))
            .def_use(params.def_use.unwrap_or(false))
            .impl_only(params.impl_only.unwrap_or(false))
            .git_ref_used(params.git_ref.is_some())
            .is_paginated(params.pagination.cursor.is_some())
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

    if std::path::Path::new(&params.path).is_file() {
        emit_error_metric(&ctx, "invalid_params", t_start, None);
        return invalid_params(
            span,
            format!(
                "'{}' is a file; analyze_symbol requires a directory path",
                params.path
            ),
            "pass a directory path, not a file",
        );
    }

    if summary_cursor_conflict(
        params.output_control.summary,
        params.pagination.cursor.as_deref(),
    ) {
        emit_error_metric(&ctx, "invalid_params", t_start, None);
        return invalid_params(
            span,
            "summary=true is incompatible with a pagination cursor; use one or the other",
            "remove cursor or set summary=false",
        );
    }

    if params.import_lookup == Some(true) && params.def_use == Some(true) {
        emit_error_metric(&ctx, "invalid_params", t_start, None);
        return invalid_params(
            span,
            "import_lookup=true and def_use=true are mutually exclusive; use one or the other",
            "remove import_lookup or set def_use=false",
        );
    }

    if let Err(e) = validate_import_lookup(params.import_lookup, &params.symbol) {
        emit_error_metric(&ctx, "invalid_params", t_start, None);
        return Ok(err_to_tool_result(e));
    }

    if params.import_lookup == Some(true) {
        handle_import_lookup(ctx, params, call).await
    } else {
        handle_call_graph(ctx, params, call).await
    }
}
