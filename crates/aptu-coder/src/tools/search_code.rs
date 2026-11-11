// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Handler logic for the `search_code` MCP tool (structural code search).
//!
//! Follows the extraction pattern documented in `tools/mod.rs`: a free function
//! called by the thin `#[tool(...)]` shim in `lib.rs`. Pattern mode delegates to
//! [`aptu_coder_core::search::pattern_search`] over the in-tree tree-sitter
//! parsers; symbol mode builds a [`aptu_coder_core::graph::structural::StructuralGraph`]
//! from a directory analysis on demand (no `GraphDiskStore` persistence; the
//! #1819 warm-up premise was refuted).
//!
//! **Instrumentation budget:** telemetry on this handler is zero-allocation on
//! the hot path beyond the event itself. The only timing work is the single
//! `t_start.elapsed()` recorded per call; emission goes through the existing
//! fire-and-forget bounded channel (drop-counted, never blocks). Do not add
//! synchronous I/O, locks, or per-match allocation to the metric path.

use crate::tools::common::{
    append_pagination_footer, decode_offset, err_to_tool_result, error_meta, no_cache_meta,
    normalize_cursor, paginate_or_internal_error,
};
use aptu_coder_core::search::{self, SearchError, SearchMatch};
use rmcp::model::{CallToolResult, ContentBlock, ErrorData, TextContent};
use tracing::instrument;

/// Shared handler context passed to the extracted `search_code` free function.
pub(crate) struct SearchCodeContext {
    pub(crate) metrics_tx: crate::metrics::MetricsSender,
}

/// Stable `error_subtype` values for `search_code` `invalid_params` telemetry,
/// following the per-tool golden-list pattern (`analyze_file::subtypes`).
pub(crate) mod subtypes {
    /// `pattern` failed to parse in the target language or was absent.
    pub(super) const INVALID_PATTERN: &str = "invalid_pattern";
    /// Pattern mode was requested for a language outside the supported four.
    pub(super) const UNSUPPORTED_LANGUAGE: &str = "unsupported_language";
    /// Symbol mode found no definitions/implementors for the requested symbol.
    pub(super) const SYMBOL_NOT_FOUND: &str = "symbol_not_found";
    /// `path` exists but is neither a directory nor a regular file.
    pub(super) const PATH_NOT_A_FILE: &str = "path_not_a_file";
    /// The pagination `cursor` failed to decode.
    pub(super) const INVALID_CURSOR: &str = "invalid_cursor";
}

/// Page size for `search_code` results; server-owned, mirroring the analyze tools.
const SEARCH_PAGE_SIZE: usize = 50;

/// Emit a terminal `result="error"` metric on an early return so the invocation
/// does not disappear from the shutdown summary call_count.
#[allow(clippy::too_many_arguments)]
fn emit_validation_error(
    ctx: &SearchCodeContext,
    params: &crate::SearchCodeParams,
    seq: u32,
    sid: &Option<String>,
    t_start: std::time::Instant,
    param_path: &str,
    error_subtype: &'static str,
) {
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("search_code", "error", dur)
            .param_path_depth(crate::metrics::path_component_count(param_path))
            .error_type(Some("invalid_params".to_string()))
            .error_subtype(Some(error_subtype.to_string()))
            .query_mode(Some(params.query_mode_label()))
            .session_id(sid.clone())
            .seq(Some(seq))
            .is_paginated(params.cursor.as_deref().is_some_and(|c| !c.is_empty()))
            .build(),
    );
}

/// Render one match as a byte-exact anchored block.
fn render_match(m: &SearchMatch) -> String {
    format!(
        "{}:{}:{} bytes {}..{} [{}]\n{}\n",
        m.file,
        m.start_line,
        m.start_col,
        m.start_byte,
        m.end_byte,
        m.kind.as_str(),
        m.text
    )
}

