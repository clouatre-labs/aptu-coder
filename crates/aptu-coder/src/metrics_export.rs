// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Metrics file I/O: JSONL writing, rotation, cleanup, migration.
//!
//! Contains [`MetricsWriter`], the receiver half of the metrics channel that
//! drains events and appends them to daily-rotated JSONL files under the XDG
//! data directory. Also provides helper functions for file-level concerns:
//! path analysis, date arithmetic, legacy migration, and old-file cleanup.

use crate::metrics::{MetricEvent, MetricsLockGuard, ToolMetrics, record_otel_metrics};
use aptu_coder_core::lang::language_for_extension;
use fs2::FileExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

/// Receiver half of the metrics channel; drains events and writes them to daily-rotated JSONL files.
pub struct MetricsWriter {
    rx: tokio::sync::mpsc::UnboundedReceiver<MetricEvent>,
    base_dir: PathBuf,
    dir_created: bool,
}

impl MetricsWriter {
    pub fn new(
        rx: tokio::sync::mpsc::UnboundedReceiver<MetricEvent>,
        base_dir: Option<PathBuf>,
    ) -> Self {
        let dir = base_dir.unwrap_or_else(xdg_metrics_dir);
        Self {
            rx,
            base_dir: dir,
            dir_created: false,
        }
    }

    /// Accumulate per-tool event counts for session summary export on shutdown.
    fn accumulate_event(
        tool_counts: &mut std::collections::HashMap<&'static str, ToolMetrics>,
        export_session_id: &mut Option<String>,
        event: &MetricEvent,
    ) {
        let entry = tool_counts.entry(event.tool).or_default();
        entry.count += 1;
        entry.duration_ms += event.duration_ms;
        // output_chars is capped at 50 KB per stream (stdout + stderr each), so usize -> u64 is lossless.
        entry.output_chars += event.output_chars as u64;
        if export_session_id.is_none() {
            *export_session_id = event.session_id.clone();
        }
    }

    /// Write accumulated batch to file. Fire-and-forget semantics: errors are logged but not propagated.
    /// Acquires an exclusive advisory lock on a sibling .lock file before writing
    /// to prevent interleaving from concurrent processes writing to the same JSONL file.
    /// Lock acquisition failures degrade gracefully (warn and continue) per the
    /// non-blocking observability contract.
    async fn flush_batch(file: &mut tokio::fs::File, path: &Path, batch: Vec<MetricEvent>) {
        // Best-effort exclusive lock on sibling .lock file
        let _lock_guard = Self::acquire_metrics_lock(path).await;

        for event in batch {
            // Record to OTel metrics if available
            record_otel_metrics(&event);

            // Always write to JSONL as fallback
            if let Ok(mut json) = serde_json::to_string(&event) {
                json.push('\n');
                let _ = file.write_all(json.as_bytes()).await;
            }
        }
        let _ = file.flush().await;
    }

