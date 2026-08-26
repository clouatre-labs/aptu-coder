// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! LRU cache for analysis results indexed by path, modification time, and mode.
//!
//! Provides thread-safe, capacity-bounded caching of file analysis outputs using LRU eviction.
//! Recovers gracefully from poisoned mutex conditions.

use crate::analyze::{AnalysisOutput, FileAnalysisOutput, FocusedAnalysisOutput};
use crate::graph::structural::StructuralGraph;
use crate::traversal::WalkEntry;
use crate::types::{AnalysisMode, SymbolMatchMode};
use lru::LruCache;
use rayon::prelude::*;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tracing::{debug, instrument, warn};

/// Indicates which cache tier served the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheTier {
    L1Memory,
    L2Disk,
    Miss,
    L1OnlyMiss,
    L1L2Miss,
}

/// Parse an LRU cache capacity from an environment variable.
///
/// Reads `env_key`, parses it as `usize`, and returns the value clamped to a minimum of 1.
/// Falls back to `default` when the variable is absent or unparseable, then also clamps
/// the fallback to at least 1.
///
/// This helper centralises all three LRU init sites so the `.max(1)` guard lives in one place.
#[must_use]
pub fn parse_cache_capacity(env_key: &str, default: usize) -> usize {
    std::env::var(env_key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
        .max(1)
}

impl CacheTier {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            CacheTier::L1Memory => "l1_memory",
            CacheTier::L2Disk => "l2_disk",
            CacheTier::Miss => "miss",
            CacheTier::L1OnlyMiss => "l1_only_miss",
            CacheTier::L1L2Miss => "l1_l2_miss",
        }
    }

    /// Returns `true` when the result was served from any cache tier (L1 or L2).
    #[must_use]
    pub fn is_hit(&self) -> bool {
        matches!(self, CacheTier::L1Memory | CacheTier::L2Disk)
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

/// Fallback cache capacity used in `lock_or_recover` when the caller-supplied capacity is zero.
// SAFETY: 100 is non-zero; verified at compile time by NonZeroUsize::new.
#[allow(clippy::expect_used)]
const DEFAULT_LOCK_RECOVER_CAPACITY: NonZeroUsize =
    NonZeroUsize::new(100).expect("100 is non-zero");

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
            tracing::warn!("Mutex poisoned in lock_or_recover; creating fresh LruCache");
            let cache_size = NonZeroUsize::new(capacity).unwrap_or(DEFAULT_LOCK_RECOVER_CAPACITY);
            let new_cache = LruCache::new(cache_size);
            let mut guard = poisoned.into_inner();
            *guard = new_cache;
            recovery(&mut guard)
        }
    }
}

/// Cache key for call graph analysis combining path, parameters, and file mtimes.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct CallGraphCacheKey {
    root_path: PathBuf,
    git_ref: Option<String>,
    follow_depth: u32,
    match_mode: SymbolMatchMode,
    impl_only: bool,
    ast_recursion_limit: Option<usize>,
    /// Sorted (path, mtime_as_unix_nanos) pairs for all non-dir entries.
    file_mtimes: Vec<(PathBuf, u64)>,
}

impl CallGraphCacheKey {
    /// Build a `CallGraphCacheKey` from walk entries and analysis parameters.
    /// Files are sorted by path for deterministic hashing.
    /// Directories are filtered out; only file entries contribute to the key.
    #[must_use]
    pub fn from_entries(
        root: &std::path::Path,
        entries: &[WalkEntry],
        git_ref: Option<&str>,
        follow_depth: u32,
        match_mode: &SymbolMatchMode,
        impl_only: bool,
        ast_recursion_limit: Option<usize>,
    ) -> Self {
        let mut file_mtimes: Vec<(PathBuf, u64)> = entries
            .par_iter()
            .filter(|e| !e.is_dir)
            .map(|e| {
                let mtime = e
                    .mtime
                    .unwrap_or(SystemTime::UNIX_EPOCH)
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                (e.path.clone(), mtime)
            })
            .collect();
        file_mtimes.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            root_path: root.to_path_buf(),
            git_ref: git_ref.map(ToOwned::to_owned),
            follow_depth,
            match_mode: match_mode.clone(),
            impl_only,
            ast_recursion_limit,
            file_mtimes,
        }
    }
}

/// Cached call graph result: the fully-built `FocusedAnalysisOutput`.
/// `CallGraph` is not serializable, so caching is L1 memory only.
pub type CallGraphCacheValue = Arc<FocusedAnalysisOutput>;

