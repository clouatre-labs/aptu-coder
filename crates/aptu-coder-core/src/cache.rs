// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! LRU cache for analysis results indexed by path, modification time, and mode.
//!
//! Provides thread-safe, capacity-bounded caching of file analysis outputs using LRU eviction.
//! Recovers gracefully from poisoned mutex conditions.

use crate::analyze::{AnalysisOutput, FileAnalysisOutput};
use crate::traversal::WalkEntry;
use crate::types::AnalysisMode;
use lru::LruCache;
use rayon::prelude::*;
use serde::{Serialize, de::DeserializeOwned};
use std::num::NonZeroUsize;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tempfile::NamedTempFile;
use tracing::{debug, error, instrument, warn};

/// Indicates which cache tier served the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheTier {
    L1Memory,
    L2Disk,
    Miss,
}

impl CacheTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            CacheTier::L1Memory => "l1_memory",
            CacheTier::L2Disk => "l2_disk",
            CacheTier::Miss => "miss",
        }
    }
}

/// Cache key combining path, modification time, and analysis mode.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct CacheKey {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub mode: AnalysisMode,
}

/// Cache key for directory analysis combining file mtimes, mode, and `max_depth`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct DirectoryCacheKey {
    files: Vec<(PathBuf, SystemTime)>,
    mode: AnalysisMode,
    max_depth: Option<u32>,
    git_ref: Option<String>,
}

impl DirectoryCacheKey {
    /// Build a cache key from walk entries, capturing mtime for each file.
    /// Files are sorted by path for deterministic hashing.
    /// Directories are filtered out; only file entries are processed.
    /// Metadata collection is parallelized using rayon.
    /// The `git_ref` is included so that filtered and unfiltered results have distinct keys.
    #[must_use]
    pub fn from_entries(
        entries: &[WalkEntry],
        max_depth: Option<u32>,
        mode: AnalysisMode,
        git_ref: Option<&str>,
    ) -> Self {
        let mut files: Vec<(PathBuf, SystemTime)> = entries
            .par_iter()
            .filter(|e| !e.is_dir)
            .map(|e| {
                let mtime = e.mtime.unwrap_or(SystemTime::UNIX_EPOCH);
                (e.path.clone(), mtime)
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            files,
            mode,
            max_depth,
            git_ref: git_ref.map(ToOwned::to_owned),
        }
    }
}

/// Recover from a poisoned mutex by clearing the cache.
/// On poison, creates a new empty cache and returns the recovery value.
fn lock_or_recover<K, V, T, F>(mutex: &Mutex<LruCache<K, V>>, capacity: usize, recovery: F) -> T
where
    K: std::hash::Hash + Eq,
    F: FnOnce(&mut LruCache<K, V>) -> T,
{
    match mutex.lock() {
        Ok(mut guard) => recovery(&mut guard),
        Err(poisoned) => {
            // SAFETY: 100 is a non-zero literal
            let cache_size = NonZeroUsize::new(capacity)
                .unwrap_or_else(|| unsafe { NonZeroUsize::new_unchecked(100) });
            let new_cache = LruCache::new(cache_size);
            let mut guard = poisoned.into_inner();
            *guard = new_cache;
            recovery(&mut guard)
        }
    }
}

/// LRU cache for file analysis results with mutex protection.
pub struct AnalysisCache {
    file_capacity: usize,
    dir_capacity: usize,
    cache: Arc<Mutex<LruCache<CacheKey, Arc<FileAnalysisOutput>>>>,
    directory_cache: Arc<Mutex<LruCache<DirectoryCacheKey, Arc<AnalysisOutput>>>>,
}

impl AnalysisCache {
    /// Create a new cache with the specified file capacity.
    /// The directory cache capacity is read from the `APTU_CODER_DIR_CACHE_CAPACITY`
    /// environment variable (default: 20).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let file_capacity = capacity.max(1);
        let dir_capacity: usize = std::env::var("APTU_CODER_DIR_CACHE_CAPACITY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        let dir_capacity = dir_capacity.max(1);
        // SAFETY: file_capacity is >= 1 due to .max(1) applied above
        let cache_size = unsafe { NonZeroUsize::new_unchecked(file_capacity) };
        // SAFETY: dir_capacity is >= 1 due to .max(1) applied above
        let dir_cache_size = unsafe { NonZeroUsize::new_unchecked(dir_capacity) };
        Self {
            file_capacity,
            dir_capacity,
            cache: Arc::new(Mutex::new(LruCache::new(cache_size))),
            directory_cache: Arc::new(Mutex::new(LruCache::new(dir_cache_size))),
        }
    }