    /// Acquire an exclusive lock on a sibling .lock file for the metrics JSONL file.
    /// Returns a guard that releases the lock when dropped.
    /// On failure, logs a warning and returns None (degrade gracefully).
    async fn acquire_metrics_lock(path: &Path) -> Option<MetricsLockGuard> {
        let lock_path = format!("{}.lock", path.display());
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    lock_path = %lock_path,
                    "metrics: failed to open lock file; proceeding without lock"
                );
                return None;
            }
        };
        let result = tokio::task::spawn_blocking(move || file.lock_exclusive().map(|_| file)).await;
        match result {
            Ok(Ok(locked)) => Some(MetricsLockGuard(locked)),
            Ok(Err(e)) => {
                tracing::warn!(
                    error = %e,
                    "metrics: failed to acquire exclusive lock; proceeding without lock"
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "metrics: spawn_blocking panicked acquiring lock; proceeding without lock"
                );
                None
            }
        }
    }

    /// Check for date transition and rotate metrics file if needed.
    /// Returns the current file path and updates state if rotation occurred.
    fn rotate_metrics_file(
        base_dir: &std::path::Path,
        current_date: &mut String,
        current_file: &mut Option<PathBuf>,
        dir_created: &mut bool,
    ) -> PathBuf {
        let new_date = current_date_str();
        if new_date != *current_date {
            *current_date = new_date;
            *current_file = None;
            *dir_created = false;
        }

        current_file
            .get_or_insert_with(|| base_dir.join(format!("metrics-{}.jsonl", current_date)))
            .clone()
    }

    /// Receive and accumulate a batch of events from the channel.
    async fn receive_batch(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<MetricEvent>,
        tool_counts: &mut std::collections::HashMap<&'static str, ToolMetrics>,
        export_session_id: &mut Option<String>,
    ) -> Option<Vec<MetricEvent>> {
        let mut batch = Vec::new();
        if let Some(event) = rx.recv().await {
            Self::accumulate_event(tool_counts, export_session_id, &event);
            batch.push(event);
            for _ in 0..99 {
                match rx.try_recv() {
                    Ok(e) => {
                        Self::accumulate_event(tool_counts, export_session_id, &e);
                        batch.push(e);
                    }
                    Err(
                        mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected,
                    ) => break,
                }
            }
            Some(batch)
        } else {
            None
        }
    }

    /// Ensure metrics directory exists for the given path.
    async fn ensure_metrics_dir(path: &std::path::Path, dir_created: &mut bool) {
        if !*dir_created
            && let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            match tokio::fs::create_dir_all(parent).await {
                Ok(()) => {
                    *dir_created = true;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %parent.display(),
                        "metrics: failed to create directory; will retry next batch"
                    );
                }
            }
        }
    }

    pub async fn run(mut self) {
        cleanup_old_files(&self.base_dir).await;
        let mut current_date = current_date_str();
        let mut current_file: Option<PathBuf> = None;

        // Accumulate per-tool metrics for export on shutdown (issue #773)
        let mut tool_counts: std::collections::HashMap<&'static str, ToolMetrics> =
            std::collections::HashMap::new();
        let mut export_session_id: Option<String> = None;

        loop {
            let Some(batch) =
                Self::receive_batch(&mut self.rx, &mut tool_counts, &mut export_session_id).await
            else {
                break;
            };

            let path = Self::rotate_metrics_file(
                &self.base_dir,
                &mut current_date,
                &mut current_file,
                &mut self.dir_created,
            );

            Self::ensure_metrics_dir(&path, &mut self.dir_created).await;

            // Open file once per batch
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await;

            if let Ok(mut file) = file {
                Self::flush_batch(&mut file, &path, batch).await;
            }
        }

        // Export metrics summary on shutdown (issue #773)
        if let Ok(export_path) = std::env::var("APTU_CODER_METRICS_EXPORT_FILE") {
            if !std::path::Path::new(&export_path).is_absolute() {
                tracing::warn!(
                    path = %export_path,
                    "metrics: APTU_CODER_METRICS_EXPORT_FILE must be an absolute path; skipping export"
                );
            } else {
                let mut tool_calls = Vec::new();
                let mut total_duration_ms = 0u64;
                let mut total_output_chars_sum = 0u64;
                // Sort by tool name for deterministic JSON output
                let mut sorted_tools: Vec<_> = tool_counts.iter().collect();
                sorted_tools.sort_by_key(|&(name, _)| name);
                for (tool_name, metrics) in sorted_tools {
                    tool_calls.push(serde_json::json!({
                        "tool": tool_name,
                        "call_count": metrics.count,
                        "total_duration_ms": metrics.duration_ms,
                        "total_output_chars": metrics.output_chars
                    }));
                    total_duration_ms += metrics.duration_ms;
                    total_output_chars_sum += metrics.output_chars;
                }
                let summary = serde_json::json!({
                    "session_id": export_session_id.unwrap_or_default(),
                    "tool_calls": tool_calls,
                    "total_duration_ms": total_duration_ms,
                    "total_output_chars": total_output_chars_sum
                });
                if let Ok(json_str) = serde_json::to_string(&summary)
                    && let Err(e) = tokio::fs::write(&export_path, json_str).await
                {
                    tracing::warn!(
                        error = %e,
                        path = %export_path,
                        "metrics: failed to write export file"
                    );
                }
            }
        }
    }
}

/// Returns the current UNIX timestamp in milliseconds.
#[must_use]
pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Counts the number of path segments in a file path.
#[must_use]
pub(crate) fn path_component_count(path: &str) -> usize {
    Path::new(path).components().count()
}

