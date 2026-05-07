// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Metrics collection and daily-rotating JSONL emission.
//!
//! Provides a channel-based pipeline: callers emit [`MetricEvent`] values via [`MetricsSender`],
//! and [`MetricsWriter`] drains the channel and appends events to a daily-rotated JSONL file
//! under the XDG data directory (`~/.local/share/aptu-coder/metrics-YYYY-MM-DD.jsonl`).
//! Files older than 30 days are deleted on startup.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

/// A single metric event emitted by a tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricEvent {
    pub ts: u64,
    pub tool: &'static str,
    pub duration_ms: u64,
    pub output_chars: usize,
    pub param_path_depth: usize,
    pub max_depth: Option<u32>,
    pub result: &'static str,
    pub error_type: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub seq: Option<u32>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_hit: Option<bool>,
}

/// Sender half of the metrics channel; cloned and passed to tools for event emission.
#[derive(Clone)]
pub struct MetricsSender(pub tokio::sync::mpsc::UnboundedSender<MetricEvent>);

impl MetricsSender {
    pub fn send(&self, event: MetricEvent) {
        let _ = self.0.send(event);
    }
}

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

    pub async fn run(mut self) {
        cleanup_old_files(&self.base_dir).await;
        let mut current_date = current_date_str();
        let mut current_file: Option<PathBuf> = None;

        // Accumulate per-tool metrics for export on shutdown (issue #773)
        let mut tool_counts: std::collections::HashMap<String, (u64, u64)> =
            std::collections::HashMap::new();
        let mut export_session_id: Option<String> = None;

        loop {
            let mut batch = Vec::new();
            if let Some(event) = self.rx.recv().await {
                // Accumulate metrics for export
                let entry = tool_counts.entry(event.tool.to_string()).or_insert((0, 0));
                entry.0 += 1;
                entry.1 += event.duration_ms;
                if export_session_id.is_none() {
                    export_session_id = event.session_id.clone();
                }

                batch.push(event);
                for _ in 0..99 {
                    match self.rx.try_recv() {
                        Ok(e) => {
                            // Accumulate metrics for export
                            let entry = tool_counts.entry(e.tool.to_string()).or_insert((0, 0));
                            entry.0 += 1;
                            entry.1 += e.duration_ms;
                            if export_session_id.is_none() {
                                export_session_id = e.session_id.clone();
                            }

                            batch.push(e);
                        }
                        Err(
                            mpsc::error::TryRecvError::Empty
                            | mpsc::error::TryRecvError::Disconnected,
                        ) => break,
                    }
                }
            } else {
                break;
            }

            let new_date = current_date_str();
            if new_date != current_date {
                current_date = new_date;
                current_file = None;
                self.dir_created = false;
            }

            if current_file.is_none() {
                current_file = Some(
                    self.base_dir
                        .join(format!("metrics-{}.jsonl", current_date)),
                );
            }

            let path = current_file.as_ref().unwrap();

            // Create directory once per day
            if !self.dir_created
                && let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                match tokio::fs::create_dir_all(parent).await {
                    Ok(()) => {
                        self.dir_created = true;
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

            // Open file once per batch
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await;

            if let Ok(mut file) = file {
                for event in batch {
                    if let Ok(mut json) = serde_json::to_string(&event) {
                        json.push('\n');
                        let _ = file.write_all(json.as_bytes()).await;
                    }
                }
                let _ = file.flush().await;
            }
        }

        // Export metrics summary on shutdown (issue #773)
        if let Ok(export_path) = std::env::var("APTU_CODER_METRICS_EXPORT_FILE") {
            let mut tool_calls = Vec::new();
            let mut total_duration_ms = 0u64;
            for (tool_name, (count, duration)) in tool_counts {
                tool_calls.push(serde_json::json!({
                    "tool": tool_name,
                    "call_count": count,
                    "total_duration_ms": duration
                }));
                total_duration_ms += duration;
            }
            let summary = serde_json::json!({
                "session_id": export_session_id.unwrap_or_default(),
                "tool_calls": tool_calls,
                "total_duration_ms": total_duration_ms
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

/// Returns the current UNIX timestamp in milliseconds.
#[must_use]
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Counts the number of path segments in a file path.
#[must_use]
pub fn path_component_count(path: &str) -> usize {
    Path::new(path).components().count()
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
pub fn current_date_str() -> String {
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
            session_id: None,
            seq: None,
            cache_hit: None,
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
    fn test_metric_event_serialization() {
        let event = MetricEvent {
            ts: 1_700_000_000_000,
            tool: "analyze_directory",
            duration_ms: 42,
            output_chars: 100,
            param_path_depth: 3,
            max_depth: Some(2),
            result: "ok",
            error_type: None,
            session_id: None,
            seq: None,
            cache_hit: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("analyze_directory"));
        assert!(json.contains(r#""result":"ok""#));
        assert!(json.contains(r#""output_chars":100"#));
    }

    #[test]
    fn test_metric_event_serialization_error() {
        let event = MetricEvent {
            ts: 1_700_000_000_000,
            tool: "analyze_directory",
            duration_ms: 5,
            output_chars: 0,
            param_path_depth: 3,
            max_depth: Some(3),
            result: "error",
            error_type: Some("invalid_params".to_string()),
            session_id: None,
            seq: None,
            cache_hit: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""result":"error""#));
        assert!(json.contains(r#""error_type":"invalid_params""#));
        assert!(json.contains(r#""output_chars":0"#));
        assert!(json.contains(r#""max_depth":3"#));
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
            session_id: Some("1742468880123-42".to_string()),
            seq: Some(5),
            cache_hit: None,
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let json_str = r#"{"ts":1700000000000,"tool":"analyze_file","duration_ms":100,"output_chars":500,"param_path_depth":2,"max_depth":3,"result":"ok","error_type":null,"session_id":"1742468880123-42","seq":5}"#;
        assert_eq!(serialized, json_str);
    }

    #[tokio::test]
    async fn test_metrics_export_file_created() {
        let _guard = metrics_export_lock();
        // Arrange: create temp dir and set export env var
        let dir = TempDir::new().unwrap();
        let export_file = dir.path().join("metrics_export.json");
        let export_path_str = export_file.to_string_lossy().to_string();

        unsafe {
            std::env::set_var("APTU_CODER_METRICS_EXPORT_FILE", &export_path_str);
        }

        // Create metrics writer and send events
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
        let writer = MetricsWriter::new(rx, Some(dir.path().to_path_buf()));

        // Act: send a few events with session_id
        tx.send(MetricEvent {
            ts: unix_ms(),
            tool: "analyze_directory",
            duration_ms: 100,
            output_chars: 50,
            param_path_depth: 1,
            max_depth: None,
            result: "ok",
            error_type: None,
            session_id: Some("test-session-123".to_string()),
            seq: None,
            cache_hit: None,
        })
        .unwrap();
        tx.send(MetricEvent {
            ts: unix_ms(),
            tool: "analyze_file",
            duration_ms: 50,
            output_chars: 100,
            param_path_depth: 2,
            max_depth: Some(3),
            result: "ok",
            error_type: None,
            session_id: Some("test-session-123".to_string()),
            seq: None,
            cache_hit: None,
        })
        .unwrap();
        drop(tx);
        writer.run().await;

        // Assert: export file should exist with correct JSON structure
        assert!(
            export_file.exists(),
            "export file should be created at {:?}",
            export_file
        );
        let content = std::fs::read_to_string(&export_file).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            json["session_id"], "test-session-123",
            "export should contain correct session_id"
        );
        assert!(
            json["tool_calls"].is_array(),
            "export should contain tool_calls array"
        );
        let tool_calls = json["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 2, "should have 2 tool calls");
        assert!(
            json["total_duration_ms"].is_number(),
            "export should contain total_duration_ms"
        );
        assert_eq!(
            json["total_duration_ms"], 150,
            "total_duration_ms should be sum of all durations"
        );

        // Cleanup
        unsafe {
            std::env::remove_var("APTU_CODER_METRICS_EXPORT_FILE");
        }
    }

    #[tokio::test]
    async fn test_metrics_export_env_var_unset() {
        let _guard = metrics_export_lock();
        // Arrange: ensure env var is not set
        unsafe {
            std::env::remove_var("APTU_CODER_METRICS_EXPORT_FILE");
        }
        let dir = TempDir::new().unwrap();
        // Use a unique marker to ensure we don't pick up files from other tests
        let marker = "metrics_export_unset_test";

        // Create metrics writer and send events
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<MetricEvent>();
        let writer = MetricsWriter::new(rx, Some(dir.path().to_path_buf()));

        // Act: send events and run writer
        tx.send(MetricEvent {
            ts: unix_ms(),
            tool: "analyze_directory",
            duration_ms: 100,
            output_chars: 50,
            param_path_depth: 1,
            max_depth: None,
            result: "ok",
            error_type: None,
            session_id: Some("test-session-456".to_string()),
            seq: None,
            cache_hit: None,
        })
        .unwrap();
        drop(tx);
        writer.run().await;

        // Assert: no export file should be created
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.contains(marker))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(
            entries.len(),
            0,
            "no export file should be created when env var is unset"
        );
    }
}