    /// Get a cached analysis result if it exists.
    #[instrument(skip(self), fields(path = ?key.path))]
    pub fn get(&self, key: &CacheKey) -> Option<Arc<FileAnalysisOutput>> {
        lock_or_recover(&self.cache, self.file_capacity, |guard| {
            let result = guard.get(key).cloned();
            let cache_size = guard.len();
            if let Some(v) = result {
                debug!(cache_event = "hit", cache_size = cache_size, path = ?key.path);
                Some(v)
            } else {
                debug!(cache_event = "miss", cache_size = cache_size, path = ?key.path);
                None
            }
        })
    }

    /// Store an analysis result in the cache.
    #[instrument(skip(self, value), fields(path = ?key.path))]
    // public API; callers expect owned semantics
    #[allow(clippy::needless_pass_by_value)]
    pub fn put(&self, key: CacheKey, value: Arc<FileAnalysisOutput>) {
        lock_or_recover(&self.cache, self.file_capacity, |guard| {
            let push_result = guard.push(key.clone(), value);
            let cache_size = guard.len();
            match push_result {
                None => {
                    debug!(cache_event = "insert", cache_size = cache_size, path = ?key.path);
                }
                Some((returned_key, _)) => {
                    if returned_key == key {
                        debug!(cache_event = "update", cache_size = cache_size, path = ?key.path);
                    } else {
                        debug!(cache_event = "eviction", cache_size = cache_size, path = ?key.path, evicted_path = ?returned_key.path);
                    }
                }
            }
        });
    }

    /// Get a cached directory analysis result if it exists.
    #[instrument(skip(self))]
    pub fn get_directory(&self, key: &DirectoryCacheKey) -> Option<Arc<AnalysisOutput>> {
        lock_or_recover(&self.directory_cache, self.dir_capacity, |guard| {
            let result = guard.get(key).cloned();
            let cache_size = guard.len();
            if let Some(v) = result {
                debug!(cache_event = "hit", cache_size = cache_size);
                Some(v)
            } else {
                debug!(cache_event = "miss", cache_size = cache_size);
                None
            }
        })
    }

    /// Store a directory analysis result in the cache.
    #[instrument(skip(self, value))]
    pub fn put_directory(&self, key: DirectoryCacheKey, value: Arc<AnalysisOutput>) {
        lock_or_recover(&self.directory_cache, self.dir_capacity, |guard| {
            let push_result = guard.push(key, value);
            let cache_size = guard.len();
            match push_result {
                None => {
                    debug!(cache_event = "insert", cache_size = cache_size);
                }
                Some((_, _)) => {
                    debug!(cache_event = "eviction", cache_size = cache_size);
                }
            }
        });
    }

    /// Returns the configured file-cache capacity.
    /// Exposed for testing across crate boundaries; not part of the stable API.
    #[doc(hidden)]
    pub fn file_capacity(&self) -> usize {
        self.file_capacity
    }

