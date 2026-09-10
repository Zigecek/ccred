//! Detecting a running Claude Code.
//!
//! Switching profiles under a live session is unsafe in one specific
//! direction: a session that already holds account A in memory will, on its
//! next token refresh, write A's new credentials into what is by then B's
//! file. The session itself keeps working -- it is the file that ends up
//! wrong.
//!
//! Claude Code writes one session file per process, named by PID, so the
//! primary signal is exact rather than a guess at a process name. Stale files
//! from crashed processes do accumulate, hence the liveness check.

use std::path::Path;

/// Session files live here, relative to the config directory.
const SESSIONS_DIR: &str = "sessions";

/// PIDs of Claude Code processes that look alive for this config directory.
pub fn running_claude_pids(claude_config_dir: &Path) -> Vec<u32> {
    let dir = claude_config_dir.join(SESSIONS_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let candidates: Vec<u32> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let stem = name.strip_suffix(".json")?;
            stem.parse::<u32>().ok()
        })
        .collect();

    if candidates.is_empty() {
        return Vec::new();
    }
    let alive = live_pids(&candidates);
    let mut out: Vec<u32> = candidates.into_iter().filter(|p| alive(*p)).collect();
    out.sort_unstable();
    out
}

/// Build a predicate answering "is this PID alive?".
///
/// Deliberately at most one process spawn per call: the naive version spawns
/// once per PID, which on a machine with a pile of stale session files turns a
/// cheap check into a visible stall.
fn live_pids(candidates: &[u32]) -> Box<dyn Fn(u32) -> bool> {
    #[cfg(target_os = "linux")]
    {
        let _ = candidates;
        // No spawn needed at all: procfs answers directly.
        Box::new(|pid: u32| Path::new(&format!("/proc/{pid}")).exists())
    }

    #[cfg(target_os = "windows")]
    {
        let _ = candidates;
        let listed = windows_pids();
        Box::new(move |pid: u32| listed.contains(&pid))
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let listed = unix_ps_pids(candidates);
        Box::new(move |pid: u32| listed.contains(&pid))
    }
}

#[cfg(target_os = "windows")]
fn windows_pids() -> std::collections::HashSet<u32> {
    use std::process::Command;
    let mut set = std::collections::HashSet::new();
    let Ok(out) = Command::new("tasklist")
        .args(["/NH", "/FO", "CSV"])
        .output()
    else {
        return set;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // "name","pid","session","#","mem"
        if let Some(field) = line.split(',').nth(1)
            && let Ok(pid) = field.trim_matches('"').trim().parse::<u32>()
        {
            set.insert(pid);
        }
    }
    set
}

#[cfg(all(unix, not(target_os = "linux")))]
fn unix_ps_pids(candidates: &[u32]) -> std::collections::HashSet<u32> {
    use std::process::Command;
    let mut set = std::collections::HashSet::new();
    let list = candidates
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let Ok(out) = Command::new("/bin/ps")
        .args(["-p", &list, "-o", "pid="])
        .output()
    else {
        return set;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            set.insert(pid);
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn session(dir: &Path, pid: u32) {
        let s = dir.join(SESSIONS_DIR);
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join(format!("{pid}.json")), b"{}").unwrap();
    }

    #[test]
    fn no_sessions_directory_means_nothing_running() {
        let dir = tempdir().unwrap();
        assert!(running_claude_pids(dir.path()).is_empty());
    }

    #[test]
    fn our_own_pid_counts_as_alive() {
        let dir = tempdir().unwrap();
        let me = std::process::id();
        session(dir.path(), me);
        assert_eq!(running_claude_pids(dir.path()), vec![me]);
    }

    #[test]
    fn a_stale_session_file_is_ignored() {
        // A crashed process leaves its file behind. Treating that as "running"
        // would block switching forever.
        let dir = tempdir().unwrap();
        session(dir.path(), 4_294_967_294); // no such process
        assert!(running_claude_pids(dir.path()).is_empty());
    }

    #[test]
    fn non_pid_filenames_are_skipped() {
        let dir = tempdir().unwrap();
        let s = dir.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("notes.txt"), b"x").unwrap();
        std::fs::write(s.join("abc.json"), b"{}").unwrap();
        assert!(running_claude_pids(dir.path()).is_empty());
    }
}
