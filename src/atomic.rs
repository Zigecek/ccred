//! Atomic writes for credential files.
//!
//! Rules that hold here without exception:
//!
//! * **Never write through a symlink.** Claude Code itself refuses symlinked
//!   credentials (`refused-symlink`); for us it is also a security matter -- a
//!   symlink would redirect a 0600 write somewhere else entirely.
//! * **Mode 0600 at creation**, not after the write. Otherwise there is a
//!   window in which the token is world-readable.
//! * **Temp file in the same directory**, so the rename is atomic -- across a
//!   filesystem boundary it is not.
//! * **`fsync` before the rename**, so a crash cannot leave an empty file.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::CcredError;

fn io_err(path: &Path, source: std::io::Error) -> CcredError {
    CcredError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Removes the temp file if anything between creation and rename goes wrong.
struct TempGuard(Option<PathBuf>);

impl TempGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

/// Temp file name. No RNG dependency -- it only needs to keep two concurrent
/// processes from picking the same name.
fn temp_name(target: &Path) -> String {
    let stem = target
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "ccred".to_string());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(".{stem}.tmp.{}.{nanos:08x}", std::process::id())
}

/// Write `bytes` to `path` atomically. `private` means mode 0600 on Unix.
pub fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> crate::Result<()> {
    // Refuse a symlink before creating anything.
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        return Err(CcredError::RefusedSymlink(path.to_path_buf()));
    }

    let dir = path
        .parent()
        .ok_or_else(|| io_err(path, std::io::Error::other("path has no parent directory")))?;
    fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;

    let tmp = dir.join(temp_name(path));

    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    // On Windows `mode` is meaningless -- the file inherits its DACL from the
    // parent directory. Explicit ACL hardening lands together with the
    // scheduler; until then this matches what Claude Code itself does.
    #[cfg(not(unix))]
    let _ = private;

    let mut file = opts.open(&tmp).map_err(|e| io_err(&tmp, e))?;
    let mut guard = TempGuard(Some(tmp.clone()));

    file.write_all(bytes).map_err(|e| io_err(&tmp, e))?;
    file.sync_all().map_err(|e| io_err(&tmp, e))?;
    drop(file);

    fs::rename(&tmp, path).map_err(|e| io_err(path, e))?;
    guard.disarm();

    // Make the rename itself durable.
    #[cfg(unix)]
    if let Ok(dir_handle) = fs::File::open(dir) {
        let _ = dir_handle.sync_all();
    }

    Ok(())
}

/// File mode in octal; `None` on non-Unix platforms.
#[cfg(unix)]
pub fn mode_of(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
pub fn mode_of(_path: &Path) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_and_replaces_content() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("creds.json");

        write_atomic(&target, b"first", true).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"first");

        write_atomic(&target, b"second", true).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
    }

    #[test]
    fn leaves_no_temp_files_behind() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("creds.json");
        write_atomic(&target, b"x", true).unwrap();

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn creates_parent_directories() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("a").join("b").join("creds.json");
        write_atomic(&target, b"x", true).unwrap();
        assert!(target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_0600() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("creds.json");
        write_atomic(&target, b"secret", true).unwrap();
        assert_eq!(mode_of(&target), Some(0o600));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_write_through_a_symlink() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("elsewhere.json");
        fs::write(&real, b"original").unwrap();
        let link = dir.path().join("creds.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = write_atomic(&link, b"attack", true).unwrap_err();
        assert!(matches!(err, CcredError::RefusedSymlink(_)), "{err}");
        // The symlink target must be untouched.
        assert_eq!(fs::read(&real).unwrap(), b"original");
    }
}