/// Return the file extension for a path, normalized to lowercase.
///
/// - Returns `Some("rs")` for `src/main.rs`.
/// - Returns `Some("other")` for unrecognized extensions (not in the supported list).
/// - Returns `None` for paths with no extension or an empty extension.
#[must_use]
pub(crate) fn path_file_ext(file_path: &str) -> Option<&'static str> {
    let ext_os = Path::new(file_path).extension()?;
    let ext_str = ext_os.to_str()?;
    if ext_str.is_empty() {
        return None;
    }
    // language_for_extension does case-insensitive lookup; if found, return the
    // canonical (lowercased) extension key from EXTENSION_MAP via supported_extensions().
    if language_for_extension(ext_str).is_some() {
        aptu_coder_core::lang::supported_extensions()
            .into_iter()
            .find(|e| e.eq_ignore_ascii_case(ext_str))
    } else {
        Some("other")
    }
}

/// Derive a human-readable language name from a file path.
///
/// - Returns `Some("Rust")` for paths with a recognized extension.
/// - Returns `None` for paths with no extension or an unrecognized extension.
#[must_use]
pub(crate) fn path_language(path: &str) -> Option<String> {
    let ext_os = Path::new(path).extension()?;
    let ext_str = ext_os.to_str()?;
    if ext_str.is_empty() {
        return None;
    }
    language_for_extension(ext_str).map(std::borrow::ToOwned::to_owned)
}

fn xdg_metrics_dir() -> PathBuf {
    if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME")
        && !xdg_data_home.is_empty()
    {
        return PathBuf::from(xdg_data_home).join("aptu-coder");
    }

    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("aptu-coder")
    } else {
        PathBuf::from(".")
    }
}

async fn cleanup_old_files(base_dir: &Path) {
    let now_days = u32::try_from(unix_ms() / 86_400_000).unwrap_or(u32::MAX);

    let Ok(mut entries) = tokio::fs::read_dir(base_dir).await else {
        return;
    };

    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                let file_name = match path.file_name() {
                    Some(n) => n.to_string_lossy().into_owned(),
                    None => continue,
                };

                // Expected format: metrics-YYYY-MM-DD.jsonl
                if !file_name.starts_with("metrics-")
                    || std::path::Path::new(&*file_name)
                        .extension()
                        .is_none_or(|e| !e.eq_ignore_ascii_case("jsonl"))
                {
                    continue;
                }
                let date_part = &file_name[8..file_name.len() - 6];
                if date_part.len() != 10
                    || date_part.as_bytes().get(4) != Some(&b'-')
                    || date_part.as_bytes().get(7) != Some(&b'-')
                {
                    continue;
                }
                let Ok(year) = date_part[0..4].parse::<u32>() else {
                    continue;
                };
                let Ok(month) = date_part[5..7].parse::<u32>() else {
                    continue;
                };
                let Ok(day) = date_part[8..10].parse::<u32>() else {
                    continue;
                };
                if month == 0 || month > 12 || day == 0 || day > 31 {
                    continue;
                }

                let file_days = date_to_days_since_epoch(year, month, day);
                if now_days > file_days && (now_days - file_days) > 30 {
                    let _ = tokio::fs::remove_file(&path).await;
                    // Remove the sibling lock file created by acquire_metrics_lock.
                    let lock_path = format!("{}.lock", path.display());
                    let _ = tokio::fs::remove_file(&lock_path).await;
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!("error reading metrics directory entry: {e}");
            }
        }
    }
}

fn date_to_days_since_epoch(y: u32, m: u32, d: u32) -> u32 {
    // Shift year so March is month 0
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    // Compute the proleptic Gregorian day number, then subtract the Unix epoch offset.
    // The subtraction must wrap the full expression; applying .saturating_sub to `doe`
    // alone would underflow for recent dates where doe < 719_468.
    (era * 146_097 + doe).saturating_sub(719_468)
}

/// Returns the current UTC date as a string in YYYY-MM-DD format.
#[must_use]
pub(crate) fn current_date_str() -> String {
    let days = u32::try_from(unix_ms() / 86_400_000).unwrap_or(u32::MAX);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Migrate legacy metrics directory from `code-analyze-mcp` to `aptu-coder`.
///
/// - If the old directory exists and the new one does not, rename it and log info.
/// - If both exist, log a warning and do nothing.
/// - If neither exists, do nothing.
///
/// Returns `Ok(())` on success, propagating any I/O errors.
pub fn migrate_legacy_metrics_dir() -> std::io::Result<()> {
    let home =
        std::env::var("HOME").map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e))?;
    migrate_legacy_metrics_dir_impl(&home)
}

