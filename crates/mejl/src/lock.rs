//! One mejl per account.
//!
//! mejl-store expects exactly one writer per account directory, and has no
//! lock of its own (std's `File::lock` needs Rust 1.89, above the 1.88
//! MSRV). On Unix this takes an advisory `flock` on `<account>/mejl.lock`.
//! The kernel drops it when the process exits, even after a crash, so a
//! stale lock is impossible. std opens files with `O_CLOEXEC`, so an
//! editor or browser started by mejl never inherits it.
//!
//! Elsewhere the fallback is a lock file created exclusively and removed on
//! drop. A crash can leave it behind; the error message says which file to
//! delete.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The lock file name inside an account directory.
pub const LOCK_FILE: &str = "mejl.lock";

/// How long [`InstanceLock::acquire`] retries a held lock.
pub const RETRY_FOR: std::time::Duration = std::time::Duration::from_millis(100);

/// Why the lock could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another mejl has this account open.
    #[error("{}", held_message(path, *pid))]
    Held {
        /// The lock file.
        path: PathBuf,
        /// The other process, when it wrote its id.
        pid: Option<u32>,
    },
    /// The lock file cannot be opened or locked.
    #[error("cannot lock {path}: {source}")]
    Io {
        /// The lock file.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
}

fn held_message(path: &Path, pid: Option<u32>) -> String {
    let who = match pid {
        Some(pid) => format!("another mejl (process {pid})"),
        None => "another mejl".to_owned(),
    };
    let mut msg = format!("{who} already has this account open; close it first");
    if cfg!(not(unix)) {
        msg.push_str(&format!(
            ". If no mejl is running, delete {}",
            path.display()
        ));
    }
    msg
}

/// A held instance lock. Released on drop.
#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
    // Kept open: closing the file would release the flock.
    _file: File,
}

impl InstanceLock {
    /// Takes the lock for an account directory, which must exist. Fails
    /// with [`LockError::Held`] when another process has it.
    ///
    /// A just-released lock can look held for a moment: a process that
    /// forks while holding it gives the child a copy of the descriptor
    /// until the child's `exec` closes it. So a held lock is retried a few
    /// times over about [`RETRY_FOR`] before giving up. A running mejl
    /// holds its lock for its whole life, so this never hides one.
    pub fn acquire(account_dir: &Path) -> Result<InstanceLock, LockError> {
        let path = account_dir.join(LOCK_FILE);
        let deadline = std::time::Instant::now() + RETRY_FOR;
        let file = loop {
            match imp::lock(&path) {
                Err(LockError::Held { .. }) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(RETRY_FOR / 10);
                }
                other => break other?,
            }
        };
        let mut lock = InstanceLock { path, _file: file };
        // The id is only for the message a second mejl prints.
        lock.write_pid();
        Ok(lock)
    }

    /// The lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write_pid(&mut self) {
        let _ = self._file.set_len(0);
        let _ = writeln!(self._file, "{}", std::process::id());
        let _ = self._file.flush();
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        imp::unlock(&self.path);
    }
}

/// Whether another process holds the lock, for `mejl doctor`. A directory
/// without a lock file is unlocked.
pub fn is_held(account_dir: &Path) -> bool {
    if !account_dir.join(LOCK_FILE).exists() {
        return false;
    }
    matches!(
        InstanceLock::acquire(account_dir),
        Err(LockError::Held { .. })
    )
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(unix)]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    use super::{LockError, read_pid};

    pub(super) fn lock(path: &Path) -> Result<File, LockError> {
        let io_err = |source| LockError::Io {
            path: path.to_owned(),
            source,
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .map_err(io_err)?;
        loop {
            // SAFETY: flock only reads the descriptor, which `file` keeps
            // open for the duration of the call.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(file);
            }
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => {
                    return Err(LockError::Held {
                        path: path.to_owned(),
                        pid: read_pid(path),
                    });
                }
                _ => return Err(io_err(err)),
            }
        }
    }

    // Closing the descriptor releases the flock. The file stays, so the next
    // mejl opens the same inode and nobody races on create and delete.
    pub(super) fn unlock(_path: &Path) {}
}

#[cfg(not(unix))]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::path::Path;

    use super::{LockError, read_pid};

    pub(super) fn lock(path: &Path) -> Result<File, LockError> {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => Ok(file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(LockError::Held {
                path: path.to_owned(),
                pid: read_pid(path),
            }),
            Err(source) => Err(LockError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    pub(super) fn unlock(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_on_the_same_account_fails() {
        let dir = tempfile::tempdir().unwrap();
        let first = InstanceLock::acquire(dir.path()).unwrap();
        assert!(first.path().ends_with(LOCK_FILE));
        let err = InstanceLock::acquire(dir.path()).unwrap_err();
        match &err {
            LockError::Held { pid, .. } => assert_eq!(*pid, Some(std::process::id())),
            other => panic!("expected Held, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("already has this account open"), "{msg}");
        assert!(is_held(dir.path()));
        drop(first);
        assert!(!is_held(dir.path()));
        let again = InstanceLock::acquire(dir.path()).unwrap();
        drop(again);
    }

    #[test]
    fn different_accounts_do_not_conflict() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let _la = InstanceLock::acquire(a.path()).unwrap();
        let _lb = InstanceLock::acquire(b.path()).unwrap();
    }

    #[test]
    fn missing_directory_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = InstanceLock::acquire(&dir.path().join("missing")).unwrap_err();
        assert!(matches!(err, LockError::Io { .. }), "{err:?}");
        assert!(!is_held(&dir.path().join("missing")));
    }

    #[test]
    fn lock_in_another_thread_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_owned();
        let held = InstanceLock::acquire(&path).unwrap();
        let other = std::thread::spawn(move || InstanceLock::acquire(&path).is_err())
            .join()
            .unwrap();
        assert!(other);
        drop(held);
    }

    #[cfg(unix)]
    #[test]
    fn lock_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let lock = InstanceLock::acquire(dir.path()).unwrap();
        let mode = std::fs::metadata(lock.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