/// Generic thread-safe LRU cache with eviction tracking and poison recovery.
///
/// Wraps an [`LruCache`] in `Arc<Mutex<...>>` with an atomic eviction counter.
/// Recovers gracefully from mutex poisoning by clearing the cache.
pub struct AnalysisLruCache<K: std::hash::Hash + Eq + Clone, V: Clone> {
    capacity: usize,
    cache: Arc<Mutex<LruCache<K, V>>>,
    eviction_count: Arc<AtomicU64>,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> AnalysisLruCache<K, V> {
    /// Create a new `AnalysisLruCache` with the given capacity.
    ///
    /// `capacity` is clamped to a minimum of 1 so a zero value does not panic.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        // SAFETY: capacity is clamped to a minimum of 1 by .max(1), so NonZeroUsize::new() returns Some.
        #[allow(clippy::expect_used)]
        let cache_size = NonZeroUsize::new(capacity).expect("capacity is non-zero after .max(1)");
        Self {
            capacity,
            cache: Arc::new(Mutex::new(LruCache::new(cache_size))),
            eviction_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Look up a cached result by key. Returns `None` on miss or mutex poison.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<V> {
        lock_or_recover(&self.cache, self.capacity, |guard| guard.get(key).cloned())
    }

    /// Store a key-value pair in the cache.
    ///
    /// Uses [`LruCache::push`] to distinguish between insertions, updates of existing
    /// keys, and evictions of other entries. Eviction count only increments when a
    /// different key is evicted at capacity.
    pub fn put(&self, key: K, value: V) {
        lock_or_recover(&self.cache, self.capacity, |guard| {
            if let Some((evicted_key, _)) = guard.push(key.clone(), value)
                && evicted_key != key
            {
                self.eviction_count.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    /// Returns the number of LRU evictions that have occurred in this cache.
    #[must_use]
    pub fn eviction_count(&self) -> u64 {
        self.eviction_count.load(Ordering::Relaxed)
    }
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> Clone for AnalysisLruCache<K, V> {
    fn clone(&self) -> Self {
        Self {
            capacity: self.capacity,
            cache: Arc::clone(&self.cache),
            eviction_count: Arc::clone(&self.eviction_count),
        }
    }
}

/// L1 in-memory LRU cache for call graph results.
/// Capacity is controlled via `APTU_CODER_SYMBOL_CACHE_CAPACITY` env var (default 32).
#[derive(Clone)]
pub struct CallGraphCache(AnalysisLruCache<CallGraphCacheKey, CallGraphCacheValue>);

impl CallGraphCache {
    /// Create a new `CallGraphCache` with the given capacity.
    ///
    /// `capacity` is clamped to a minimum of 1 so a zero value does not panic.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self(AnalysisLruCache::new(capacity))
    }

    /// Look up a cached result by key. Returns `None` on miss or mutex poison.
    #[must_use]
    pub fn get(&self, key: &CallGraphCacheKey) -> Option<CallGraphCacheValue> {
        self.0.get(key)
    }

    /// Store a result in the cache.
    pub fn put(&self, key: CallGraphCacheKey, value: CallGraphCacheValue) {
        self.0.put(key, value);
    }

    /// Returns the number of LRU evictions that have occurred in this cache.
    #[must_use]
    pub fn eviction_count(&self) -> u64 {
        self.0.eviction_count()
    }
}

/// Cached structural graph result: the fully-built `StructuralGraph`.
pub type StructuralGraphCacheValue = Arc<StructuralGraph>;

/// L1 in-memory LRU cache for structural graph results.
/// Capacity is controlled via `APTU_CODER_STRUCTURAL_CACHE_CAPACITY` env var (default 16).
#[derive(Clone)]
pub struct StructuralGraphCache(AnalysisLruCache<String, StructuralGraphCacheValue>);

impl StructuralGraphCache {
    /// Create a new `StructuralGraphCache` with the given capacity.
    ///
    /// `capacity` is clamped to a minimum of 1 so a zero value does not panic.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self(AnalysisLruCache::new(capacity))
    }

    /// Look up a cached result by key. Returns `None` on miss or mutex poison.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<StructuralGraphCacheValue> {
        self.0.get(&key.to_string())
    }

    /// Store a result in the cache.
    pub fn put(&self, key: String, value: StructuralGraphCacheValue) {
        self.0.put(key, value);
    }

    /// Returns the number of LRU evictions that have occurred in this cache.
    #[must_use]
    pub fn eviction_count(&self) -> u64 {
        self.0.eviction_count()
    }
}

/// LRU cache for file analysis results with mutex protection.
pub struct AnalysisCache {
    file_capacity: usize,
    dir_capacity: usize,
    cache: Arc<Mutex<LruCache<CacheKey, Arc<FileAnalysisOutput>>>>,
    directory_cache: Arc<Mutex<LruCache<DirectoryCacheKey, Arc<AnalysisOutput>>>>,
    eviction_count: Arc<AtomicU64>,
}

impl AnalysisCache {
    /// Create a new cache with the specified file capacity.
    /// The directory cache capacity is read from the `APTU_CODER_DIR_CACHE_CAPACITY`
    /// environment variable (default: 20).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let file_capacity = capacity.max(1);
        let dir_capacity = parse_cache_capacity("APTU_CODER_DIR_CACHE_CAPACITY", 20);
        // SAFETY: file_capacity is clamped to a minimum of 1 by .max(1), so NonZeroUsize::new() returns Some.
        #[allow(clippy::expect_used)]
        let cache_size =
            NonZeroUsize::new(file_capacity).expect("file_capacity is non-zero after .max(1)");
        // SAFETY: dir_capacity is clamped to a minimum of 1 by parse_cache_capacity, so NonZeroUsize::new() returns Some.
        #[allow(clippy::expect_used)]
        let dir_cache_size =
            NonZeroUsize::new(dir_capacity).expect("dir_capacity is non-zero after .max(1)");
        Self {
            file_capacity,
            dir_capacity,
            cache: Arc::new(Mutex::new(LruCache::new(cache_size))),
            directory_cache: Arc::new(Mutex::new(LruCache::new(dir_cache_size))),
            eviction_count: Arc::new(AtomicU64::new(0)),
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
                        self.eviction_count.fetch_add(1, Ordering::Relaxed);
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
    #[must_use]
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

    /// Returns the number of LRU evictions that have occurred in this cache.
    #[must_use]
    pub fn eviction_count(&self) -> u64 {
        self.eviction_count.load(Ordering::Relaxed)
    }
}

impl Clone for AnalysisCache {
    fn clone(&self) -> Self {
        Self {
            file_capacity: self.file_capacity,
            dir_capacity: self.dir_capacity,
            cache: Arc::clone(&self.cache),
            directory_cache: Arc::clone(&self.directory_cache),
            eviction_count: Arc::clone(&self.eviction_count),
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

    // Mutex serialises parse_cache_capacity tests that set env vars.
    static PARSE_CAP_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_parse_cache_capacity_missing_returns_default() {
        let _guard = PARSE_CAP_ENV_LOCK.lock().unwrap();

        // Arrange: env var is absent
        unsafe { std::env::remove_var("_TEST_APTU_PARSE_CAP") };

        // Act
        let result = parse_cache_capacity("_TEST_APTU_PARSE_CAP", 42);

        // Assert: default is returned as-is
        assert_eq!(result, 42);
    }

    #[test]
    fn test_parse_cache_capacity_valid_returns_value() {
        let _guard = PARSE_CAP_ENV_LOCK.lock().unwrap();

        // Arrange
        unsafe { std::env::set_var("_TEST_APTU_PARSE_CAP", "64") };

        // Act
        let result = parse_cache_capacity("_TEST_APTU_PARSE_CAP", 10);

        // Cleanup
        unsafe { std::env::remove_var("_TEST_APTU_PARSE_CAP") };

        // Assert: parsed value is returned
        assert_eq!(result, 64);
    }

    #[test]
    fn test_parse_cache_capacity_zero_returns_one() {
        let _guard = PARSE_CAP_ENV_LOCK.lock().unwrap();

        // Arrange: zero is below the minimum of 1
        unsafe { std::env::set_var("_TEST_APTU_PARSE_CAP", "0") };

        // Act
        let result = parse_cache_capacity("_TEST_APTU_PARSE_CAP", 10);

        // Cleanup
        unsafe { std::env::remove_var("_TEST_APTU_PARSE_CAP") };

        // Assert: clamped to 1
        assert_eq!(result, 1);
    }

    #[test]
    fn test_parse_cache_capacity_garbage_returns_default() {
        let _guard = PARSE_CAP_ENV_LOCK.lock().unwrap();

        // Arrange: unparseable string
        unsafe { std::env::set_var("_TEST_APTU_PARSE_CAP", "not_a_number") };

        // Act
        let result = parse_cache_capacity("_TEST_APTU_PARSE_CAP", 8);

        // Cleanup
        unsafe { std::env::remove_var("_TEST_APTU_PARSE_CAP") };

        // Assert: falls back to default
        assert_eq!(result, 8);
    }

    #[test]
    fn test_analysis_lru_cache_hit_miss_recency_and_eviction() {
        let cache = AnalysisLruCache::<&'static str, i32>::new(2);
        cache.put("a", 1);
        cache.put("b", 2);
        // hit returns Some, miss returns None
        assert_eq!(cache.get(&"a"), Some(1));
        assert_eq!(cache.get(&"z"), None);
        // "a" is now MRU; inserting "c" evicts "b" (LRU)
        cache.put("c", 3);
        assert_eq!(cache.get(&"a"), Some(1));
        assert_eq!(cache.get(&"b"), None);
        assert_eq!(cache.eviction_count(), 1);
    }

    #[test]
    fn test_analysis_lru_cache_put_update_at_capacity_no_false_eviction() {
        let cache = AnalysisLruCache::<String, i32>::new(2);
        cache.put("k1".to_string(), 10);
        cache.put("k2".to_string(), 20);
        assert_eq!(cache.eviction_count(), 0);
        // update existing key at full capacity
        cache.put("k1".to_string(), 99);
        // no false eviction on update
        assert_eq!(cache.eviction_count(), 0);
        assert_eq!(cache.get(&"k1".to_string()), Some(99));
        assert_eq!(cache.get(&"k2".to_string()), Some(20));
    }

    #[test]
    fn test_analysis_lru_cache_clone_shares_eviction_counter() {
        let cache1 = AnalysisLruCache::<&'static str, i32>::new(1);
        let cache2 = cache1.clone();
        cache1.put("k1", 1);
        cache2.put("k2", 2);
        // cloned instances share eviction_count
        assert_eq!(cache1.eviction_count(), 1);
        assert_eq!(cache2.eviction_count(), 1);
        assert_eq!(cache1.get(&"k1"), None);
        assert_eq!(cache1.get(&"k2"), Some(2));
    }

    #[test]
    fn test_analysis_lru_cache_new_clamps_zero_capacity_to_one() {
        let cache = AnalysisLruCache::<&'static str, i32>::new(0);
        assert_eq!(cache.capacity, 1);
        cache.put("item1", 1);
        assert_eq!(cache.get(&"item1"), Some(1));
        assert_eq!(cache.eviction_count(), 0);
        cache.put("item2", 2);
        assert_eq!(cache.eviction_count(), 1);
        assert_eq!(cache.get(&"item1"), None);
        assert_eq!(cache.get(&"item2"), Some(2));
    }

    #[test]
    fn test_structural_graph_cache_hit_miss() {
        let cache = StructuralGraphCache::new(2);
        let key1 = "key1".to_string();
        let key2 = "key2".to_string();
        let graph = Arc::new(StructuralGraph::from_graph(petgraph::graph::DiGraph::new()));

        // Miss on non-existent key
        assert!(cache.get("nonexistent").is_none());

        // Put and hit
        cache.put(key1.clone(), graph.clone());
        assert!(cache.get("key1").is_some());

        // Miss on different key
        assert!(cache.get("key2").is_none());

        // Put second key
        cache.put(key2.clone(), graph.clone());
        assert!(cache.get("key2").is_some());
    }

    #[test]
    fn test_structural_graph_cache_eviction() {
        let cache = StructuralGraphCache::new(2);
        let graph = Arc::new(StructuralGraph::from_graph(petgraph::graph::DiGraph::new()));

        cache.put("k1".to_string(), graph.clone());
        cache.put("k2".to_string(), graph.clone());
        assert_eq!(cache.eviction_count(), 0);

        // Inserting k3 evicts k1 (LRU)
        cache.put("k3".to_string(), graph.clone());
        assert_eq!(cache.eviction_count(), 1);
        assert!(cache.get("k1").is_none());
        assert!(cache.get("k2").is_some());
        assert!(cache.get("k3").is_some());
    }

    #[test]
    fn test_structural_graph_cache_clone_shares_counter() {
        let cache1 = StructuralGraphCache::new(1);
        let cache2 = cache1.clone();
        let graph = Arc::new(StructuralGraph::from_graph(petgraph::graph::DiGraph::new()));

        cache1.put("k1".to_string(), graph.clone());
        cache2.put("k2".to_string(), graph);

        // Cloned instances share eviction_count
        assert_eq!(cache1.eviction_count(), 1);
        assert_eq!(cache2.eviction_count(), 1);
    }
}

pub use crate::cache_disk::DiskCache;