#[allow(dead_code)]
fn migrate_legacy_metrics_dir_impl(home: &str) -> std::io::Result<()> {
    let old_dir = PathBuf::from(home).join(".local/share/code-analyze-mcp");
    let new_dir = PathBuf::from(home).join(".local/share/aptu-coder");

    let old_exists = old_dir.is_dir();
    let new_exists = new_dir.is_dir();

    if old_exists && !new_exists {
        std::fs::rename(&old_dir, &new_dir)?;
        tracing::info!(
            "Migrated legacy metrics directory from {:?} to {:?}",
            old_dir,
            new_dir
        );
    } else if old_exists && new_exists {
        tracing::warn!("Both legacy and new metrics directories exist; not migrating");
    }
    // If old does not exist, nothing to do.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    /// Serializes tests that mutate `APTU_CODER_METRICS_EXPORT_FILE` to prevent parallel
    /// pollution. Recovers from poison caused by panicking tests.
    fn metrics_export_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let m = LOCK.get_or_init(|| Mutex::new(()));
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn test_migrate_legacy_only_old_exists() {
        // Arrange
        let tmp_home = TempDir::new().unwrap();
        let home_str = tmp_home.path().to_str().unwrap();
        let old_path = tmp_home.path().join(".local/share/code-analyze-mcp");
        let new_path = tmp_home.path().join(".local/share/aptu-coder");
        fs::create_dir_all(&old_path).unwrap();
        assert!(!new_path.exists());

        // Act
        let result = migrate_legacy_metrics_dir_impl(home_str);

        // Assert
        assert!(result.is_ok());
        assert!(!old_path.exists(), "old dir should be moved");
        assert!(new_path.is_dir(), "new dir should exist");
    }

    #[test]
    fn test_migrate_legacy_both_exist() {
        // Arrange
        let tmp_home = TempDir::new().unwrap();
        let home_str = tmp_home.path().to_str().unwrap();
        let old_path = tmp_home.path().join(".local/share/code-analyze-mcp");
        let new_path = tmp_home.path().join(".local/share/aptu-coder");
        fs::create_dir_all(&old_path).unwrap();
        fs::create_dir_all(&new_path).unwrap();

        // Act
        let result = migrate_legacy_metrics_dir_impl(home_str);

        // Assert
        assert!(result.is_ok());
        assert!(old_path.is_dir(), "old dir should remain");
        assert!(new_path.is_dir(), "new dir should remain");
    }

    #[test]
    fn test_migrate_legacy_neither_exists() {
        // Arrange
        let tmp_home = TempDir::new().unwrap();
        let home_str = tmp_home.path().to_str().unwrap();
        let old_path = tmp_home.path().join(".local/share/code-analyze-mcp");
        let new_path = tmp_home.path().join(".local/share/aptu-coder");

        // Act
        let result = migrate_legacy_metrics_dir_impl(home_str);

        // Assert
        assert!(result.is_ok());
        assert!(!old_path.exists(), "old dir should not exist");
        assert!(!new_path.exists(), "new dir should not exist");
    }

    #[test]
    fn test_date_to_days_since_epoch_known_dates() {
        assert_eq!(date_to_days_since_epoch(1970, 1, 1), 0);
        assert_eq!(date_to_days_since_epoch(2020, 1, 1), 18_262);
        assert_eq!(date_to_days_since_epoch(2000, 2, 29), 11_016);
    }

    #[test]
    fn test_current_date_str_format() {
        let s = current_date_str();
        assert_eq!(s.len(), 10);
        assert_eq!(s.as_bytes()[4], b'-');
        assert_eq!(s.as_bytes()[7], b'-');
        let year: u32 = s[0..4].parse().expect("year must be numeric");
        assert!(year >= 2020 && year <= 2100);
    }

    #[tokio::test]
    async fn test_metrics_writer_batching() {
        let dir = TempDir::new().unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
        let writer = MetricsWriter::new(rx, Some(dir.path().to_path_buf()));
        let make_event = || MetricEvent {
            ts: unix_ms(),
            tool: "analyze_directory",
            duration_ms: 1,
            output_chars: 10,
            param_path_depth: 1,
            max_depth: None,
            result: "ok",
            error_type: None,
            error_subtype: None,
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
        tx.send(make_event()).unwrap();
        tx.send(make_event()).unwrap();
        tx.send(make_event()).unwrap();
        drop(tx);
        writer.run().await;
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("jsonl"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(entries.len(), 1);
        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
    }

    #[tokio::test]
    async fn test_cleanup_old_files_deletes_old_keeps_recent() {
        let dir = TempDir::new().unwrap();
        let old_file = dir.path().join("metrics-1970-01-01.jsonl");
        let today = current_date_str();
        let recent_file = dir.path().join(format!("metrics-{}.jsonl", today));
        std::fs::write(&old_file, "old\n").unwrap();
        std::fs::write(&recent_file, "recent\n").unwrap();
        cleanup_old_files(dir.path()).await;
        assert!(!old_file.exists());
        assert!(recent_file.exists());
    }

    #[test]
    fn test_path_file_ext_known() {
        // Arrange / Act / Assert: known extension returns the lowercased extension key
        assert_eq!(path_file_ext("src/main.rs"), Some("rs"));
    }

    #[test]
    fn test_path_file_ext_unknown() {
        // Arrange / Act / Assert: unrecognized extension returns Some("other")
        assert_eq!(path_file_ext("file.xyz"), Some("other"));
    }

    #[test]
    fn test_path_file_ext_no_ext() {
        // Arrange / Act / Assert: path with no extension returns None
        assert_eq!(path_file_ext("Makefile"), None);
    }

    #[test]
    fn test_path_file_ext_case_insensitive() {
        // Arrange / Act / Assert: uppercase extension is normalized to lowercase key
        assert_eq!(path_file_ext("src/main.RS"), Some("rs"));
    }

    #[test]
    fn test_path_file_ext_multi_dot() {
        // Arrange / Act / Assert: multi-dot filename uses the last extension
        assert_eq!(path_file_ext("file.test.rs"), Some("rs"));
    }

    #[test]
    fn test_path_language_known_ext() {
        // Arrange / Act / Assert: known extension returns Some(language name)
        assert_eq!(path_language("src/main.rs"), Some("rust".to_string()));
    }

    #[test]
    fn test_path_language_unknown_ext() {
        // Arrange / Act / Assert: unknown extension returns None
        assert_eq!(path_language("file.xyz"), None);
    }

    #[test]
    fn test_path_language_no_ext() {
        // Arrange / Act / Assert: path without extension returns None
        assert_eq!(path_language("Makefile"), None);
    }

    #[tokio::test]
    async fn test_metrics_export_file_created() {
        let _guard = metrics_export_lock();
        // Arrange: create temp dir and set export env var
        let dir = TempDir::new().unwrap();
        let export_file = dir.path().join("metrics_export.json");
        let export_path = export_file.to_str().unwrap().to_string();
        unsafe {
            std::env::set_var("APTU_CODER_METRICS_EXPORT_FILE", &export_path);
        }

        // Act: run writer with a couple of events and drop the sender to trigger shutdown
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
        let writer = MetricsWriter::new(rx, Some(dir.path().to_path_buf()));

        let make_event = || MetricEvent {
            ts: unix_ms(),
            tool: "analyze_directory",
            duration_ms: 1,
            output_chars: 10,
            param_path_depth: 1,
            max_depth: None,
            result: "ok",
            error_type: None,
            error_subtype: None,
            session_id: Some("test-session-1".to_string()),
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

        tx.send(make_event()).unwrap();
        tx.send(make_event()).unwrap();
        drop(tx);
        writer.run().await;

        // Assert: export file was created with JSON content
        assert!(
            export_file.exists(),
            "export file should exist at {}",
            export_path
        );
        let content = std::fs::read_to_string(&export_file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["session_id"], "test-session-1");
        assert!(parsed["total_duration_ms"].as_u64().unwrap() >= 2);
        assert_eq!(parsed["tool_calls"][0]["tool"], "analyze_directory");
        assert_eq!(parsed["tool_calls"][0]["call_count"], 2);

        // Cleanup
        unsafe {
            std::env::remove_var("APTU_CODER_METRICS_EXPORT_FILE");
        }
    }

    #[tokio::test]
    async fn test_metrics_export_env_var_unset() {
        let _guard = metrics_export_lock();
        // Edge case: no APTU_CODER_METRICS_EXPORT_FILE -> no export file written
        let dir = TempDir::new().unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
        let writer = MetricsWriter::new(rx, Some(dir.path().to_path_buf()));

        let make_event = || MetricEvent {
            ts: unix_ms(),
            tool: "analyze_directory",
            duration_ms: 1,
            output_chars: 10,
            param_path_depth: 1,
            max_depth: None,
            result: "ok",
            error_type: None,
            error_subtype: None,
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

        tx.send(make_event()).unwrap();
        drop(tx);
        writer.run().await;

        // No export file should exist in the dir
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.contains("metrics.json"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(entries.len(), 0, "no export file should be created");
    }

    #[tokio::test]
    async fn test_metrics_export_relative_path_rejected() {
        let _guard = metrics_export_lock();
        // Edge case: relative path in APTU_CODER_METRICS_EXPORT_FILE -> warning, no file
        let dir = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("APTU_CODER_METRICS_EXPORT_FILE", "relative/export.json");
        }

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
        let writer = MetricsWriter::new(rx, Some(dir.path().to_path_buf()));

        let make_event = || MetricEvent {
            ts: unix_ms(),
            tool: "analyze_file",
            duration_ms: 1,
            output_chars: 10,
            param_path_depth: 1,
            max_depth: None,
            result: "ok",
            error_type: None,
            error_subtype: None,
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

        tx.send(make_event()).unwrap();
        drop(tx);
        writer.run().await;

        // No export file should be created for relative path
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.contains("metrics.json"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(
            entries.len(),
            0,
            "no export file should be created for relative path"
        );

        // Cleanup
        unsafe {
            std::env::remove_var("APTU_CODER_METRICS_EXPORT_FILE");
        }
    }

    #[tokio::test]
    async fn test_lock_file_created() {
        // Assert: lock file is created next to JSONL file with deterministic name
        let dir = TempDir::new().unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
        let writer = MetricsWriter::new(rx, Some(dir.path().to_path_buf()));
        let make_event = || MetricEvent {
            ts: unix_ms(),
            tool: "analyze_directory",
            duration_ms: 1,
            output_chars: 10,
            param_path_depth: 1,
            max_depth: None,
            result: "ok",
            error_type: None,
            error_subtype: None,
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
        tx.send(make_event()).unwrap();
        drop(tx);
        writer.run().await;

        // Check that a .lock file exists next to the JSONL file
        let jsonl_entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("jsonl"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(jsonl_entries.len(), 1);
        let lock_path = format!("{}.lock", jsonl_entries[0].path().display());
        assert!(
            std::path::Path::new(&lock_path).exists(),
            "lock file must exist next to JSONL file"
        );
    }

    #[tokio::test]
    async fn test_flush_batch_concurrent_writes() {
        // Edge case: two writers writing to the same metrics directory
        // should both complete without panic (advisory lock protects against corruption).
        let dir = TempDir::new().unwrap();
        let base = dir.path().to_path_buf();

        // Writer 1
        let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
        let writer1 = MetricsWriter::new(rx1, Some(base.clone()));
        let make_event = || MetricEvent {
            ts: unix_ms(),
            tool: "analyze_directory",
            duration_ms: 1,
            output_chars: 10,
            param_path_depth: 1,
            max_depth: None,
            result: "ok",
            error_type: None,
            error_subtype: None,
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
        tx1.send(make_event()).unwrap();
        tx1.send(make_event()).unwrap();
        drop(tx1);

        // Writer 2
        let (tx2, rx2) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
        let writer2 = MetricsWriter::new(rx2, Some(base));
        tx2.send(make_event()).unwrap();
        tx2.send(make_event()).unwrap();
        drop(tx2);

        // Run both writers concurrently
        let h1 = tokio::spawn(writer1.run());
        let h2 = tokio::spawn(writer2.run());
        let (r1, r2) = tokio::join!(h1, h2);
        r1.unwrap();
        r2.unwrap();

        // Both writers succeeded; verify the JSONL file has all 4 events
        let jsonl_entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("jsonl"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(jsonl_entries.len(), 1);
        let content = std::fs::read_to_string(jsonl_entries[0].path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 4, "expected 4 JSONL lines from 2 writers");
    }
}
