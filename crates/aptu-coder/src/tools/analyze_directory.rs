//! Extracted handler logic for the `analyze_directory` MCP tool.
//!
//! The `#[tool(...)]`-decorated method and `#[instrument]` outer decorator
//! remain in `lib.rs` as a thin shim. This module contains the free functions
//! that implement the actual logic, following the extraction pattern documented
//! in `tools/mod.rs`.

use aptu_coder_core::analyze;
use aptu_coder_core::cache::{CacheTier, DirectoryCacheKey};
use aptu_coder_core::formatter::{format_structure_paginated, format_summary};

/// Fixed server-side page size for analyze_directory. Clients cannot override it.
const ANALYZE_DIRECTORY_PAGE_SIZE: usize = 50;
use aptu_coder_core::traversal::{
    WalkEntry, changed_files_from_git_ref, filter_entries_by_git_ref, walk_directory,
};
use aptu_coder_core::types::{AnalysisMode, AnalyzeDirectoryParams};
use rmcp::model::{Annotations, CallToolResult, ContentBlock, ErrorData, TextContent};
use std::path::Path;
use std::sync::Arc;
use tracing::instrument;

use crate::SIZE_LIMIT;
use crate::tools::common::{
    err_to_tool_result, error_meta, no_cache_meta, normalize_cursor, summary_cursor_conflict,
};
use crate::tools::{AnalyzeDirectoryContext, DirectoryHandlerCall};

/// Relativizes each `FileInfo.path` in `output` against `base`.
///
/// `base` must already be canonicalized (see the traversal.rs canonicalize
/// pattern) so `strip_prefix` matches on symlink-resolved roots such as the
/// macOS /tmp -> /private/tmp alias. Paths outside `base` are left absolute;
/// a failed strip is silently ignored and never an error.
fn relativize_file_paths(output: &mut analyze::AnalysisOutput, base: &Path) {
    for file in &mut output.files {
        if let Ok(rel) = Path::new(&file.path).strip_prefix(base) {
            file.path = rel.to_string_lossy().into_owned();
        }
    }
}

/// Applies a safe text-level transformation to `output.formatted` so the text
/// payload matches the relativized `files[].path` values.
///
/// Formatters must consume absolute paths (their per-directory grouping joins
/// on `starts_with` against absolute `WalkEntry` paths), so `formatted` is
/// rendered from absolute paths and then transformed here: every occurrence of
/// a base prefix (canonical base and the raw `params.path`) followed by a
/// separator is stripped, mirroring `strip_prefix` in `relativize_file_paths`.
///
/// Runs in a single pass over the text: at each byte position the remaining
/// slice is tested against the (small, at most two-element) prefix set, so the
/// cost is O(N * P) where P is the prefix count, with no repeated full-string
/// reallocation the way a `replace` loop would incur.
fn relativize_formatted_text(formatted: &mut String, bases: &[&Path]) {
    // Platform-aware prefix construction: the trailing separator uses
    // MAIN_SEPARATOR, and each base also yields a variant with separators
    // swapped so params.path values containing either '/' or MAIN_SEPARATOR
    // produce a matching prefix.
    let sep = std::path::MAIN_SEPARATOR;
    let alt = if sep == '/' { '\\' } else { '/' };
    let mut prefixes: Vec<String> = Vec::with_capacity(bases.len() * 2);
    for base in bases {
        let trimmed = base.to_string_lossy();
        let trimmed = trimmed.trim_end_matches(['/', '\\']);
        // A base that trims to empty (e.g. "/") or reduces to a bare
        // filesystem root (e.g. "/" or "C:\") would yield a prefix that is
        // just the root or separator, stripping every separator from the
        // output and corrupting tree formatting. Such bases contribute
        // nothing to the prefix set.
        let is_bare_root = base.has_root() && base.parent().is_none();
        if trimmed.is_empty() || trimmed.len() <= 1 || is_bare_root {
            continue;
        }
        let mut prefix = String::with_capacity(trimmed.len() + 1);
        prefix.push_str(trimmed);
        prefix.push(sep);
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
        if alt != sep {
            let swapped: String = trimmed
                .chars()
                .map(|c| {
                    if c == sep {
                        alt
                    } else if c == alt {
                        sep
                    } else {
                        c
                    }
                })
                .collect();
            let mut prefix = String::with_capacity(swapped.len() + 1);
            prefix.push_str(&swapped);
            prefix.push(alt);
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
    }

    let mut result = String::with_capacity(formatted.len());
    let mut rest = formatted.as_str();
    'scan: while !rest.is_empty() {
        for prefix in &prefixes {
            if let Some(stripped) = rest.strip_prefix(prefix) {
                rest = stripped;
                continue 'scan;
            }
        }
        match rest.chars().next() {
            Some(c) => {
                result.push(c);
                rest = &rest[c.len_utf8()..];
            }
            None => break,
        }
    }
    *formatted = result;
}

