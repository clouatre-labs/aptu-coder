// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Focused analysis helpers for the `analyze_symbol` tool.
//!
//! Extracted functions for handling focused mode analysis, pagination, and caching.

use std::path::Path;
use std::sync::Arc;

use aptu_coder_core::analyze;
use aptu_coder_core::cache::{CacheTier, CallGraphCacheKey};
use aptu_coder_core::formatter::format_focused_paginated;
use aptu_coder_core::formatter_defuse::format_focused_paginated_defuse;
use aptu_coder_core::pagination::{
    CursorData, PaginationMode, decode_cursor, encode_cursor, paginate_slice,
};
use aptu_coder_core::traversal::{
    WalkEntry, changed_files_from_git_ref, filter_entries_by_git_ref, walk_directory,
};
use aptu_coder_core::types::AnalyzeSymbolParams;
use rmcp::model::{CallToolResult, ErrorData};
use tracing::instrument;

use crate::tools::common::{err_to_tool_result, error_meta};
use crate::{SIZE_LIMIT, err_to_tool_result_from_pagination};

use crate::tools::analyze_symbol::{
    AnalyzeSymbolContext, FocusedAnalysisParams, emit_error_metric, err_invalid_params,
    validate_impl_only,
};

/// Paginates a slice of call chains and returns the paginated items with an optional next cursor.
pub(crate) fn paginate_focus_chains(
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

/// Runs focused analysis with automatic summary fallback when output exceeds size limit.
pub(crate) async fn run_focused_with_auto_summary(
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

/// Handles focused mode analysis with caching and filtering.
#[instrument(skip(ctx, params, ct))]
pub(crate) async fn handle_focused_mode(
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

    Ok((CacheTier::L1L2Miss, output))
}

/// Applies pagination to call graph output based on cursor mode.
pub(crate) fn apply_call_graph_pagination(
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
