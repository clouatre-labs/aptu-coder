// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Directory traversal with .gitignore support.
//!
//! Provides recursive directory walking with automatic filtering based on `.gitignore` and `.ignore` files.
//! Uses the `ignore` crate for cross-platform, efficient file system traversal.

use ignore::WalkBuilder;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime};
use thiserror::Error;
use tracing::instrument;

pub const MAX_WALK_ENTRIES: usize = 50_000;

/// Build a `WalkEntry` from a directory entry, capturing symlink target and mtime.
fn walk_entry_from(entry: &ignore::DirEntry) -> WalkEntry {
    let path = entry.path().to_path_buf();
    let is_symlink = entry.path_is_symlink();

    let symlink_target = if is_symlink {
        std::fs::read_link(&path).ok()
    } else {
        None
    };

    let mtime = entry.metadata().ok().and_then(|m| m.modified().ok());

    WalkEntry {
        canonical_path: path.clone(),
        depth: entry.depth(),
        is_dir: entry.file_type().is_some_and(|ft| ft.is_dir()),
        is_symlink,
        symlink_target,
        mtime,
        path,
    }
}

/// Truncate, log, and sort walk results. `message` distinguishes parallel vs sync walks.
fn finish_walk(mut entries: Vec<WalkEntry>, start: Instant, message: &str) -> Vec<WalkEntry> {
    entries.truncate(MAX_WALK_ENTRIES);
    if entries.len() >= MAX_WALK_ENTRIES {
        tracing::warn!(
            "walk truncated at {} entries (MAX_WALK_ENTRIES={}); results are partial",
            MAX_WALK_ENTRIES,
            MAX_WALK_ENTRIES
        );
    }

    let dir_count = entries.iter().filter(|e| e.is_dir).count();
    let file_count = entries.iter().filter(|e| !e.is_dir).count();

    tracing::debug!(
        entries = entries.len(),
        dirs = dir_count,
        files = file_count,
        duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        "{}",
        message
    );

    // Restore sort contract: walk_parallel does not guarantee order.
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

#[derive(Debug, Clone)]
pub struct WalkEntry {
    pub path: PathBuf,
    /// Depth in the directory tree (0 = root).
    pub depth: usize,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub symlink_target: Option<PathBuf>,
    pub mtime: Option<SystemTime>,
    pub canonical_path: PathBuf,
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TraversalError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal concurrency error: {0}")]
    Internal(String),
    #[error("git error: {0}")]
    GitError(String),
}

/// Walk a directory with support for `.gitignore` and `.ignore`.
/// `max_depth=0` maps to unlimited recursion (None), positive values limit depth.
/// The returned entries are sorted lexicographically by path.
#[instrument(skip_all, fields(path = %root.display(), max_depth))]
pub fn walk_directory(
    root: &Path,
    max_depth: Option<u32>,
) -> Result<Vec<WalkEntry>, TraversalError> {
    let start = Instant::now();

    // Optimization: use synchronous walker for max_depth == Some(1) to avoid thread spawn overhead.
    if max_depth == Some(1) {
        return walk_directory_sync(root, start);
    }

    let mut builder = WalkBuilder::new(root);
    builder.hidden(true).standard_filters(true);

    // Map max_depth: 0 = unlimited (None), positive = Some(n)
    let max_depth_usize = if let Some(depth) = max_depth
        && depth > 0
    {
        builder.max_depth(Some(depth as usize));
        Some(depth as usize)
    } else {
        None
    };

    let (sender, receiver) = std::sync::mpsc::channel::<WalkEntry>();
    let entry_count = Arc::new(AtomicUsize::new(0));

    builder.build_parallel().run(move || {
        let sender = sender.clone();
        let entry_count = entry_count.clone();
        Box::new(move |result| match result {
            Ok(entry) => {
                let walk_entry = walk_entry_from(&entry);
                let depth = walk_entry.depth;
                let is_dir = walk_entry.is_dir;
                sender.send(walk_entry).ok();
                let count = entry_count.fetch_add(1, Ordering::Relaxed);
                if count >= MAX_WALK_ENTRIES {
                    return ignore::WalkState::Quit;
                }
                // Skip recursing into directories at the depth boundary. The entry
                // itself has already been sent above; this only prevents descent into
                // its children, avoiding gitignore matching on large trees (target/,
                // node_modules/) whose contents would be discarded by the depth limit.
                if is_dir && max_depth_usize.is_some_and(|max_d| depth >= max_d) {
                    return ignore::WalkState::Skip;
                }
                ignore::WalkState::Continue
            }
            Err(e) => {
                tracing::warn!(error = %e, "skipping unreadable entry");
                ignore::WalkState::Continue
            }
        })
    });

    let entries = finish_walk(receiver.try_iter().collect(), start, "walk complete");
    Ok(entries)
}

/// Synchronous walker for max_depth == Some(1) to eliminate thread spawn overhead.
fn walk_directory_sync(root: &Path, start: Instant) -> Result<Vec<WalkEntry>, TraversalError> {
    let mut builder = WalkBuilder::new(root);
    builder.hidden(true).standard_filters(true);
    builder.max_depth(Some(1));

    let mut entries = Vec::new();
    let entry_count = Arc::new(AtomicUsize::new(0));

    for result in builder.build() {
        match result {
            Ok(entry) => {
                entries.push(walk_entry_from(&entry));
                let count = entry_count.fetch_add(1, Ordering::Relaxed);
                if count >= MAX_WALK_ENTRIES {
                    break;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "skipping unreadable entry");
            }
        }
    }

    let entries = finish_walk(entries, start, "walk complete (sync)");
    Ok(entries)
}

/// Return the set of absolute paths changed relative to `git_ref` in the repository
/// containing `dir`. Invokes git without shell interpolation.
///
/// # Errors
/// Returns [`TraversalError::GitError`] when:
/// - `git` is not on PATH (distinct message)
/// - `dir` is not inside a git repository
pub fn changed_files_from_git_ref(
    dir: &Path,
    git_ref: &str,
) -> Result<HashSet<PathBuf>, TraversalError> {
    // Validate git_ref to prevent argument injection attacks.
    // Reject refs that start with '-' (would be interpreted as flags).
    // Also reject refs containing whitespace or shell metacharacters.
    if git_ref.is_empty() || git_ref.starts_with('-') {
        return Err(TraversalError::GitError(
            "invalid git_ref: must not be empty or start with '-'".to_string(),
        ));
    }
    if git_ref.chars().any(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '|' | '&'
                    | ';'
                    | '>'
                    | '<'
                    | '`'
                    | '$'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '*'
                    | '?'
                    | '\\'
                    | '"'
                    | '\''
            )
    }) {
        return Err(TraversalError::GitError(
            "invalid git_ref: contains forbidden characters".to_string(),
        ));
    }

    // Resolve the git repository root so that relative paths from `git diff` can
    // be anchored to an absolute base.
    let root_out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TraversalError::GitError("git not found on PATH".to_string())
            } else {
                TraversalError::GitError(format!("failed to run git: {e}"))
            }
        })?;

    if !root_out.status.success() {
        let stderr = String::from_utf8_lossy(&root_out.stderr);
        return Err(TraversalError::GitError(format!(
            "not a git repository: {stderr}"
        )));
    }

    let root_raw = PathBuf::from(String::from_utf8_lossy(&root_out.stdout).trim().to_string());
    // Canonicalize to resolve symlinks (e.g. macOS /tmp -> /private/tmp) so that
    // paths from git and paths from walk_directory are comparable.
    let root = std::fs::canonicalize(&root_raw).unwrap_or(root_raw);

    // Run `git diff --name-only <git_ref>` to get changed files relative to root.
    let diff_out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("diff")
        .arg("--name-only")
        .arg(git_ref)
        .output()
        .map_err(|e| TraversalError::GitError(format!("failed to run git diff: {e}")))?;

    if !diff_out.status.success() {
        let stderr = String::from_utf8_lossy(&diff_out.stderr);
        return Err(TraversalError::GitError(format!(
            "git diff failed: {stderr}"
        )));
    }

    let changed: HashSet<PathBuf> = String::from_utf8_lossy(&diff_out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| root.join(l))
        .collect();

    Ok(changed)
}

