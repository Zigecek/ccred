//! A record of what the unattended runs did.
//!
//! A scheduled refresh leaves no trace anyone can read. systemd captures
//! stdout in the journal and launchd can be pointed at a file, but Windows
//! Task Scheduler discards it entirely -- so on the platform where a job is
//! least visible, there is nothing at all to look at afterwards. When a
//! refresh quietly emptied a profile, the only reason it could be explained
//! was that someone happened to be watching at the time.
//!
//! # What may be written here
//!
//! Decisions and numbers. **Never rendered error messages**: a message can
//! echo its input and that input can be a token, which is why the project
//! rule is to persist error *kinds*. Every field below is an enum, a name the
//! user chose, or a count of days.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CcredError;

/// Rotate once the file passes this, keeping [`GENERATIONS`] older ones.
const MAX_BYTES: u64 = 1024 * 1024;
const GENERATIONS: usize = 3;

/// One line of the log: what a run decided, per profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub at_ms: i64,
    pub command: String,
    /// `"ran"`, or `"skipped: ..."` -- our own words, never an error's.
    pub status: String,
    pub profiles: Vec<ProfileLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileLine {
    pub name: String,
    /// The decision enum, serialised by name. Not the detail text, which is
    /// built from error messages.
    pub decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_days_before: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_days_after: Option<i64>,
}

/// Append one entry, rotating first if the file has grown past the limit.
///
/// Failure to log is never allowed to fail the command that was being logged.
/// Callers get the error so they can mention it; nothing acts on it.
pub fn append(log_dir: &Path, entry: &Entry) -> crate::Result<()> {
    fs::create_dir_all(log_dir).map_err(|source| CcredError::Io {
        path: log_dir.to_path_buf(),
        source,
    })?;
    let path = log_path(log_dir);

    if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) >= MAX_BYTES {
        rotate(&path);
    }

    let mut line = serde_json::to_string(entry).map_err(|source| CcredError::Json {
        path: path.clone(),
        source,
    })?;
    line.push('\n');

    // 0600 at creation, like everything else this tool writes. The contents
    // hold no secret by design, but profile names are the user's business and
    // the file sits among their credentials; a mode that differs from its
    // neighbours invites the question of which one is wrong.
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&path).map_err(|source| CcredError::Io {
        path: path.clone(),
        source,
    })?;
    file.write_all(line.as_bytes())
        .map_err(|source| CcredError::Io { path, source })
}

pub fn log_path(log_dir: &Path) -> PathBuf {
    log_dir.join("ccred.jsonl")
}

/// The most recent `count` entries, oldest first.
///
/// Unparseable lines are skipped rather than reported: a truncated last line
/// after a crash is exactly the situation someone is reading the log to
/// understand, and refusing to show them the rest would be perverse.
pub fn tail(log_dir: &Path, count: usize) -> Vec<Entry> {
    let Ok(text) = fs::read_to_string(log_path(log_dir)) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if entries.len() > count {
        entries.drain(..entries.len() - count);
    }
    entries
}

/// `ccred.jsonl` -> `ccred.jsonl.1`, and so on. The oldest falls off the end.
fn rotate(path: &Path) {
    let oldest = path.with_extension(format!("jsonl.{GENERATIONS}"));
    let _ = fs::remove_file(&oldest);
    for n in (1..GENERATIONS).rev() {
        let from = path.with_extension(format!("jsonl.{n}"));
        let to = path.with_extension(format!("jsonl.{}", n + 1));
        let _ = fs::rename(from, to);
    }
    let _ = fs::rename(path, path.with_extension("jsonl.1"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(status: &str) -> Entry {
        Entry {
            at_ms: 1_788_000_000_000,
            command: "refresh".to_string(),
            status: status.to_string(),
            profiles: vec![ProfileLine {
                name: "work".into(),
                decision: "skip_fresh".into(),
                window_days_before: Some(21),
                window_days_after: None,
            }],
        }
    }

    #[test]
    fn entries_round_trip_and_keep_their_order() {
        let d = tempfile::TempDir::new().unwrap();
        for s in ["ran", "skipped: last run was recent", "ran"] {
            append(d.path(), &entry(s)).unwrap();
        }
        let got = tail(d.path(), 10);
        assert_eq!(got.len(), 3);
        assert_eq!(got[1].status, "skipped: last run was recent");
        assert_eq!(got[2].status, "ran", "oldest first");
    }

    #[test]
    fn only_the_last_n_come_back() {
        let d = tempfile::TempDir::new().unwrap();
        for _ in 0..10 {
            append(d.path(), &entry("ran")).unwrap();
        }
        assert_eq!(tail(d.path(), 3).len(), 3);
    }

    /// A truncated final line is exactly what a crash leaves behind, and is
    /// the situation someone is reading the log to understand.
    #[test]
    fn a_half_written_line_does_not_hide_the_rest() {
        let d = tempfile::TempDir::new().unwrap();
        append(d.path(), &entry("ran")).unwrap();
        let mut f = OpenOptions::new()
            .append(true)
            .open(log_path(d.path()))
            .unwrap();
        f.write_all(br#"{"at_ms":1,"command":"refr"#).unwrap();
        drop(f);

        assert_eq!(tail(d.path(), 10).len(), 1);
    }

    #[test]
    fn the_log_rotates_instead_of_growing_without_bound() {
        let d = tempfile::TempDir::new().unwrap();
        let path = log_path(d.path());
        fs::create_dir_all(d.path()).unwrap();
        fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();

        append(d.path(), &entry("ran")).unwrap();

        assert!(
            path.with_extension("jsonl.1").exists(),
            "the old file is kept"
        );
        assert!(
            fs::metadata(&path).unwrap().len() < MAX_BYTES,
            "the live file starts again"
        );
        assert_eq!(tail(d.path(), 10).len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn the_log_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::TempDir::new().unwrap();
        append(d.path(), &entry("ran")).unwrap();
        let mode = fs::metadata(log_path(d.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "group and other must have nothing: {mode:o}"
        );
    }

    /// The project rule is to persist error kinds, never rendered messages: a
    /// message can echo its input and that input can be a token. The record
    /// therefore has no free-text field at all.
    #[test]
    fn nothing_in_an_entry_carries_a_rendered_error() {
        let json = serde_json::to_string(&entry("ran")).unwrap();
        assert!(!json.contains("detail"), "{json}");
        assert!(!json.contains("sk-ant-"), "{json}");
    }
}