/// Applies an optional `git_ref` filter to the directory walk entries.
///
/// If `git_ref` is `None` or empty, `entries` is returned unchanged.
/// If `git_ref` is non-empty, calls `changed_files_from_git_ref` to resolve the
/// changed set and filters `entries` accordingly.
pub(crate) fn apply_git_ref_filter(
    path: &Path,
    entries: Vec<WalkEntry>,
    git_ref: Option<&str>,
) -> Result<Vec<WalkEntry>, ErrorData> {
    if let Some(git_ref) = git_ref
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
        Ok(filter_entries_by_git_ref(entries, &changed, path))
    } else {
        Ok(entries)
    }
}

/// Core analysis logic for the `analyze_directory` tool (overview mode).
///
/// Checks L1/L2 caches, walks the directory, optionally filters by git ref,
/// spawns the blocking analysis task with progress tracking, and stores results.
#[instrument(skip(ctx, params, ct))]
pub(crate) async fn handle_overview_mode(
    ctx: &AnalyzeDirectoryContext,
    params: &AnalyzeDirectoryParams,
    ct: tokio_util::sync::CancellationToken,
) -> Result<(Arc<analyze::AnalysisOutput>, CacheTier), ErrorData> {
    let path = Path::new(&params.path);
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max_depth = params.max_depth;

    let all_entries = walk_directory(path, params.max_depth).map_err(|e| {
        ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            format!("Failed to walk directory: {e}"),
            Some(error_meta(
                "resource",
                false,
                "check path permissions and availability",
            )),
        )
    })?;

    let all_entries = apply_git_ref_filter(path, all_entries, params.git_ref.as_deref())?;

    let canonical_max_depth = max_depth.filter(|&d| d != 0);
    let git_ref_val = params.git_ref.as_deref().filter(|s| !s.is_empty());
    let cache_key = DirectoryCacheKey::from_entries(
        &all_entries,
        canonical_max_depth,
        AnalysisMode::Overview,
        git_ref_val,
    );

    if let Some(cached) = ctx.cache.get_directory(&cache_key) {
        tracing::debug!(cache_hit = true, message = "returning cached result");
        return Ok((cached, CacheTier::L1Memory));
    }

    let root = Path::new(&params.path);
    let disk_key = {
        let mut hasher = blake3::Hasher::new();
        let mut sorted_entries: Vec<_> = all_entries.iter().collect();
        sorted_entries.sort_by(|a, b| a.path.cmp(&b.path));
        for entry in &sorted_entries {
            let rel = entry.path.strip_prefix(root).unwrap_or(&entry.path);
            hasher.update(rel.as_os_str().to_string_lossy().as_bytes());
            let mtime_secs = entry
                .mtime
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            hasher.update(&mtime_secs.to_le_bytes());
        }
        if let Some(depth) = canonical_max_depth {
            hasher.update(depth.to_string().as_bytes());
        }
        if let Some(ref git_ref) = params.git_ref {
            hasher.update(git_ref.as_bytes());
        }
        hasher.finalize()
    };

    if let Some(cached) = ctx
        .disk_cache
        .get::<analyze::AnalysisOutput>("analyze_directory", &disk_key)
    {
        let arc = Arc::new(cached);
        ctx.cache.put_directory(cache_key.clone(), arc.clone());
        return Ok((arc, CacheTier::L2Disk));
    }

    let subtree_counts = if max_depth.is_some_and(|d| d > 0) {
        Some(aptu_coder_core::traversal::subtree_counts_from_entries(
            path,
            &all_entries,
        ))
    } else {
        None
    };

    let entries: Vec<WalkEntry> = if let Some(depth) = max_depth
        && depth > 0
    {
        all_entries
            .into_iter()
            .filter(|e| e.depth <= depth as usize)
            .collect()
    } else {
        all_entries
    };

    let path_owned = std::path::PathBuf::from(&params.path);
    let counter_clone = counter.clone();
    let ct_clone = ct.clone();

    let handle = tokio::task::spawn_blocking(move || {
        analyze::analyze_directory_with_progress(&path_owned, entries, counter_clone, ct_clone)
    });

    match handle.await {
        Ok(Ok(mut output)) => {
            output.subtree_counts = subtree_counts;
            let arc_output = Arc::new(output);
            ctx.cache.put_directory(cache_key, arc_output.clone());
            {
                let dc = ctx.disk_cache.clone();
                let k = disk_key;
                let v = arc_output.as_ref().clone();
                let spawn_handle = tokio::task::spawn_blocking(move || {
                    dc.put("analyze_directory", &k, &v);
                    dc.drain_write_failures()
                });
                let metrics_tx = ctx.metrics_tx.clone();
                let sid = ctx.sid.clone();
                tokio::spawn(async move {
                    if let Ok(failures) = spawn_handle.await
                        && failures > 0
                    {
                        tracing::warn!(
                            tool = "analyze_directory",
                            failures,
                            "L2 disk cache write failed"
                        );
                        metrics_tx.send(
                            crate::metrics::MetricEventBuilder::new("analyze_directory", "ok", 0)
                                .session_id(sid)
                                .cache_write_failure(Some(true))
                                .build(),
                        );
                    }
                });
            }
            Ok((arc_output, CacheTier::Miss))
        }
        Ok(Err(analyze::AnalyzeError::Cancelled)) => Err(ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            "Analysis cancelled".to_string(),
            Some(error_meta("transient", true, "analysis was cancelled")),
        )),
        Ok(Err(e)) => Err(ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            format!("Error analyzing directory: {e}"),
            Some(error_meta(
                "resource",
                false,
                "check path and file permissions",
            )),
        )),
        Err(e) => Err(ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            format!("Task join error: {e}"),
            Some(error_meta("transient", true, "retry the request")),
        )),
    }
}