/// Filter walk entries to only those that are either changed files or ancestor directories
/// of changed files. This preserves the tree structure while limiting analysis scope.
///
/// Uses O(|changed| * depth + |entries|) time by precomputing a HashSet of ancestor
/// directories for each changed file (up to and including `root`).
#[must_use]
pub fn filter_entries_by_git_ref(
    entries: Vec<WalkEntry>,
    changed: &HashSet<PathBuf>,
    root: &Path,
) -> Vec<WalkEntry> {
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    // Precompute canonical changed set so comparison works across symlink differences.
    let canonical_changed: HashSet<PathBuf> = changed
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();

    // Build HashSet of all ancestor directories of changed files (bounded by canonical_root).
    let mut ancestor_dirs: HashSet<PathBuf> = HashSet::new();
    ancestor_dirs.insert(canonical_root.clone());
    for p in &canonical_changed {
        let mut cur = p.as_path();
        while let Some(parent) = cur.parent() {
            if !ancestor_dirs.insert(parent.to_path_buf()) {
                // Already inserted this ancestor and all its ancestors; stop early.
                break;
            }
            if parent == canonical_root {
                break;
            }
            cur = parent;
        }
    }

    entries
        .into_iter()
        .filter(|e| {
            let canonical = std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone());
            if e.is_dir {
                ancestor_dirs.contains(&canonical)
            } else {
                canonical_changed.contains(&canonical)
            }
        })
        .collect()
}