/// Run pattern mode across a directory (only the four pattern languages) or a
/// single file, collecting byte-exact matches.
fn run_pattern_mode(
    path: &std::path::Path,
    language: &str,
    pattern: &str,
) -> Result<Vec<SearchMatch>, SearchError> {
    let mut matches = Vec::new();
    if path.is_dir() {
        let entries = aptu_coder_core::traversal::walk_directory(path, None)
            .map_err(|e| SearchError::InvalidPattern(format!("directory walk failed: {e}")))?;
        for entry in entries {
            if entry.is_dir {
                continue;
            }
            let ext = entry.path.extension().and_then(|e| e.to_str());
            let Some(lang_name) = ext.and_then(aptu_coder_core::language_for_extension) else {
                continue;
            };
            if lang_name != language {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&entry.path) else {
                continue;
            };
            let file = entry.path.to_string_lossy().to_string();
            matches.extend(search::pattern_search(pattern, language, &source, &file)?);
        }
    } else {
        let source = std::fs::read_to_string(path)
            .map_err(|e| SearchError::InvalidPattern(format!("read failed: {e}")))?;
        let file = path.to_string_lossy().to_string();
        matches.extend(search::pattern_search(pattern, language, &source, &file)?);
    }
    matches.sort_by(|a, b| (a.file.as_str(), a.start_byte).cmp(&(b.file.as_str(), b.start_byte)));
    Ok(matches)
}

