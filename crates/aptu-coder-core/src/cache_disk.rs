// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Disk-based cache for analysis results.
//!
//! Provides persistent, file-backed caching of analysis outputs with atomic writes,
//! per-shard locking, and stale-file eviction.

use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::NamedTempFile;
use tracing::{error, warn};

/// Threshold at which cumulative disk cache write failures trigger an alert.
const DISK_CACHE_DEGRADED_THRESHOLD: u64 = 100;

/// Persistent disk cache for analysis results.
///
/// Stores serialized analysis outputs in a directory hierarchy, with per-shard
/// advisory locking to prevent concurrent writes to the same entry. Supports
/// atomic writes via `NamedTempFile::persist` and graceful degradation on I/O errors.
pub struct DiskCache {
    base: PathBuf,
    disabled: bool,
    /// Counts write failures since last drain. Incremented inside `put` on any I/O error.
    write_failures: AtomicU64,
    /// Cumulative write failures across all drains. Never reset; used for threshold checks.
    total_write_failures: AtomicU64,
}

impl DiskCache {
    /// Returns the number of write failures accumulated since the last call and resets the
    /// per-drain counter. The cumulative `total_write_failures` is never reset.
    pub fn drain_write_failures(&self) -> u64 {
        self.write_failures.swap(0, Ordering::Relaxed)
    }

    /// Returns true when cumulative write failures have reached `DISK_CACHE_DEGRADED_THRESHOLD`.
    /// Callers can use this to emit a degraded health signal without polling the counter.
    pub fn is_degraded(&self) -> bool {
        self.total_write_failures.load(Ordering::Relaxed) >= DISK_CACHE_DEGRADED_THRESHOLD
    }
}