/// Compute files-per-depth-1-subdirectory counts from an already-collected entry list.
/// Returns a Vec of (depth-1 path, file count) sorted by path.
/// Only counts file entries (not directories); skips entries containing `EXCLUDED_DIRS` components.
/// Output Vec is sorted by construction (entries are pre-sorted by path).
#[must_use]
pub fn subtree_counts_from_entries(root: &Path, entries: &[WalkEntry]) -> Vec<(PathBuf, usize)> {
    let mut counts: Vec<(PathBuf, usize)> = Vec::new();
    for entry in entries {
        if entry.is_dir {
            continue;
        }
        // Skip entries whose path components contain EXCLUDED_DIRS
        if entry.path.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            crate::EXCLUDED_DIRS.contains(&s.as_ref())
        }) {
            continue;
        }
        let Ok(rel) = entry.path.strip_prefix(root) else {
            continue;
        };
        if let Some(first) = rel.components().next() {
            let key = root.join(first);
            match counts.last_mut() {
                Some(last) if last.0 == key => last.1 += 1,
                _ => counts.push((key, 1)),
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_ref_injection_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_path = tmp.path();

        // --output=/tmp/evil should be rejected (starts with -)
        let result = changed_files_from_git_ref(tmp_path, "--output=/tmp/evil");
        assert!(result.is_err(), "should reject git_ref starting with '-'");

        // double-dash alone should be rejected (starts with -)
        let result = changed_files_from_git_ref(tmp_path, "--");
        assert!(result.is_err(), "should reject git_ref starting with '-'");

        // git_ref with spaces should be rejected
        let result = changed_files_from_git_ref(tmp_path, "main branch");
        assert!(result.is_err(), "should reject git_ref with spaces");

        // empty git_ref should be rejected
        let result = changed_files_from_git_ref(tmp_path, "");
        assert!(result.is_err(), "should reject empty git_ref");

        // valid refs should pass validation (may fail on git not found, but not on validation)
        // HEAD~1 is valid
        let result = changed_files_from_git_ref(tmp_path, "HEAD~1");
        // We expect a git error (not a git repo), not a validation error
        if let Err(TraversalError::GitError(msg)) = result {
            assert!(
                !msg.contains("invalid git_ref"),
                "HEAD~1 should pass validation"
            );
        }

        // main is valid
        let result = changed_files_from_git_ref(tmp_path, "main");
        if let Err(TraversalError::GitError(msg)) = result {
            assert!(
                !msg.contains("invalid git_ref"),
                "main should pass validation"
            );
        }

        // abc123 is valid
        let result = changed_files_from_git_ref(tmp_path, "abc123");
        if let Err(TraversalError::GitError(msg)) = result {
            assert!(
                !msg.contains("invalid git_ref"),
                "abc123 should pass validation"
            );
        }
    }

    #[test]
    fn test_walk_directory_depth1_sync_matches_depth2_filtered() {
        // Create a small test directory structure
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Create: root/file1.txt, root/dir1/, root/dir1/file2.txt, root/dir1/dir2/file3.txt
        std::fs::write(root.join("file1.txt"), b"content1").unwrap();
        std::fs::create_dir(root.join("dir1")).unwrap();
        std::fs::write(root.join("dir1/file2.txt"), b"content2").unwrap();
        std::fs::create_dir(root.join("dir1/dir2")).unwrap();
        std::fs::write(root.join("dir1/dir2/file3.txt"), b"content3").unwrap();

        // Walk with max_depth=1 (sync walker)
        let entries_depth1 = walk_directory(root, Some(1)).unwrap();

        // Walk with max_depth=2 (parallel walker)
        let entries_depth2 = walk_directory(root, Some(2)).unwrap();

        // Filter depth2 entries to only those at depth <= 1
        let entries_depth2_filtered: Vec<_> =
            entries_depth2.iter().filter(|e| e.depth <= 1).collect();

        // Both should have the same number of entries at depth <= 1
        assert_eq!(
            entries_depth1.len(),
            entries_depth2_filtered.len(),
            "depth=1 sync walker should match depth=2 filtered to depth<=1"
        );

        // Verify that all entries in depth1 are present in depth2_filtered
        for entry1 in &entries_depth1 {
            let found = entries_depth2_filtered.iter().any(|e2| {
                e2.path == entry1.path && e2.depth == entry1.depth && e2.is_dir == entry1.is_dir
            });
            assert!(
                found,
                "entry {:?} from depth=1 not found in depth=2 filtered",
                entry1.path
            );
        }
    }
}
