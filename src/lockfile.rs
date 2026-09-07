//! A lock protocol compatible with the one Claude Code uses.
//!
//! Claude Code guards every credential mutation with a `proper-lockfile` lock
//! on `<storageDir>/.storage-write`. That library implements a lock as a
//! **directory** `<path>.lock` created with `mkdir` (an atomic test-and-set)
//! whose mtime is refreshed periodically as a heartbeat. A lock that stops
//! beating is treated as abandoned.
//!
//! Speaking the same protocol means we mutually exclude with Claude Code
//! itself, which is far stronger than any private lock it would not know about.
//!
//! The parameters match what the shipped binary passes for `.storage-write`:
//!
//! ```text
//! lockfile(join(storageDir, ".storage-write"), {
//!   realpath: false,
//!   retries: { retries: 10, minTimeout: 100, maxTimeout: 1000 },
//!   stale: 15000,
//! })
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::error::CcredError;

/// How long without a heartbeat before a lock counts as abandoned.
const STALE: Duration = Duration::from_millis(15_000);
/// How often we refresh the mtime while holding the lock.
const HEARTBEAT: Duration = Duration::from_millis(5_000);

/// A held lock. Released on `Drop`.
#[derive(Debug)]
pub struct DirLock {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl DirLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        // The directory is normally empty; remove_dir_all is a fallback in
        // case something was left inside.
        if fs::remove_dir(&self.path).is_err() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Lock path for a guarded file: `<path>.lock`.
pub fn lock_path_for(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// Refresh the lock's heartbeat.
///
/// Creating and immediately deleting a file inside the directory updates the
/// **directory's** mtime, which is exactly what `proper-lockfile` reads on the
/// other side. Merely rewriting a marker file would not change the directory's
/// mtime, and Claude Code would treat our live lock as abandoned after 15s and
/// steal it.
///
/// Side benefit: the directory stays empty, so `Drop` can clear it with a plain
/// `remove_dir`.
fn touch(dir: &Path) {
    let beat = dir.join("beat");
    if fs::write(&beat, b"1").is_ok() {
        let _ = fs::remove_file(&beat);
    }
}

/// How long the lock has gone without a heartbeat. Reads the directory mtime,
/// the same quantity `proper-lockfile` uses.
fn age_of(dir: &Path) -> Option<Duration> {
    fs::metadata(dir).ok()?.modified().ok()?.elapsed().ok()
}

/// Acquire a lock over `target`, waiting at most `timeout`.
pub fn acquire(target: &Path, timeout: Duration) -> crate::Result<DirLock> {
    let lock = lock_path_for(target);
    if let Some(parent) = lock.parent() {
        fs::create_dir_all(parent).map_err(|e| CcredError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let start = Instant::now();
    loop {
        match fs::create_dir(&lock) {
            Ok(()) => {
                touch(&lock);
                let stop = Arc::new(AtomicBool::new(false));
                let beat_stop = Arc::clone(&stop);
                let beat_path = lock.clone();
                let handle = std::thread::spawn(move || {
                    while !beat_stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(200));
                        if beat_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        if age_of(&beat_path).is_some_and(|a| a >= HEARTBEAT) {
                            touch(&beat_path);
                        }
                    }
                });
                return Ok(DirLock {
                    path: lock,
                    stop,
                    handle: Some(handle),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Clean up a lock abandoned by a crashed process.
                if age_of(&lock).is_some_and(|a| a > STALE) {
                    let _ = fs::remove_dir_all(&lock);
                    continue;
                }
                if start.elapsed() > timeout {
                    return Err(CcredError::UnsafeWrite(format!(
                        "credential store is locked by another process ({})",
                        lock.display()
                    )));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(CcredError::Io {
                    path: lock.clone(),
                    source: e,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lock_path_appends_dot_lock() {
        let p = Path::new("/x/.storage-write");
        assert_eq!(lock_path_for(p), PathBuf::from("/x/.storage-write.lock"));
    }

    #[test]
    fn acquire_and_release() {
        let dir = tempdir().unwrap();
        let target = dir.path().join(".storage-write");

        let lock = acquire(&target, Duration::from_millis(500)).unwrap();
        assert!(lock.path().is_dir(), "the lock must be a directory");
        let path = lock.path().to_path_buf();
        drop(lock);
        assert!(!path.exists(), "the lock should be cleared on Drop");
    }

    #[test]
    fn second_acquire_times_out_while_held() {
        let dir = tempdir().unwrap();
        let target = dir.path().join(".storage-write");

        let _held = acquire(&target, Duration::from_millis(500)).unwrap();
        let err = acquire(&target, Duration::from_millis(300)).unwrap_err();
        assert!(matches!(err, CcredError::UnsafeWrite(_)), "{err}");
    }

    #[test]
    fn a_stale_lock_is_reclaimed() {
        let dir = tempdir().unwrap();
        let target = dir.path().join(".storage-write");
        let lock = lock_path_for(&target);

        // A lock left by a crashed process: it exists, but nobody refreshes it.
        fs::create_dir_all(&lock).unwrap();
        let old = std::time::SystemTime::now() - (STALE + Duration::from_secs(5));
        filetime::set_file_mtime(&lock, filetime::FileTime::from_system_time(old)).unwrap();

        let got = acquire(&target, Duration::from_millis(500));
        assert!(
            got.is_ok(),
            "an abandoned lock should be taken over: {got:?}"
        );
    }

    #[test]
    fn heartbeat_keeps_the_directory_empty() {
        let dir = tempdir().unwrap();
        let target = dir.path().join(".storage-write");
        let lock = acquire(&target, Duration::from_millis(500)).unwrap();
        touch(lock.path());
        let entries: Vec<_> = fs::read_dir(lock.path()).unwrap().collect();
        assert!(entries.is_empty(), "the lock directory must stay empty");
    }

    #[test]
    fn heartbeat_advances_the_directory_mtime() {
        // This is the property that stops Claude Code from stealing our lock.
        let dir = tempdir().unwrap();
        let lock = dir.path().join("x.lock");
        fs::create_dir(&lock).unwrap();
        let before = fs::metadata(&lock).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        touch(&lock);
        let after = fs::metadata(&lock).unwrap().modified().unwrap();
        assert!(after > before, "heartbeat did not advance the dir mtime");
    }
}