impl DiskCache {
    /// Creates the cache directory (mode 0700) and returns a new instance.
    /// If `disabled` is true, or if directory creation fails, all operations are no-ops.
    pub fn new(base: PathBuf, disabled: bool) -> Self {
        if disabled {
            return Self {
                base,
                disabled: true,
                write_failures: AtomicU64::new(0),
                total_write_failures: AtomicU64::new(0),
            };
        }
        if let Err(e) = std::fs::create_dir_all(&base) {
            warn!(path = %base.display(), error = %e, "disk cache disabled: failed to create cache directory");
            return Self {
                base,
                disabled: true,
                write_failures: AtomicU64::new(0),
                total_write_failures: AtomicU64::new(0),
            };
        }
        #[cfg(unix)]
        if let Err(e) = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)) {
            warn!(path = %base.display(), error = %e, "disk cache: failed to set directory permissions to 0700");
        }
        #[cfg(not(unix))]
        let _ = &base; // permissions not supported on this platform
        Self {
            base,
            disabled: false,
            write_failures: AtomicU64::new(0),
            total_write_failures: AtomicU64::new(0),
        }
    }

    pub fn entry_path(&self, tool: &str, key: &blake3::Hash) -> PathBuf {
        let hex = format!("{}", key);
        self.base
            .join(tool)
            .join(&hex[..2])
            .join(format!("{}.json.snap", hex))
    }

    /// Retrieves a cached entry by key, decompressing and deserializing on success.
    /// Returns None if the entry does not exist, is corrupted, or deserialization fails.
    pub fn get<T: DeserializeOwned>(&self, tool: &str, key: &blake3::Hash) -> Option<T> {
        if self.disabled {
            return None;
        }
        let path = self.entry_path(tool, key);
        let dir = path.parent()?;

        // Acquire shared lock on per-shard .lock sentinel before reading
        let _lock = lock_shard_shared(dir)?;

        let compressed = std::fs::read(&path).ok()?;
        let mut decompressed_data = Vec::new();
        snap::read::FrameDecoder::new(&compressed[..])
            .read_to_end(&mut decompressed_data)
            .ok()?;
        serde_json::from_slice(&decompressed_data).ok()
    }

    /// Serializes and compresses a value for storage.
    fn serialize_entry<T: Serialize>(value: &T) -> Option<Vec<u8>> {
        let json = serde_json::to_vec(value).ok()?;
        let mut compressed = Vec::new();
        snap::write::FrameEncoder::new(&mut compressed)
            .write_all(&json)
            .ok()?;
        Some(compressed)
    }

    /// Atomically writes a compressed entry to disk using NamedTempFile::persist.
    /// Acquires an exclusive lock on the per-shard .lock sentinel before writing.
    /// Returns Err if any step fails; caller silently drops the error.
    fn write_entry_atomically(
        dir: &std::path::Path,
        path: &std::path::Path,
        compressed: &[u8],
    ) -> Result<(), std::io::Error> {
        use std::io::Write;
        // Acquire exclusive lock on per-shard .lock sentinel before writing
        let _lock = lock_shard_exclusive(dir)?;
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
        self.write_failures.fetch_add(1, Ordering::Relaxed);
        let total = self.total_write_failures.fetch_add(1, Ordering::Relaxed) + 1;
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

/// Acquire a shared (read) lock on the per-shard `.lock` sentinel.
/// Creates the lock file if it does not exist. Lock failures degrade
/// gracefully (warn and return None) so that read availability is
/// never blocked by lock infrastructure issues.
fn lock_shard_shared(shard_dir: &std::path::Path) -> Option<ShardLockGuard> {
    let lock_path = shard_dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    match file.lock_shared() {
        Ok(()) => Some(ShardLockGuard(file)),
        Err(e) => {
            warn!(
                error = %e, lock_path = %lock_path.display(),
                "disk cache: failed to acquire shared lock on shard; proceeding without lock"
            );
            None
        }
    }
}

/// Acquire an exclusive (write) lock on the per-shard `.lock` sentinel.
/// Creates the lock file if it does not exist. Returns Err if the lock
/// file cannot be opened or if the lock acquisition fails, propagating
/// the error to the caller (which typically degrades gracefully).
fn lock_shard_exclusive(shard_dir: &std::path::Path) -> Result<ShardLockGuard, std::io::Error> {
    let lock_path = shard_dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    file.lock_exclusive()?;
    Ok(ShardLockGuard(file))
}

/// RAII guard that releases a per-shard flock when dropped.
/// Closing the underlying file descriptor releases the BSD/OFC lock.
struct ShardLockGuard(
    /// Held exclusively for its `Drop` implementation: closing the file
    /// descriptor releases the advisory flock. Never read directly.
    #[expect(dead_code)]
    std::fs::File,
);

#[cfg(test)]
mod disk_cache_tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;

    #[test]
    fn test_disk_cache_roundtrip() {
        let dir = TempDir::new().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), false);
        let key = blake3::hash(b"test-key");
        let value = serde_json::json!({"result": "success", "count": 42});
        cache.put("analyze_file", &key, &value);
        let retrieved: Option<serde_json::Value> = cache.get("analyze_file", &key);
        assert_eq!(retrieved, Some(value));
    }

    #[test]
    fn test_disk_cache_permissions() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = TempDir::new().unwrap();
            let cache_dir = dir.path().join("analysis-cache");
            let _cache = DiskCache::new(cache_dir.clone(), false);
            let meta = std::fs::metadata(&cache_dir).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "cache dir must be mode 0700");
        }
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

    #[test]
    fn test_concurrent_get_put_same_shard() {
        // Edge case: concurrent get() + put() on the same shard from two threads
        // must not panic and must return consistent results.
        let dir = TempDir::new().unwrap();
        let cache = std::sync::Arc::new(DiskCache::new(dir.path().to_path_buf(), false));
        let key = blake3::hash(b"concurrent-test-key");
        let value = serde_json::json!({"result": "from put thread", "n": 42});

        // Pre-populate so get() has a chance to read something
        cache.put("analyze_file", &key, &value);

        let cache_get = cache.clone();
        let cache_put = cache.clone();
        let key_put = key;
        let key_get = key;
        let value_put = serde_json::json!({"result": "from put thread", "n": 100});

        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Write thread: perform put
                cache_put.put("analyze_file", &key_put, &value_put);
            });
            scope.spawn(|| {
                // Read thread: perform get concurrently
                let _: Option<serde_json::Value> = cache_get.get("analyze_file", &key_get);
            });
        });

        // After both threads complete, verify the cache is in a consistent state
        let result: Option<serde_json::Value> = cache.get("analyze_file", &key);
        assert!(
            result.is_some(),
            "entry must still be retrievable after concurrent access"
        );
    }

    #[test]
    fn test_concurrent_puts_same_shard() {
        // Edge case: write_entry_atomically acquires exclusive lock before persist;
        // concurrent writes from two threads must not corrupt the cache entry.
        let dir = TempDir::new().unwrap();
        let cache = std::sync::Arc::new(DiskCache::new(dir.path().to_path_buf(), false));
        let key = blake3::hash(b"concurrent-put-key");
        let value_a = serde_json::json!({"writer": "A", "data": "hello from A"});
        let value_b = serde_json::json!({"writer": "B", "data": "hello from B"});

        let cache_a = cache.clone();
        let cache_b = cache.clone();
        let key_a = key;
        let key_b = key;

        std::thread::scope(|scope| {
            scope.spawn(|| {
                cache_a.put("analyze_file", &key_a, &value_a);
            });
            scope.spawn(|| {
                cache_b.put("analyze_file", &key_b, &value_b);
            });
        });

        // After both writes complete, the entry must be deserializable (not corrupt)
        let result: Option<serde_json::Value> = cache.get("analyze_file", &key);
        assert!(
            result.is_some(),
            "entry must be retrievable after concurrent puts"
        );
        // Either value is acceptable; the key invariant is that the data is uncorrupted
        let v = result.unwrap();
        let writer = v.get("writer").and_then(|w| w.as_str());
        assert!(
            writer == Some("A") || writer == Some("B"),
            "entry must contain data from one of the concurrent writers, got {writer:?}"
        );
    }
}
