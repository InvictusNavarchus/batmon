//! Single-instance daemon locking via standard Unix advisory file locks (`flock`).
//!
//! Prevents multiple `batmon` daemon instances from running concurrently against
//! the same database directory. If another instance is running (e.g. systemd),
//! the lock acquisition fails non-blockingly, allowing the process to inform the
//! user and exit immediately rather than competing for SQLite WAL writes.
//!
//! Kernel-managed: when the process exits or terminates (even via `SIGKILL`),
//! the kernel automatically releases the lock.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result};
use rustix::fs::{FlockOperation, flock};
use rustix::io::Errno;

/// Outcome of attempting to acquire the single-instance lock.
#[derive(Debug)]
pub enum LockOutcome {
    /// Lock was acquired successfully. The lock is held until [`LockHandle`] is dropped.
    Acquired(LockHandle),
    /// Another instance is already holding the lock, optionally with its PID.
    AlreadyRunning { pid: Option<u32> },
}

/// An active single-instance lock holding an open file descriptor.
#[derive(Debug)]
pub struct LockHandle {
    _file: File,
}

impl LockOutcome {
    /// Attempt to acquire an exclusive, non-blocking lock on `path`.
    ///
    /// If successful, the current process ID is written to the file.
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating lock directory {}", parent.display()))?;
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o644)
            .open(path)
            .with_context(|| format!("opening lock file {}", path.display()))?;

        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                let _ = file.set_len(0);
                let _ = file.seek(SeekFrom::Start(0));
                let pid = std::process::id();
                let _ = writeln!(file, "{pid}");
                let _ = file.flush();

                Ok(Self::Acquired(LockHandle { _file: file }))
            }
            Err(Errno::WOULDBLOCK) => {
                let mut content = String::new();
                let pid = file
                    .read_to_string(&mut content)
                    .ok()
                    .and_then(|_| content.trim().parse::<u32>().ok());

                Ok(Self::AlreadyRunning { pid })
            }
            Err(err) => Err(err).with_context(|| format!("locking {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lock_lifecycle() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");

        let first = LockOutcome::acquire(&lock_path).expect("first lock should succeed");
        let handle = match first {
            LockOutcome::Acquired(h) => h,
            LockOutcome::AlreadyRunning { .. } => panic!("expected Acquired"),
        };

        // Second attempt on the same lock must report AlreadyRunning
        let second = LockOutcome::acquire(&lock_path).expect("second acquire should not error");
        match second {
            LockOutcome::AlreadyRunning { pid } => {
                assert_eq!(pid, Some(std::process::id()));
            }
            LockOutcome::Acquired(_) => panic!("expected AlreadyRunning"),
        }

        // Dropping the handle releases the lock
        drop(handle);

        let third = LockOutcome::acquire(&lock_path).expect("re-acquire after drop should succeed");
        assert!(matches!(third, LockOutcome::Acquired(_)));
    }
}
