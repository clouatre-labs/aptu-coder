// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Shared advisory file-lock helper built on `std::fs::File` native locking.
//!
//! Provides an RAII [`FileLockGuard`] that holds an advisory (BSD/OFC) lock on
//! a lock file. The lock is released when the guard is dropped, which closes
//! the underlying file descriptor. Two acquisition flavors are provided:
//!
//! - [`lock_shared`]: warn-and-degrade, returns `None` on any failure so read
//!   paths are never blocked by lock infrastructure issues.
//! - [`lock_exclusive`]: propagates errors so write paths can decide how to
//!   degrade.
//!
//! Lock files are opened with `create(true).write(true).truncate(false)` and
//! are 0-byte advisory control files, never written to.

use std::io;
use std::path::Path;
use tracing::warn;

/// RAII guard that releases an advisory flock on the lock file when dropped.
/// Closing the underlying file descriptor releases the lock.
pub struct FileLockGuard(
    /// Held exclusively for its `Drop` implementation: closing the file
    /// descriptor releases the advisory flock. Never read directly.
    #[expect(dead_code)]
    std::fs::File,
);

impl FileLockGuard {
    /// Wrap an already-opened file whose advisory lock has already been
    /// acquired. Lock release happens on drop.
    #[must_use]
    pub fn from_file(file: std::fs::File) -> Self {
        Self(file)
    }
}

/// Open the 0-byte advisory control file at `lock_path` without truncating.
fn open_lock_file(lock_path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
}

/// Acquire a shared (read) lock on the lock file at `lock_path`.
/// Creates the lock file if it does not exist. Lock failures degrade
/// gracefully (warn and return None) so that read availability is
/// never blocked by lock infrastructure issues.
pub fn lock_shared(lock_path: &Path) -> Option<FileLockGuard> {
    let file = match open_lock_file(lock_path) {
        Ok(f) => f,
        Err(e) => {
            warn!(
                error = %e, lock_path = %lock_path.display(),
                "failed to open lock file; proceeding without lock"
            );
            return None;
        }
    };
    match file.lock_shared() {
        Ok(()) => Some(FileLockGuard(file)),
        Err(e) => {
            warn!(
                error = %e, lock_path = %lock_path.display(),
                "failed to acquire shared lock; proceeding without lock"
            );
            None
        }
    }
}

/// Acquire an exclusive (write) lock on the lock file at `lock_path`.
/// Creates the lock file if it does not exist. Returns Err if the lock
/// file cannot be opened or if the lock acquisition fails, propagating
/// the error to the caller (which typically degrades gracefully).
pub fn lock_exclusive(lock_path: &Path) -> io::Result<FileLockGuard> {
    let file = open_lock_file(lock_path)?;
    file.lock()?;
    Ok(FileLockGuard(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_exclusive_drop_releases_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join(".lock");

        let guard = lock_exclusive(&lock_path).expect("exclusive lock");
        // While held, a second handle cannot take the lock.
        let second = std::fs::File::open(&lock_path).expect("open second handle");
        match second.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(other) => panic!("unexpected try_lock error: {other}"),
            Ok(()) => panic!("expected WouldBlock while lock held"),
        }
        drop(guard);
        // After drop, the second handle can acquire the lock.
        second.lock().expect("lock after guard drop");
    }

    #[test]
    fn lock_shared_returns_none_when_open_fails() {
        // Passing a directory path makes OpenOptions::open fail; expect
        // warn-and-None rather than a panic.
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(lock_shared(dir.path()).is_none());
    }
}
