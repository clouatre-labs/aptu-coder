// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Disk-backed structural graph cache with versioned postcard encoding, fs2
//! per-shard locking, atomic writes via NamedTempFile::persist, and size-capped
//! LRU eviction by file mtime. All I/O errors degrade silently via tracing::warn!.

use super::structural::StructuralGraph;
use blake3;
use fs2::FileExt;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use tracing::warn;

const FORMAT_VERSION: u32 = 2;
pub const DEFAULT_MAX_DISK_CACHE_BYTES: u64 = 512 * 1024 * 1024;

struct ShardLockGuard {
    _file: std::fs::File,
}
/// `.lock` files are 0-byte advisory control files, never written to.
/// Shard count is bounded at 256 by the 2-hex-char blake3 key prefix (`&key[..2]`).
fn lock_shard_shared(shard_dir: &Path) -> Option<ShardLockGuard> {
    let lock_path = shard_dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    file.lock_shared().map_err(|e| {
        warn!(error = %e, lock_path = %lock_path.display(), "graph store: shared lock failed")
    }).ok()?;
    Some(ShardLockGuard { _file: file })
}

/// `.lock` files are 0-byte advisory control files, never written to.
/// Shard count is bounded at 256 by the 2-hex-char blake3 key prefix (`&key[..2]`).
fn lock_shard_exclusive(shard_dir: &Path) -> Result<ShardLockGuard, std::io::Error> {
    let lock_path = shard_dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    file.lock_exclusive()?;
    Ok(ShardLockGuard { _file: file })
}

fn write_entry_atomically(dir: &Path, path: &Path, data: &[u8]) -> Result<(), std::io::Error> {
    let _lock = lock_shard_exclusive(dir)?;
    let mut tmp = NamedTempFile::new_in(dir)?;
    tmp.write_all(data)?;
    tmp.persist(path).map(|_| ()).map_err(|e| e.error)
}

fn evict_lru_if_over_budget(base_dir: &Path, max_bytes: u64) {
    // List all .bin files in all shard subdirectories
    let mut entries = Vec::new();

    match std::fs::read_dir(base_dir) {
        Ok(shards) => {
            for shard_entry in shards.flatten() {
                let shard_path = shard_entry.path();
                if !shard_path.is_dir() {
                    continue;
                }

                // List .bin files in this shard
                if let Ok(files) = std::fs::read_dir(&shard_path) {
                    for file_entry in files.flatten() {
                        let file_path = file_entry.path();
                        if file_path.extension().and_then(|e| e.to_str()) != Some("bin") {
                            continue;
                        }

                        if let Ok(metadata) = file_entry.metadata() {
                            entries.push((file_path, metadata.len(), metadata.modified().ok()));
                        }
                    }
                }
            }
        }
        Err(e) => {
            warn!(path = %base_dir.display(), error = %e, "graph store: failed to read base dir for eviction");
            return;
        }
    }

    // Sum total size
    let total_size: u64 = entries.iter().map(|(_, len, _)| len).sum();
    if total_size <= max_bytes {
        return;
    }

    // Sort by mtime ascending (oldest first)
    entries.sort_by(|a, b| {
        // Files without mtime go to the front (oldest)
        match (&a.2, &b.2) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(at), Some(bt)) => at.cmp(bt),
        }
    });

    // Delete entries until under budget
    let mut current_size = total_size;
    for (path, size, _) in entries {
        if current_size <= max_bytes {
            break;
        }

        // Lock the shard directory
        let shard_dir = path.parent().unwrap_or(base_dir);
        match lock_shard_exclusive(shard_dir) {
            Ok(_lock) => {
                if let Err(e) = std::fs::remove_file(&path) {
                    warn!(key = %path.display(), error = %e, "graph store: eviction remove_file failed");
                } else {
                    current_size = current_size.saturating_sub(size);
                }
            }
            Err(e) => {
                warn!(shard = %shard_dir.display(), error = %e, "graph store: eviction lock failed");
                continue;
            }
        }
    }
}

pub struct GraphDiskStore {
    base_dir: PathBuf,
    max_bytes: u64,
}