/// Handler body for the `search_code` MCP tool.
///
/// Called by the thin shim in `lib.rs` after parameter extraction and metric
/// emission. Runs pattern or symbol search off the async thread and paginates
/// byte-exact matches with the shared `CursorData` machinery.
#[instrument(skip(ctx, params, span))]
#[allow(clippy::too_many_lines)] // single dispatch function; splitting harms readability
pub(crate) async fn search_code_handler(
    ctx: &SearchCodeContext,
    params: crate::SearchCodeParams,
    seq: u32,
    sid: Option<String>,
    t_start: std::time::Instant,
    param_path: String,
    span: &tracing::Span,
) -> Result<CallToolResult, ErrorData> {
    let cursor = normalize_cursor(params.cursor.as_deref());

    // Telemetry note: all metric emissions below obey the zero-hot-path-allocation
    // budget documented at the top of this module.
    let offset = match decode_offset(cursor) {
        Ok(o) => o,
        Err(e) => {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            emit_validation_error(
                ctx,
                &params,
                seq,
                &sid,
                t_start,
                &param_path,
                subtypes::INVALID_CURSOR,
            );
            return Ok(err_to_tool_result(e));
        }
    };

    let path = std::path::Path::new(&params.path);
    if !path.is_dir() && !path.is_file() {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        emit_validation_error(
            ctx,
            &params,
            seq,
            &sid,
            t_start,
            &param_path,
            subtypes::PATH_NOT_A_FILE,
        );
        return Ok(err_to_tool_result(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "path is neither a directory nor a regular file".to_string(),
            Some(error_meta(
                "validation",
                false,
                "pass a directory root or a source file path",
            )),
        )));
    }

    let mode = params.mode;
    let query_mode_label = params.query_mode_label();
    let spath = params.path.clone();
    let spattern = params.pattern.clone();
    let slang = params.language.clone();
    let ssymbol = params.symbol.clone();
    let simpl = params.impl_only;
    let search_result = tokio::task::spawn_blocking(move || -> Result<Vec<SearchMatch>, String> {
        match mode {
            crate::SearchMode::Pattern => {
                let Some(pattern) = spattern.as_deref().filter(|p| !p.is_empty()) else {
                    return Err(subtypes::INVALID_PATTERN.to_string());
                };
                let Some(language) = slang.as_deref().map(str::to_owned) else {
                    return Err(subtypes::INVALID_PATTERN.to_string());
                };
                run_pattern_mode(std::path::Path::new(&spath), &language, pattern).map_err(|e| {
                    match e {
                        SearchError::UnsupportedLanguage(_) => {
                            subtypes::UNSUPPORTED_LANGUAGE.to_string()
                        }
                        // SearchError is #[non_exhaustive]; all remaining variants
                        // are pattern-side failures.
                        _ => subtypes::INVALID_PATTERN.to_string(),
                    }
                })
            }
            crate::SearchMode::Symbol => {
                let Some(symbol) = ssymbol.as_deref().filter(|s| !s.is_empty()) else {
                    return Err(subtypes::SYMBOL_NOT_FOUND.to_string());
                };
                search::symbol_search(std::path::Path::new(&spath), symbol, simpl.unwrap_or(false))
                    .map_err(|e| e.to_string())
            }
        }
    })
    .await
    .unwrap_or_else(|e| Err(format!("internal_error: {e}")));

    let matches = match search_result {
        Ok(m) => m,
        Err(subtype) => {
            let subtype: &'static str = match subtype.as_str() {
                s if s == subtypes::UNSUPPORTED_LANGUAGE => subtypes::UNSUPPORTED_LANGUAGE,
                s if s == subtypes::SYMBOL_NOT_FOUND => subtypes::SYMBOL_NOT_FOUND,
                s if s == subtypes::INVALID_PATTERN => subtypes::INVALID_PATTERN,
                _ => {
                    span.record("error", true);
                    span.record("error.type", "internal_error");
                    emit_validation_error(
                        ctx,
                        &params,
                        seq,
                        &sid,
                        t_start,
                        &param_path,
                        subtypes::INVALID_PATTERN,
                    );
                    return Ok(err_to_tool_result(ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        subtype,
                        Some(error_meta("internal", false, "report this as a bug")),
                    )));
                }
            };
            span.record("error", true);
            span.record("error.type", "invalid_params");
            emit_validation_error(ctx, &params, seq, &sid, t_start, &param_path, subtype);
            let message = match subtype {
                s if s == subtypes::UNSUPPORTED_LANGUAGE => format!(
                    "unsupported language for pattern mode; supported: {}",
                    search::PATTERN_LANGUAGES.join(", ")
                ),
                s if s == subtypes::SYMBOL_NOT_FOUND => {
                    "symbol not found; verify the exact symbol name".to_string()
                }
                _ => "pattern does not parse in the target language; patterns are code-shaped and may require metavariables ($NAME, $$$REST)".to_string(),
            };
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                message,
                Some(error_meta(
                    "validation",
                    false,
                    "fix the request and retry; see the parameter descriptions",
                )),
            )));
        }
    };

    if matches.is_empty() {
        let empty_message = if mode == crate::SearchMode::Symbol {
            subtypes::SYMBOL_NOT_FOUND
        } else {
            // An empty pattern result is a valid outcome, not an error.
            ""
        };
        if !empty_message.is_empty() {
            span.record("error", true);
            span.record("error.type", "invalid_params");
            emit_validation_error(ctx, &params, seq, &sid, t_start, &param_path, empty_message);
            return Ok(err_to_tool_result(ErrorData::new(
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "symbol not found; verify the exact symbol name".to_string(),
                Some(error_meta(
                    "validation",
                    false,
                    "verify the exact symbol name or use pattern mode",
                )),
            )));
        }
    }

    let paginated =
        paginate_or_internal_error(&matches, offset, SEARCH_PAGE_SIZE).map_err(|e| {
            span.record("error", true);
            span.record("error.type", "internal_error");
            emit_validation_error(
                ctx,
                &params,
                seq,
                &sid,
                t_start,
                &param_path,
                subtypes::INVALID_CURSOR,
            );
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                e.to_string(),
                Some(error_meta("transient", true, "retry the request")),
            )
        })?;

    let mut final_text = format!(
        "Search: mode={query_mode_label} path={} matches={} (page {}..{})\n\n",
        params.path,
        paginated.total,
        offset,
        offset + paginated.items.len()
    );
    for m in &paginated.items {
        final_text.push_str(&render_match(m));
        final_text.push('\n');
    }
    append_pagination_footer(&mut final_text, paginated.next_cursor.as_deref(), cursor);

    let mut meta = no_cache_meta().0;
    let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
    meta.insert(
        "content_hash".to_string(),
        serde_json::Value::String(content_hash),
    );
    let meta = rmcp::model::MetaObject(meta);

    let result = CallToolResult::success(vec![ContentBlock::Text(TextContent::new(
        final_text.clone(),
    ))])
    .with_meta(Some(meta));
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("search_code", "ok", dur)
            .output_chars(final_text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .query_mode(Some(query_mode_label))
            .result_count(Some(paginated.items.len()))
            .session_id(sid)
            .seq(Some(seq))
            .is_paginated(cursor.is_some())
            .build(),
    );
    Ok(result)
}