/// Emit a terminal `result="error"` metric on an `invalid_params` early return
/// so the invocation does not disappear from the shutdown summary call_count.
/// `error_type` reflects the failure class: `invalid_params` for validation
/// failures, `internal_error` for walk/pagination failures.
#[allow(clippy::too_many_arguments)]
fn emit_validation_error(
    ctx: &AnalyzeDirectoryContext,
    params: &AnalyzeDirectoryParams,
    seq: u32,
    sid: &Option<String>,
    t_start: std::time::Instant,
    param_path: &str,
    cursor: Option<&str>,
    error_type: &str,
) {
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("analyze_directory", "error", dur)
            .param_path_depth(crate::metrics::path_component_count(param_path))
            .error_type(Some(error_type.to_string()))
            .session_id(sid.clone())
            .seq(Some(seq))
            .summary_mode(params.output_control.summary.unwrap_or(false))
            .is_paginated(cursor.is_some())
            .build(),
    );
}

/// Classify a terminal failure for the shared emit_internal_error helper: record span
/// fields, emit the validation metric, and wrap the `ErrorData` exactly once.
#[allow(clippy::too_many_arguments)]
fn emit_internal_error(
    span: &tracing::Span,
    ctx: &AnalyzeDirectoryContext,
    params: &AnalyzeDirectoryParams,
    seq: u32,
    sid: &Option<String>,
    t_start: std::time::Instant,
    param_path: &str,
    cursor: Option<&str>,
    error_type: &str,
    e: ErrorData,
) -> Result<CallToolResult, ErrorData> {
    span.record("error", true);
    span.record("error.type", error_type);
    emit_validation_error(
        ctx, params, seq, sid, t_start, param_path, cursor, error_type,
    );
    Ok(err_to_tool_result(e))
}