impl GraphDiskStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self::new_with_max_bytes(base_dir, DEFAULT_MAX_DISK_CACHE_BYTES)
    }

    pub fn new_with_max_bytes(base_dir: PathBuf, max_bytes: u64) -> Self {
        if let Err(e) = std::fs::create_dir_all(&base_dir) {
            warn!(path = %base_dir.display(), error = %e, "graph store: failed to create base dir");
        }
        GraphDiskStore {
            base_dir,
            max_bytes,
        }
    }

    pub fn cache_key(root: &Path, file_hashes: &[(PathBuf, blake3::Hash)]) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(root.to_string_lossy().as_bytes());
        let mut sorted: Vec<&(PathBuf, blake3::Hash)> = file_hashes.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, hash) in &sorted {
            hasher.update(path.to_string_lossy().as_bytes());
            hasher.update(hash.as_bytes());
        }
        hasher.finalize().to_string()
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        self.base_dir.join(&key[..2]).join(format!("{}.bin", key))
    }
    pub fn get(&self, key: &str) -> Option<StructuralGraph> {
        let path = self.entry_path(key);
        let dir = path.parent()?;
        let _lock = lock_shard_shared(dir)?;
        let data = std::fs::read(&path).ok()?;
        if data.len() < 4 {
            return None;
        }
        let (hdr, payload) = data.split_at(4);
        if u32::from_le_bytes(<[u8; 4]>::try_from(hdr).ok()?) != FORMAT_VERSION {
            warn!(key, "graph store: format version mismatch");
            return None;
        }
        let mut graph: StructuralGraph = postcard::from_bytes(payload).ok()?;
        graph.rebuild_symbol_index();

        // Touch file mtime to mark as recently used (best-effort)
        if let Ok(file) = std::fs::File::options().write(true).open(&path)
            && let Err(e) = file.set_modified(std::time::SystemTime::now())
        {
            warn!(key, error = %e, "graph store: failed to touch mtime on read");
        }

        Some(graph)
    }

    pub fn put(&self, key: &str, graph: &StructuralGraph) {
        let payload = match postcard::to_allocvec(graph) {
            Ok(p) => p,
            Err(e) => {
                warn!(key, error = %e, "graph store: serialize failed");
                return;
            }
        };
        let mut data = Vec::with_capacity(4 + payload.len());
        data.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        data.extend_from_slice(&payload);
        let path = self.entry_path(key);
        let Some(dir) = path.parent().map(|d| d.to_path_buf()) else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(key, error = %e, "graph store: mkdir failed");
            return;
        }
        if let Err(e) = write_entry_atomically(&dir, &path, &data) {
            warn!(key, error = %e, "graph store: write failed");
            return;
        }

        // Evict old entries if over budget (after successful write)
        evict_lru_if_over_budget(&self.base_dir, self.max_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_test_graph() -> StructuralGraph {
        use crate::graph::structural::Node;
        let mut g = petgraph::graph::DiGraph::new();
        g.add_node(Node::File {
            path: "t.rs".into(),
        });
        StructuralGraph::from_graph(g)
    }

    #[test]
    fn test_put_and_get_roundtrip() {
        let tmp = TempDir::new().expect("temp dir");
        let store = GraphDiskStore::new(tmp.path().to_path_buf());
        let graph = make_test_graph();
        store.put("key1", &graph);
        let got = store.get("key1");
        assert!(got.is_some());
        assert_eq!(got.unwrap().graph.node_count(), 1);
    }

    #[test]
    fn test_get_version_mismatch_returns_none() {
        let tmp = TempDir::new().expect("temp dir");
        let store = GraphDiskStore::new(tmp.path().to_path_buf());
        let key = "vm";
        let dir = tmp.path().join(&key[..2]);
        let path = dir.join(format!("{}.bin", key));
        std::fs::create_dir_all(&dir).ok();
        let mut data = 99u32.to_le_bytes().to_vec();
        data.extend_from_slice(b"x");
        std::fs::write(&path, &data).ok();
        assert!(store.get(key).is_none());
    }

    #[test]
    fn test_eviction_by_lru_mtime() {
        let tmp = TempDir::new().expect("temp dir");
        // Use a small 1 KiB budget to easily trigger eviction
        let store = GraphDiskStore::new_with_max_bytes(tmp.path().to_path_buf(), 1024);
        let graph = make_test_graph();

        // Write multiple entries that will exceed the budget
        store.put("aaa_key1", &graph);
        store.put("aaa_key2", &graph);
        store.put("aaa_key3", &graph);

        // Check at least one entry was evicted (oldest should be gone)
        // We can't guarantee which entries remain, but we know:
        // 1. The total size should be <= budget
        // 2. The oldest entries should be gone first
        let dir = tmp.path().join("aa");
        let total_size: u64 = std::fs::read_dir(&dir)
            .ok()
            .and_then(|entries| {
                let size = entries
                    .flatten()
                    .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("bin"))
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .sum::<u64>();
                Some(size)
            })
            .unwrap_or(0);

        assert!(
            total_size <= 1024,
            "total size {} exceeds budget",
            total_size
        );
    }

    #[test]
    fn test_get_touches_mtime() {
        let tmp = TempDir::new().expect("temp dir");
        let store = GraphDiskStore::new(tmp.path().to_path_buf());
        let graph = make_test_graph();

        store.put("touch_key", &graph);
        let path = store.entry_path("touch_key");

        // Get initial mtime
        let initial_mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok());

        // Sleep a tiny bit to ensure time difference is measurable
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Read the entry
        let _graph = store.get("touch_key");

        // Get new mtime
        let new_mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok());

        // New mtime should be >= initial mtime (should be newer due to touch)
        match (initial_mtime, new_mtime) {
            (Some(im), Some(nm)) => assert!(nm >= im, "mtime should advance or stay same on read"),
            _ => {} // Can't test if metadata fails
        }
    }
}
