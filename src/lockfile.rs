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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

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
    /// The mtime our last heartbeat left on the directory. Anything else
    /// there means the lock is no longer ours; see [`Ownership`].
    beat: Arc<Mutex<Option<SystemTime>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Whether the directory at the lock path is still the one we created.
///
/// A process that stalls for longer than `STALE` -- a laptop lid closed
/// mid-run is enough -- loses its lock by the protocol's own rules: Claude
/// Code reclaims it and creates its own directory at the same path. Nothing
/// about the path says so. The mtime does: ours is whatever our last
/// heartbeat set, and a directory made later carries a different one. This is
/// the check `proper-lockfile` itself uses to call a lock "compromised".
#[derive(Debug, PartialEq, Eq)]
enum Ownership {
    Ours,
    Lost,
}

fn ownership(dir: &Path, last_beat: Option<SystemTime>) -> Ownership {
    match (fs::metadata(dir).and_then(|m| m.modified()).ok(), last_beat) {
        (Some(now), Some(ours)) if now == ours => Ownership::Ours,
        _ => Ownership::Lost,
    }
}

/// Beat, and remember what the beat left behind.
fn beat_and_record(dir: &Path, record: &Mutex<Option<SystemTime>>) {
    touch(dir);
    let stamp = fs::metadata(dir).and_then(|m| m.modified()).ok();
    if let Ok(mut slot) = record.lock() {
        *slot = stamp;
    }
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
        let last = self.beat.lock().ok().and_then(|slot| *slot);
        // A lock that was taken over belongs to its new holder. Removing it
        // would let a third writer in beside them.
        //
        // `remove_dir`, never `remove_dir_all`: our directory is always empty
        // (see `touch`), so anything inside was put there by someone else.
        if ownership(&self.path, last) == Ownership::Ours {
            let _ = fs::remove_dir(&self.path);
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
                let beat = Arc::new(Mutex::new(None));
                beat_and_record(&lock, &beat);
                let stop = Arc::new(AtomicBool::new(false));
                let beat_stop = Arc::clone(&stop);
                let beat_record = Arc::clone(&beat);
                let beat_path = lock.clone();
                let handle = std::thread::spawn(move || {
                    while !beat_stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(200));
                        if beat_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        if age_of(&beat_path).is_none_or(|a| a < HEARTBEAT) {
                            continue;
                        }
                        let last = beat_record.lock().ok().and_then(|slot| *slot);
                        if ownership(&beat_path, last) == Ownership::Lost {
                            // Keeping someone else's lock alive would stop it
                            // ever being reclaimed if they crash.
                            break;
                        }
                        beat_and_record(&beat_path, &beat_record);
                    }
                });
                return Ok(DirLock {
                    path: lock,
                    stop,
                    beat,
                    handle: Some(handle),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Clean up a lock abandoned by a crashed process.
                //
                // The timeout is checked first, and the retry still sleeps.
                // `continue`ing straight back to the top on a reclaim that
                // cannot succeed -- a sharing violation on Windows, a
                // directory owned by another user, a read-only parent --
                // spun at full CPU for ever and never consulted the caller's
                // deadline at all. An unattended run would wedge a scheduler
                // slot indefinitely.
                if start.elapsed() > timeout {
                    return Err(CcredError::Busy(format!(
                        "credential store is locked by another process ({})",
                        lock.display()
                    )));
                }
                if age_of(&lock).is_some_and(|a| a > STALE) {
                    let _ = fs::remove_dir_all(&lock);
                }
                if start.elapsed() > timeout {
                    // Busy, not UnsafeWrite. Nothing is wrong: Claude Code
                    // takes this lock on every refresh, and the right
                    // response is to come back shortly, which is what exit 6
                    // tells a scheduler.
                    return Err(CcredError::Busy(format!(
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
        // Busy, not UnsafeWrite: a held lock is the most ordinary thing here
        // -- Claude Code takes it on every refresh -- and a scheduler has to
        // tell "come back shortly" apart from "stop, something is wrong".
        assert!(matches!(err, CcredError::Busy(_)), "{err}");
        assert_eq!(err.exit_code(), crate::error::ExitCode::Busy);
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

    /// Stand in for another process reclaiming our lock: its directory, at
    /// our path, made at a different moment.
    fn taken_over(lock: &Path, age: Duration) {
        fs::remove_dir(lock).unwrap();
        fs::create_dir(lock).unwrap();
        let then = std::time::SystemTime::now() - age;
        filetime::set_file_mtime(lock, filetime::FileTime::from_system_time(then)).unwrap();
    }

    #[test]
    fn a_lock_taken_over_while_stalled_is_not_removed() {
        let dir = tempdir().unwrap();
        let target = dir.path().join(".storage-write");
        let lock = acquire(&target, Duration::from_millis(500)).unwrap();
        let path = lock.path().to_path_buf();

        taken_over(&path, Duration::from_secs(1));
        drop(lock);
        assert!(
            path.is_dir(),
            "dropping a lost lock deleted the new holder's lock"
        );
    }

    #[test]
    fn a_lock_taken_over_is_not_kept_alive() {
        let dir = tempdir().unwrap();
        let target = dir.path().join(".storage-write");
        let lock = acquire(&target, Duration::from_millis(500)).unwrap();
        let path = lock.path().to_path_buf();

        // Old enough that a heartbeat is due, so only the ownership check
        // stands between the thread and a touch.
        taken_over(&path, HEARTBEAT + Duration::from_secs(1));
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after, "the heartbeat refreshed a lock it had lost");
        drop(lock);
    }

    #[test]
    fn a_held_lock_is_still_recognised_as_ours() {
        let dir = tempdir().unwrap();
        let target = dir.path().join(".storage-write");
        let lock = acquire(&target, Duration::from_millis(500)).unwrap();
        let last = lock.beat.lock().unwrap().unwrap();
        assert_eq!(ownership(lock.path(), Some(last)), Ownership::Ours);
        assert_eq!(ownership(lock.path(), None), Ownership::Lost);
    }

    /// A reclaim that cannot succeed must not become a spin.
    ///
    /// The loop used to `continue` straight back to the top after trying to
    /// remove a stale lock, without consulting the caller's deadline or
    /// sleeping. When the removal could not work -- a sharing violation on
    /// Windows, a directory owned by someone else, a read-only parent -- that
    /// ran at full CPU for ever, and an unattended run wedged a scheduler
    /// slot indefinitely.
    #[test]
    fn a_stale_lock_that_cannot_be_removed_still_times_out() {
        let dir = tempdir().unwrap();
        let target = dir.path().join(".storage-write");
        let lock = lock_path_for(&target);
        fs::create_dir_all(&lock).unwrap();

        // Old enough to look stale, with something inside that a removal on a
        // locked file would trip over. Whether the removal succeeds here is
        // beside the point: what is pinned is that the deadline is honoured.
        fs::write(lock.join("keep"), b"x").unwrap();
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
        let _ = filetime::set_file_mtime(&lock, long_ago.into());

        let started = std::time::Instant::now();
        let _ = acquire(&target, Duration::from_millis(200));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "acquire must return on its deadline, took {:?}",
            started.elapsed()
        );
    }
}