    /// Invalidate all cache entries for a given file path.
    /// Removes all entries regardless of modification time or analysis mode.
    #[instrument(skip(self), fields(path = ?path))]
    pub fn invalidate_file(&self, path: &std::path::Path) {
        lock_or_recover(&self.cache, self.file_capacity, |guard| {
            let keys: Vec<CacheKey> = guard
                .iter()
                .filter(|(k, _)| k.path == path)
                .map(|(k, _)| k.clone())
                .collect();
            for key in keys {
                guard.pop(&key);
            }
            let cache_size = guard.len();
            debug!(cache_event = "invalidate_file", cache_size = cache_size, path = ?path);
        });
    }
}

impl Clone for AnalysisCache {
    fn clone(&self) -> Self {
        Self {
            file_capacity: self.file_capacity,
            dir_capacity: self.dir_capacity,
            cache: Arc::clone(&self.cache),
            directory_cache: Arc::clone(&self.directory_cache),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SemanticAnalysis;

    #[test]
    fn test_from_entries_skips_dirs() {
        // Arrange: create a real temp dir and a real temp file for hermetic isolation.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = tempfile::NamedTempFile::new_in(dir.path()).expect("tempfile");
        let file_path = file.path().to_path_buf();

        let entries = vec![
            WalkEntry {
                path: dir.path().to_path_buf(),
                depth: 0,
                is_dir: true,
                is_symlink: false,
                symlink_target: None,
                mtime: None,
                canonical_path: PathBuf::new(),
            },
            WalkEntry {
                path: file_path.clone(),
                depth: 0,
                is_dir: false,
                is_symlink: false,
                symlink_target: None,
                mtime: None,
                canonical_path: PathBuf::new(),
            },
        ];

        // Act: build cache key from entries
        let key = DirectoryCacheKey::from_entries(&entries, None, AnalysisMode::Overview, None);

        // Assert: only the file entry should be in the cache key
        // The directory entry should be filtered out
        assert_eq!(key.files.len(), 1);
        assert_eq!(key.files[0].0, file_path);
    }

    #[test]
    fn test_invalidate_file_single_mode() {
        // Arrange: create a cache and insert one entry for a path
        let cache = AnalysisCache::new(10);
        let path = PathBuf::from("/test/file.rs");
        let key = CacheKey {
            path: path.clone(),
            modified: SystemTime::UNIX_EPOCH,
            mode: AnalysisMode::Overview,
        };
        let output = Arc::new(FileAnalysisOutput::new(
            String::new(),
            SemanticAnalysis::default(),
            0,
            None,
        ));
        cache.put(key.clone(), output);

        // Act: invalidate the file
        cache.invalidate_file(&path);

        // Assert: the entry should be removed
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn test_invalidate_file_multi_mode() {
        // Arrange: create a cache and insert two entries for the same path with different modes
        let cache = AnalysisCache::new(10);
        let path = PathBuf::from("/test/file.rs");
        let key1 = CacheKey {
            path: path.clone(),
            modified: SystemTime::UNIX_EPOCH,
            mode: AnalysisMode::Overview,
        };
        let key2 = CacheKey {
            path: path.clone(),
            modified: SystemTime::UNIX_EPOCH,
            mode: AnalysisMode::FileDetails,
        };
        let output = Arc::new(FileAnalysisOutput::new(
            String::new(),
            SemanticAnalysis::default(),
            0,
            None,
        ));
        cache.put(key1.clone(), output.clone());
        cache.put(key2.clone(), output);

        // Act: invalidate the file
        cache.invalidate_file(&path);

        // Assert: both entries should be removed
        assert!(cache.get(&key1).is_none());
        assert!(cache.get(&key2).is_none());
    }

    // Mutex serialises the two dir-cache-capacity tests to prevent env var races.
    static DIR_CACHE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_dir_cache_capacity_default() {
        let _guard = DIR_CACHE_ENV_LOCK.lock().unwrap();

        // Arrange: ensure the env var is not set
        unsafe { std::env::remove_var("APTU_CODER_DIR_CACHE_CAPACITY") };

        // Act
        let cache = AnalysisCache::new(100);

        // Assert: default dir capacity is 20
        assert_eq!(cache.dir_capacity, 20);
    }

    #[test]
    fn test_dir_cache_capacity_from_env() {
        let _guard = DIR_CACHE_ENV_LOCK.lock().unwrap();

        // Arrange
        unsafe { std::env::set_var("APTU_CODER_DIR_CACHE_CAPACITY", "7") };

        // Act
        let cache = AnalysisCache::new(100);

        // Cleanup before assertions to minimise env pollution window
        unsafe { std::env::remove_var("APTU_CODER_DIR_CACHE_CAPACITY") };

        // Assert
        assert_eq!(cache.dir_capacity, 7);
    }
}

/// Persistent content-addressable disk cache for analyze_* tools.
/// All methods are infallible from the caller's perspective: errors are silently dropped.
/// Number of consecutive L2 write failures that triggers an `error!` log escalation.
/// Below this threshold each failure logs at `warn!`. At or above it the cache is
/// considered degraded and a single `error!` is emitted so operators are alerted
/// without flooding logs on a sustained disk-full condition.
const DISK_CACHE_DEGRADED_THRESHOLD: u64 = 3;

pub struct DiskCache {
    base: std::path::PathBuf,
    disabled: bool,
    /// Counts write failures since last drain. Incremented inside `put` on any I/O error.
    write_failures: std::sync::atomic::AtomicU64,
    /// Cumulative write failures across all drains. Never reset; used for threshold checks.
    total_write_failures: std::sync::atomic::AtomicU64,
}

impl DiskCache {
    /// Returns the number of write failures accumulated since the last call and resets the
    /// per-drain counter. The cumulative `total_write_failures` is never reset.
    pub fn drain_write_failures(&self) -> u64 {
        self.write_failures
            .swap(0, std::sync::atomic::Ordering::Relaxed)
    }

    /// Returns true when cumulative write failures have reached `DISK_CACHE_DEGRADED_THRESHOLD`.
    /// Callers can use this to emit a degraded health signal without polling the counter.
    pub fn is_degraded(&self) -> bool {
        self.total_write_failures
            .load(std::sync::atomic::Ordering::Relaxed)
            >= DISK_CACHE_DEGRADED_THRESHOLD
    }
}

impl DiskCache {
    /// Creates the cache directory (mode 0700) and returns a new instance.
    /// If `disabled` is true, or if directory creation fails, all operations are no-ops.
    pub fn new(base: std::path::PathBuf, disabled: bool) -> Self {
        if disabled {
            return Self {
                base,
                disabled: true,
                write_failures: std::sync::atomic::AtomicU64::new(0),
                total_write_failures: std::sync::atomic::AtomicU64::new(0),
            };
        }
        if let Err(e) = std::fs::create_dir_all(&base) {
            warn!(path = %base.display(), error = %e, "disk cache disabled: failed to create cache directory");
            return Self {
                base,
                disabled: true,
                write_failures: std::sync::atomic::AtomicU64::new(0),
                total_write_failures: std::sync::atomic::AtomicU64::new(0),
            };
        }
        if let Err(e) = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)) {
            warn!(path = %base.display(), error = %e, "disk cache: failed to set directory permissions to 0700");
        }
        Self {
            base,
            disabled: false,
            write_failures: std::sync::atomic::AtomicU64::new(0),
            total_write_failures: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn entry_path(&self, tool: &str, key: &blake3::Hash) -> std::path::PathBuf {
        let hex = format!("{}", key);
        self.base
            .join(tool)
            .join(&hex[..2])
            .join(format!("{}.json.snap", hex))
    }

    /// Returns None if entry is absent or corrupt. Never propagates errors.
    pub fn get<T: DeserializeOwned>(&self, tool: &str, key: &blake3::Hash) -> Option<T> {
        if self.disabled {
            return None;
        }
        let path = self.entry_path(tool, key);
        let compressed = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return None,
        };
        let bytes = match snap::raw::Decoder::new().decompress_vec(&compressed) {
            Ok(b) => b,
            Err(e) => {
                debug!(tool, error = %e, "disk cache decompression failed");
                return None;
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(v) => Some(v),
            Err(e) => {
                debug!(tool, error = %e, "disk cache deserialization failed");
                None
            }
        }
    }

    /// Serialize and compress a value. Returns None if serialization or compression fails.
    fn serialize_entry<T: Serialize>(value: &T) -> Option<Vec<u8>> {
        let bytes = serde_json::to_vec(value).ok()?;
        snap::raw::Encoder::new().compress_vec(&bytes).ok()
    }

    /// Write compressed data to a temporary file and atomically rename it to the target path.
    /// Returns Err if any step fails; caller silently drops the error.
    fn write_entry_atomically(
        dir: &std::path::Path,
        path: &std::path::Path,
        compressed: &[u8],
    ) -> Result<(), std::io::Error> {
        use std::io::Write;
        let mut tmp = NamedTempFile::new_in(dir)?;
        tmp.write_all(compressed)?;
        tmp.persist(path).map(|_| ()).map_err(|e| e.error)
    }

    /// Atomic write via NamedTempFile::persist (rename(2)). Silently drops all errors.
    pub fn put<T: Serialize>(&self, tool: &str, key: &blake3::Hash, value: &T) {
        if self.disabled {
            return;
        }
        let path = self.entry_path(tool, key);
        let dir = match path.parent() {
            Some(d) => d.to_path_buf(),
            None => return,
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(tool, error = %e, "disk cache: failed to create cache directory");
            self.record_write_failure();
            return;
        }
        let compressed = match Self::serialize_entry(value) {
            Some(c) => c,
            None => return,
        };
        if Self::write_entry_atomically(&dir, &path, &compressed)
            .ok()
            .is_none()
        {
            self.record_write_failure();
        }
    }

    /// Increments both the per-drain and cumulative failure counters. Escalates to `error!`
    /// once cumulative failures reach `DISK_CACHE_DEGRADED_THRESHOLD` so a sustained
    /// disk-full or permission problem surfaces above the noise of individual `warn!` entries.
    fn record_write_failure(&self) {
        self.write_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let total = self
            .total_write_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if total == DISK_CACHE_DEGRADED_THRESHOLD {
            error!(
                path = %self.base.display(),
                total,
                threshold = DISK_CACHE_DEGRADED_THRESHOLD,
                "disk cache is degraded: consecutive write failures have reached the alert threshold; \
                 check disk space and permissions at the cache directory"
            );
        }
    }

    /// Removes files not accessed within retention_days. Best-effort; silently drops errors.
    pub fn evict_stale(&self, retention_days: u64) {
        if self.disabled {
            return;
        }
        let cutoff = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(retention_days * 86_400))
            .unwrap_or(std::time::UNIX_EPOCH);
        let _ = evict_dir_recursive(&self.base, cutoff);
    }
}

fn evict_dir_recursive(
    dir: &std::path::Path,
    cutoff: std::time::SystemTime,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        let path = entry.path();
        if meta.is_dir() {
            let _ = evict_dir_recursive(&path, cutoff);
        } else if meta.is_file()
            && let Ok(mtime) = meta.modified()
            && mtime < cutoff
        {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod disk_cache_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_disk_cache_roundtrip() {
        let dir = TempDir::new().unwrap();
        let cache1 = DiskCache::new(dir.path().to_path_buf(), false);
        let key = blake3::hash(b"test-key");
        let value = serde_json::json!({"result": "hello", "count": 42});
        cache1.put("analyze_file", &key, &value);
        let cache2 = DiskCache::new(dir.path().to_path_buf(), false);
        let result: Option<serde_json::Value> = cache2.get("analyze_file", &key);
        assert_eq!(result, Some(value));
    }

    #[test]
    fn test_disk_cache_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let cache_dir = dir.path().join("analysis-cache");
        let _cache = DiskCache::new(cache_dir.clone(), false);
        let meta = std::fs::metadata(&cache_dir).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "cache dir must be mode 0700");
    }

    #[test]
    fn test_disk_cache_corrupt_entry_returns_none() {
        let dir = TempDir::new().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), false);
        let key = blake3::hash(b"corrupt-key");
        let path = cache.entry_path("analyze_file", &key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not valid snappy data").unwrap();
        let result: Option<serde_json::Value> = cache.get("analyze_file", &key);
        assert!(result.is_none(), "corrupt entry must return None");
    }

    #[test]
    fn test_disk_cache_disabled_on_dir_creation_failure() {
        let dir = TempDir::new().unwrap();
        // Place a regular file where DiskCache::new() would create a directory.
        // create_dir_all fails with ENOTDIR; new() must flip disabled=true.
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"").unwrap();
        let cache = DiskCache::new(blocked, false);
        // disabled=true: put is a no-op, get always returns None
        let key = blake3::hash(b"should-not-exist");
        cache.put("analyze_file", &key, &serde_json::json!({"x": 1}));
        let result: Option<serde_json::Value> = cache.get("analyze_file", &key);
        assert!(
            result.is_none(),
            "cache must be disabled after dir creation failure"
        );
        assert!(
            cache.disabled,
            "disabled flag must be true after dir creation failure"
        );
    }
}