/// Handler body for the `analyze_directory` MCP tool.
///
/// Called by the thin shim in `lib.rs` after parameter extraction and metric
/// emission. Applies summary/pagination logic and builds the `CallToolResult`.
#[instrument(skip(ctx, params, call, span))]
pub(crate) async fn analyze_directory_handler(
    ctx: &AnalyzeDirectoryContext,
    params: AnalyzeDirectoryParams,
    call: DirectoryHandlerCall,
    span: &tracing::Span,
) -> Result<CallToolResult, ErrorData> {
    let DirectoryHandlerCall {
        seq,
        sid,
        t_start,
        param_path,
        max_depth_val,
        ct,
    } = call;
    let cursor = normalize_cursor(params.pagination.cursor.as_deref());
    let (arc_output, dir_cache_hit) = match handle_overview_mode(ctx, &params, ct).await {
        Ok(v) => v,
        Err(e) => {
            span.record("error", true);
            let error_type = match e.code {
                rmcp::model::ErrorCode::INVALID_PARAMS => "invalid_params",
                _ => "internal_error",
            };
            span.record("error.type", error_type);
            emit_validation_error(
                ctx,
                &params,
                seq,
                &sid,
                t_start,
                &param_path,
                cursor,
                error_type,
            );
            return Ok(err_to_tool_result(e));
        }
    };

    let mut output = match Arc::try_unwrap(arc_output) {
        Ok(owned) => owned,
        Err(arc) => (*arc).clone(),
    };

    if summary_cursor_conflict(params.output_control.summary, cursor) {
        span.record("error", true);
        span.record("error.type", "invalid_params");
        emit_validation_error(
            ctx,
            &params,
            seq,
            &sid,
            t_start,
            &param_path,
            cursor,
            "invalid_params",
        );
        return Ok(err_to_tool_result(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "summary=true is incompatible with a pagination cursor; use one or the other"
                .to_string(),
            Some(error_meta(
                "validation",
                false,
                "remove cursor or set summary=false",
            )),
        )));
    }

    let use_summary = if params.output_control.summary == Some(true) {
        true
    } else if params.output_control.summary == Some(false) {
        false
    } else {
        output.formatted.len() > SIZE_LIMIT
    };

    let use_paginated = params.output_control.summary == Some(false);

    if use_summary {
        output.formatted = format_summary(
            &output.entries,
            &output.files,
            params.max_depth,
            output.subtree_counts.as_deref(),
        );
    }

    let page_size = ANALYZE_DIRECTORY_PAGE_SIZE;
    let offset = match super::common::decode_offset(cursor) {
        Ok(o) => o,
        Err(e) => {
            return emit_internal_error(
                span,
                ctx,
                &params,
                seq,
                &sid,
                t_start,
                &param_path,
                cursor,
                "invalid_params",
                e,
            );
        }
    };

    let paginated =
        match super::common::paginate_or_internal_error(&output.files, offset, page_size) {
            Ok(v) => v,
            Err(e) => {
                return emit_internal_error(
                    span,
                    ctx,
                    &params,
                    seq,
                    &sid,
                    t_start,
                    &param_path,
                    cursor,
                    "internal_error",
                    e,
                );
            }
        };

    if use_paginated {
        output.formatted = format_structure_paginated(
            &paginated.items,
            paginated.total,
            params.max_depth,
            Some(Path::new(&params.path)),
            false,
        );
    }

    if use_paginated {
        output.next_cursor.clone_from(&paginated.next_cursor);
    } else {
        output.next_cursor = None;
    }

    // Relativize emitted FileInfo.path values against the canonicalized target
    // directory. This must run AFTER format_summary and
    // format_structure_paginated have consumed the absolute paths (their
    // per-directory grouping joins on starts_with) and immediately BEFORE
    // final_text/content_hash serialization. Because the
    // transform is emit-time, cached absolute-path output is relativized
    // identically on L1/L2/miss, so content_hash = blake3(final relativized
    // text) stays self-consistent with no cache version bump.
    let base = std::fs::canonicalize(&params.path)
        .unwrap_or_else(|_| std::path::PathBuf::from(&params.path));
    relativize_file_paths(&mut output, &base);
    // Keep the text payload in sync with the relativized structured paths.
    relativize_formatted_text(
        &mut output.formatted,
        &[&base, Path::new(params.path.trim_end_matches('/'))],
    );

    let mut final_text = output.formatted.clone();
    if use_paginated && let Some(cursor) = paginated.next_cursor {
        final_text.push('\n');
        final_text.push_str("NEXT_CURSOR: ");
        final_text.push_str(&cursor);
    }

    tracing::Span::current().record("cache_tier", dir_cache_hit.as_str());

    let content_hash = format!("{}", blake3::hash(final_text.as_bytes()));
    let mut meta = no_cache_meta().0;
    meta.insert(
        "content_hash".to_string(),
        serde_json::Value::String(content_hash),
    );
    let meta = rmcp::model::MetaObject(meta);

    let result = CallToolResult::success(vec![ContentBlock::Text(
        TextContent::new(final_text.clone())
            .with_annotations(Annotations::default().with_priority(0.9_f32)),
    )])
    .with_meta(Some(meta));
    let dur = t_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    ctx.metrics_tx.send(
        crate::metrics::MetricEventBuilder::new("analyze_directory", "ok", dur)
            .output_chars(final_text.len())
            .param_path_depth(crate::metrics::path_component_count(&param_path))
            .max_depth(max_depth_val)
            .session_id(sid)
            .seq(Some(seq))
            .cache_hit(Some(dir_cache_hit != CacheTier::Miss))
            .cache_tier(Some(dir_cache_hit.as_str()))
            .git_ref_used(params.git_ref.is_some())
            .summary_mode(use_summary)
            .is_paginated(cursor.is_some())
            .build(),
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aptu_coder_core::cache::AnalysisCache;

    #[test]
    fn relativize_formatted_text_prefix_construction_and_stripping() {
        // Arrange: base uses MAIN_SEPARATOR; text contains both a
        // MAIN_SEPARATOR-joined occurrence and a swapped-separator variant
        // (as a params.path containing the other separator would produce).
        let sep = std::path::MAIN_SEPARATOR;
        let alt = if sep == '/' { '\\' } else { '/' };
        let base = std::path::PathBuf::from(format!("tmp{sep}proj"));
        let mut text = format!(
            "first: tmp{sep}proj{sep}src{sep}lib.rs\nsecond: tmp{alt}proj{alt}src{alt}main.rs\nkeep: other{sep}file.txt"
        );

        // Act
        relativize_formatted_text(&mut text, &[&base]);

        // Assert: both separator variants are stripped; unrelated paths stay.
        assert_eq!(
            text,
            format!("first: src{sep}lib.rs\nsecond: src{alt}main.rs\nkeep: other{sep}file.txt")
        );
    }

    #[test]
    fn relativize_formatted_text_trims_trailing_separators_and_avoids_double_prefix() {
        // Arrange: base with a trailing separator should not yield '//'.
        let sep = std::path::MAIN_SEPARATOR;
        let base = std::path::PathBuf::from(format!("tmp{sep}proj{sep}"));
        let mut text = format!("tmp{sep}proj{sep}README.md");

        // Act
        relativize_formatted_text(&mut text, &[&base]);

        // Assert
        assert_eq!(text, "README.md");
    }

    #[test]
    fn relativize_formatted_text_skips_filesystem_root_bases() {
        // Arrange: a base at the filesystem root (and a trailing-separator
        // variant) must not produce a prefix that strips every separator.
        let base = std::path::Path::new("/");
        let base_trailing = std::path::Path::new("//");
        let mut text = String::from("src/lib.rs\nsrc/main.rs");

        // Act
        relativize_formatted_text(&mut text, &[base, base_trailing]);

        // Assert: byte-identical output
        assert_eq!(text, "src/lib.rs\nsrc/main.rs");
    }

    /// Builds a minimal `AnalyzeDirectoryContext` backed by an unbounded metrics
    /// channel, returning the context and the receiving end so tests can inspect
    /// emitted events.
    fn test_context() -> (
        AnalyzeDirectoryContext,
        tokio::sync::mpsc::UnboundedReceiver<crate::metrics::MetricEvent>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = AnalyzeDirectoryContext {
            cache: AnalysisCache::new(1),
            disk_cache: Arc::new(aptu_coder_core::cache::DiskCache::new(
                std::path::PathBuf::new(),
                true,
            )),
            metrics_tx: crate::metrics::MetricsSender(tx),
            sid: None,
        };
        (ctx, rx)
    }

    fn test_call(param_path: String) -> DirectoryHandlerCall {
        DirectoryHandlerCall {
            seq: 0,
            sid: None,
            t_start: std::time::Instant::now(),
            param_path,
            max_depth_val: None,
            ct: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn handler_git_ref_failure_emits_single_terminal_error_event() {
        // Arrange: a non-git directory with a git_ref filter forces
        // handle_overview_mode to fail (walk succeeds, git_ref filter fails).
        let (ctx, mut rx) = test_context();
        let dir = tempfile::TempDir::new().expect("create temp dir");
        std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").expect("write temp file");
        let path = dir.path().to_str().expect("valid utf8 path").to_string();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": path,
            "max_depth": 1,
            "git_ref": "nonexistent-ref",
        }))
        .expect("valid AnalyzeDirectoryParams JSON");
        let call = test_call(path.clone());

        // Act
        let _ = analyze_directory_handler(&ctx, params, call, &tracing::Span::none()).await;

        // Assert: exactly one terminal event, paired with the implicit receipt.
        let event = rx.try_recv().expect("expected a terminal error metric");
        assert_eq!(event.result, "error");
        assert_eq!(event.error_type.as_deref(), Some("invalid_params"));
        assert!(
            rx.try_recv().is_err(),
            "no additional events should be emitted"
        );
    }

    #[tokio::test]
    async fn emit_validation_error_internal_error_sends_terminal_event() {
        // Arrange: covers the paginate_slice / internal failure classification.
        let (ctx, mut rx) = test_context();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": "src",
        }))
        .expect("valid AnalyzeDirectoryParams JSON");

        // Act
        emit_validation_error(
            &ctx,
            &params,
            0,
            &None,
            std::time::Instant::now(),
            "src",
            None,
            "internal_error",
        );

        // Assert
        let event = rx.try_recv().expect("expected a terminal error metric");
        assert_eq!(event.result, "error");
        assert_eq!(event.error_type.as_deref(), Some("internal_error"));
    }

    /// Builds a context with an enabled disk cache rooted at `disk_base`.
    fn test_context_with_disk(disk_base: std::path::PathBuf) -> AnalyzeDirectoryContext {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        AnalyzeDirectoryContext {
            cache: AnalysisCache::new(1),
            disk_cache: Arc::new(aptu_coder_core::cache::DiskCache::new(disk_base, false)),
            metrics_tx: crate::metrics::MetricsSender(tx),
            sid: None,
        }
    }

    /// Extracts the blake3 content_hash from a successful result's meta.
    fn content_hash_of(result: &rmcp::model::CallToolResult) -> String {
        result
            .meta
            .as_ref()
            .expect("meta present")
            .0
            .get("content_hash")
            .and_then(|v| v.as_str())
            .expect("content_hash string")
            .to_string()
    }

    /// Waits until the disk cache has at least one entry (async L2 write).
    async fn wait_for_disk_entry(ctx: &AnalyzeDirectoryContext) {
        for _ in 0..500 {
            if ctx.disk_cache.cache_stats().0 > 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("disk cache entry never appeared");
    }

    /// Creates a temp source dir under the CWD containing `top.rs` and
    /// `sub/inner.rs`; optionally also creates a temp disk-cache dir.
    /// Returns (source dir, optional cache dir, source path as a string).
    fn setup_source_fixture(
        with_cache_dir: bool,
    ) -> (tempfile::TempDir, Option<tempfile::TempDir>, String) {
        // Arrange: temp source dir inside CWD (symlinked roots on macOS are
        // resolved by the canonicalized base).
        let cwd = std::env::current_dir().expect("cwd");
        let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
        let cache_dir =
            with_cache_dir.then(|| tempfile::TempDir::new_in(&cwd).expect("cache tempdir"));
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir sub");
        std::fs::write(dir.path().join("sub").join("inner.rs"), "fn inner() {}")
            .expect("write inner.rs");
        std::fs::write(dir.path().join("top.rs"), "fn top() {}").expect("write top.rs");
        let path = dir.path().to_str().expect("utf8 path").to_string();
        (dir, cache_dir, path)
    }

    #[tokio::test]
    async fn files_paths_relative_and_identical_across_cache_tiers() {
        let (_dir, cache_dir, path) = setup_source_fixture(true);
        let cache_dir = cache_dir.expect("cache tempdir");

        let make_params = || {
            let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
                "path": path,
                "summary": false,
            }))
            .expect("valid params");
            params
        };

        // Act + Assert (cache miss)
        let ctx1 = test_context_with_disk(cache_dir.path().to_path_buf());
        let result = analyze_directory_handler(
            &ctx1,
            make_params(),
            test_call(path.clone()),
            &tracing::Span::none(),
        )
        .await
        .expect("handler ok");
        assert!(
            result.structured_content.is_none(),
            "text-only emission: no structuredContent may be present"
        );
        let text = text_of(&result);
        let hash_miss = content_hash_of(&result);
        assert!(
            text.contains("top.rs") && text.contains("sub/inner.rs"),
            "text payload must list relativized file paths: {text}"
        );
        assert!(
            !text.contains(&format!("{path}/")),
            "text payload must contain no absolute path of the analyzed dir: {text}"
        );
        assert_eq!(
            hash_miss,
            format!("{}", blake3::hash(text.as_bytes())),
            "content_hash must hash the emitted relativized text"
        );
        wait_for_disk_entry(&ctx1).await;

        // Act + Assert (L1 hit: same ctx, same result)
        let result_l1 = analyze_directory_handler(
            &ctx1,
            make_params(),
            test_call(path.clone()),
            &tracing::Span::none(),
        )
        .await
        .expect("handler ok");
        assert_eq!(hash_miss, content_hash_of(&result_l1));
        let text_l1 = text_of(&result_l1);
        assert!(
            !text_l1.contains(&format!("{path}/")),
            "L1 hit text payload must contain no absolute path: {text_l1}"
        );

        // Act + Assert (L2 disk hit: fresh L1, shared disk cache)
        let ctx2 = test_context_with_disk(cache_dir.path().to_path_buf());
        let result_l2 = analyze_directory_handler(
            &ctx2,
            make_params(),
            test_call(path.clone()),
            &tracing::Span::none(),
        )
        .await
        .expect("handler ok");
        assert_eq!(hash_miss, content_hash_of(&result_l2));
        let text_l2 = text_of(&result_l2);
        assert!(
            !text_l2.contains(&format!("{path}/")),
            "L2 hit text payload must contain no absolute path: {text_l2}"
        );
        assert_eq!(
            text_l2,
            text_of(&result),
            "L2 hit text must match miss text"
        );
    }

    /// Extracts the text payload from a successful result's content blocks.
    fn text_of(result: &rmcp::model::CallToolResult) -> String {
        match result.content.first().expect("content") {
            rmcp::model::ContentBlock::Text(t) => t.text.clone(),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    /// Builds an `AnalysisOutput` from JSON (struct is non-exhaustive).
    fn output_from_json(files: serde_json::Value) -> analyze::AnalysisOutput {
        serde_json::from_value(serde_json::json!({ "formatted": "", "files": files }))
            .expect("valid AnalysisOutput JSON")
    }

    #[tokio::test]
    async fn relativize_keeps_absolute_paths_outside_base() {
        // Arrange: file paths that are not under the base must stay unchanged
        // (both absolute-outside and already-relative inputs).
        let mut output = output_from_json(serde_json::json!([
            { "path": "/elsewhere/other.rs", "language": "rust", "line_count": 1, "function_count": 1, "class_count": 0, "is_test": false },
            { "path": "already/relative.rs", "language": "rust", "line_count": 1, "function_count": 1, "class_count": 0, "is_test": false }
        ]));

        // Act
        relativize_file_paths(&mut output, Path::new("/base"));

        // Assert: strip_prefix failures keep the original paths, no error.
        assert_eq!(output.files[0].path, "/elsewhere/other.rs");
        assert_eq!(output.files[1].path, "already/relative.rs");
    }

    #[tokio::test]
    async fn summary_mode_per_directory_counts_survive_relativization() {
        // Ordering guard: format_summary must consume the absolute FileInfo
        // paths BEFORE relativization, otherwise the starts_with join in
        // summary.rs zeroes per-directory stats.
        let (_dir, _cache_dir, path) = setup_source_fixture(false);
        let (ctx, _rx) = test_context();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": path,
            "summary": true,
        }))
        .expect("valid params");

        // Act
        let result = analyze_directory_handler(
            &ctx,
            params,
            test_call(path.clone()),
            &tracing::Span::none(),
        )
        .await
        .expect("handler ok");

        // Assert: the sub/ section must report 1 file (not zero).
        let text = text_of(&result);
        assert!(
            !text.contains(&format!("{path}/")),
            "summary text payload must contain no absolute path: {text}"
        );
        assert!(
            text.contains("sub/ [1 files"),
            "per-directory count must survive; got: {text}"
        );
        assert!(
            !text.contains("showing 0"),
            "relativization must not zero per-directory stats; got: {text}"
        );
    }

    #[tokio::test]
    async fn relative_params_path_yields_relative_paths_on_symlinked_roots() {
        // Arrange: a relative params.path resolved through a symlinked CWD
        // (e.g. macOS /var -> /private/var) must still emit relative paths.
        let cwd = std::env::current_dir().expect("cwd");
        let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
        std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").expect("write lib.rs");
        let relative = dir
            .path()
            .strip_prefix(&cwd)
            .expect("relative to cwd")
            .to_str()
            .expect("utf8")
            .to_string();
        let (ctx, _rx) = test_context();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": relative,
        }))
        .expect("valid params");

        // Act
        let result =
            analyze_directory_handler(&ctx, params, test_call(relative), &tracing::Span::none())
                .await
                .expect("handler ok");

        // Assert: no absolute paths leak into the text block.
        let text = text_of(&result);
        let cwd_abs = format!("{}/", cwd.display());
        assert!(
            !text.contains(&cwd_abs),
            "text payload must contain no absolute path of the analyzed dir: {text}"
        );
        assert!(
            result.structured_content.is_none(),
            "text-only emission: no structuredContent may be present"
        );
    }

    #[tokio::test]
    async fn success_result_has_no_structured_content() {
        // Arrange + Act
        let (_dir, _cache_dir, path) = setup_source_fixture(false);
        let (ctx, _rx) = test_context();
        let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
            "path": path,
        }))
        .expect("valid params");
        let result = analyze_directory_handler(
            &ctx,
            params,
            test_call(path.clone()),
            &tracing::Span::none(),
        )
        .await
        .expect("handler ok");

        // Assert: regression guard against dual emission (MCP 2026-07-28
        // forbids structuredContent without a registered outputSchema).
        assert!(
            result.structured_content.is_none(),
            "analyze_directory must be text-only"
        );
    }

    #[tokio::test]
    async fn pagination_emits_next_cursor_line_on_page_1_only() {
        // Arrange: 60 files exceed the fixed page size of 50.
        let cwd = std::env::current_dir().expect("cwd");
        let dir = tempfile::TempDir::new_in(&cwd).expect("tempdir");
        for i in 0..60 {
            let name = dir.path().join(format!("file_{i:02}.rs"));
            std::fs::write(&name, "fn f() {}").expect("write file");
        }
        let path = dir.path().to_str().expect("utf8").to_string();
        let (ctx, _rx) = test_context();
        let make_params = |extra: serde_json::Value| {
            let mut v = serde_json::json!({ "path": path, "summary": false, "max_depth": 0 });
            if let (Some(obj), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
                obj.extend(extra.clone());
            }
            let params: AnalyzeDirectoryParams = serde_json::from_value(v).expect("valid params");
            params
        };

        // Act: page 1.
        let result1 = analyze_directory_handler(
            &ctx,
            make_params(serde_json::json!({})),
            test_call(path.clone()),
            &tracing::Span::none(),
        )
        .await
        .expect("handler ok");
        let text1 = text_of(&result1);

        // Assert: NEXT_CURSOR line is present on page 1 and carries the cursor.
        let cursor_line = text1
            .lines()
            .find(|l| l.starts_with("NEXT_CURSOR: "))
            .expect("page 1 must emit a NEXT_CURSOR line");
        let cursor = cursor_line
            .strip_prefix("NEXT_CURSOR: ")
            .expect("cursor after prefix")
            .to_string();
        assert!(!cursor.is_empty());

        // Act: page 2 via cursor only.
        let result2 = analyze_directory_handler(
            &ctx,
            make_params(serde_json::json!({ "cursor": cursor })),
            test_call(path.clone()),
            &tracing::Span::none(),
        )
        .await
        .expect("handler ok");
        let text2 = text_of(&result2);

        // Assert: page 2 terminates without a cursor and contains the tail.
        assert!(
            !text2.contains("NEXT_CURSOR: "),
            "page 2 must terminate: {text2}"
        );
        assert!(
            text2.contains("file_59.rs"),
            "page 2 must contain remaining files"
        );
    }
}
