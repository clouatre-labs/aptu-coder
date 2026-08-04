// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Disk-backed structural graph cache with versioned postcard encoding.
//! Uses fs2 per-shard locking and atomic writes via NamedTempFile::persist.
//! All I/O errors degrade silently via tracing::warn!.

use super::structural::StructuralGraph;
use blake3;
use fs2::FileExt;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use tracing::warn;

const FORMAT_VERSION: u32 = 1;

struct ShardLockGuard {
    _file: std::fs::File,
}

fn lock_shard_shared(shard_dir: &Path) -> Option<ShardLockGuard> {
    let lock_path = shard_dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    file.lock_shared()
        .map_err(|e| {
            warn!(error = %e, lock_path = %lock_path.display(), "graph store: shared lock failed");
        })
        .ok()?;
    Some(ShardLockGuard { _file: file })
}

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

pub struct GraphDiskStore {
    base_dir: PathBuf,
}

impl GraphDiskStore {
    pub fn new(base_dir: PathBuf) -> Self {
        if let Err(e) = std::fs::create_dir_all(&base_dir) {
            warn!(path = %base_dir.display(), error = %e, "graph store: failed to create base dir");
        }
        GraphDiskStore { base_dir }
    }

    pub fn cache_key(root: &Path, file_mtimes: &[(PathBuf, u64)]) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(root.to_string_lossy().as_bytes());
        let mut sorted: Vec<&(PathBuf, u64)> = file_mtimes.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, mtime) in &sorted {
            hasher.update(path.to_string_lossy().as_bytes());
            hasher.update(&mtime.to_le_bytes());
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
        postcard::from_bytes(payload).ok()
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
        let dir = path.parent().map(|d| d.to_path_buf());
        let Some(dir) = dir else { return };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(key, error = %e, "graph store: mkdir failed");
            return;
        }
        if let Err(e) = write_entry_atomically(&dir, &path, &data) {
            warn!(key, error = %e, "graph store: write failed");
        }
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
        StructuralGraph(g)
    }

    #[test]
    fn test_put_and_get_roundtrip() {
        let tmp = TempDir::new().expect("temp dir");
        let store = GraphDiskStore::new(tmp.path().to_path_buf());
        let graph = make_test_graph();
        store.put("key1", &graph);
        let got = store.get("key1");
        assert!(got.is_some());
        assert_eq!(got.unwrap().0.node_count(), 1);
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
}
