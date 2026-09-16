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
//! from crashed processes do accumulate, hence the liveness check -- and
//! because a PID is reused, especially on Windows, "alive" also means "a
//! process that could be Claude Code". A stale file whose number now belongs
//! to a browser would otherwise block switching until the file was deleted by
//! hand.

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
    let mut out: Vec<u32> = candidates
        .into_iter()
        .filter(|p| match alive(*p) {
            Liveness::Gone => false,
            // One candidate per line: any of them may identify it.
            Liveness::Running(Some(names)) => names.lines().any(could_be_claude),
            // Alive, but the name could not be read: assume the worst.
            Liveness::Running(None) => true,
        })
        .collect();
    out.sort_unstable();
    out
}

/// What is known about one PID.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Liveness {
    Gone,
    /// Running, with its executable name when that could be read.
    Running(Option<String>),
}

/// Could a process of this name be Claude Code?
///
/// The native build runs as `claude`; an npm install runs under `node` (or
/// `bun`). Anything else holding the PID is a different program that
/// inherited the number.
///
/// The native installer on Linux and macOS runs a file named after its
/// version, `~/.local/share/claude/versions/2.1.236`, so a path with a
/// `claude` directory in it counts, and so does a bare version number -- all
/// that `comm` shows for such a process.
fn could_be_claude(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    // A replaced executable reads back as "<path> (deleted)".
    let name = name.strip_suffix(" (deleted)").unwrap_or(&name);
    let mut parts = name.rsplit(['/', '\\']);
    let base = parts.next().unwrap_or(name);
    let base = base.strip_suffix(".exe").unwrap_or(base);
    let versioned = base.contains('.') && base.chars().all(|c| c.is_ascii_digit() || c == '.');
    base.starts_with("claude")
        || is_js_runtime(base)
        || versioned
        || parts.any(|dir| dir == "claude")
}

/// `node`, `nodejs` or `bun`, alone or with a version: Fedora installs the
/// real binary as `/usr/bin/node-22`, and `/proc/<pid>/exe` names that
/// rather than the `node` link, so an npm install of Claude Code there read
/// as "not running" and a switch went ahead under it.
fn is_js_runtime(base: &str) -> bool {
    ["nodejs", "node", "bun"].iter().any(|name| {
        base.strip_prefix(name).is_some_and(|rest| {
            rest.trim_start_matches(['-', '_'])
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.')
        })
    })
}

/// Build a predicate answering "is this PID alive, and as what?".
///
/// Deliberately at most one process spawn per call: the naive version spawns
/// once per PID, which on a machine with a pile of stale session files turns a
/// cheap check into a visible stall.
fn live_pids(candidates: &[u32]) -> Box<dyn Fn(u32) -> Liveness> {
    #[cfg(target_os = "linux")]
    {
        let _ = candidates;
        // No spawn needed at all: procfs answers directly.
        Box::new(|pid: u32| {
            let dir = std::path::PathBuf::from(format!("/proc/{pid}"));
            if !dir.exists() {
                return Liveness::Gone;
            }
            // The executable's path says the most; `comm` is the fallback, and
            // either can be unreadable under `hidepid`, which is "unknown".
            //
            // Both are read, and either may identify it: an interpreter's
            // path can be a name this check does not know while `comm` is
            // plain `node`, and the other way round.
            let names: Vec<String> = [
                std::fs::read_link(dir.join("exe"))
                    .map(|p| p.to_string_lossy().into_owned())
                    .ok(),
                std::fs::read_to_string(dir.join("comm")).ok(),
            ]
            .into_iter()
            .flatten()
            .collect();
            if names.is_empty() {
                Liveness::Running(None)
            } else {
                Liveness::Running(Some(names.join("\n")))
            }
        })
    }

    #[cfg(not(target_os = "linux"))]
    {
        #[cfg(target_os = "windows")]
        let listed = {
            let _ = candidates;
            windows_pids()
        };
        #[cfg(not(target_os = "windows"))]
        let listed = unix_ps_pids(candidates);
        Box::new(move |pid: u32| match listed.get(&pid) {
            Some(name) if name.trim().is_empty() => Liveness::Running(None),
            Some(name) => Liveness::Running(Some(name.clone())),
            None => Liveness::Gone,
        })
    }
}

/// The quoted fields of one `tasklist /FO CSV` line.
///
/// Split on the quotes, not the commas: the memory column is written with
/// the locale's thousands separator, which is a comma in many of them.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn csv_fields(line: &str) -> Vec<&str> {
    line.split('"').skip(1).step_by(2).collect()
}

#[cfg(target_os = "windows")]
fn windows_pids() -> std::collections::HashMap<u32, String> {
    use std::process::Command;
    let mut map = std::collections::HashMap::new();
    let Ok(out) = Command::new("tasklist")
        .args(["/NH", "/FO", "CSV"])
        .output()
    else {
        return map;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // "name","pid","session","#","mem"
        let fields = csv_fields(line);
        if let (Some(name), Some(pid)) = (fields.first(), fields.get(1))
            && let Ok(pid) = pid.trim().parse::<u32>()
        {
            map.insert(pid, name.to_string());
        }
    }
    map
}

#[cfg(all(unix, not(target_os = "linux")))]
fn unix_ps_pids(candidates: &[u32]) -> std::collections::HashMap<u32, String> {
    use std::process::Command;
    let mut map = std::collections::HashMap::new();
    let list = candidates
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let Ok(out) = Command::new("/bin/ps")
        .args(["-p", &list, "-o", "pid=,comm="])
        .output()
    else {
        return map;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // `comm` can be a path with spaces in it; everything after the pid.
        let line = line.trim_start();
        let (pid, name) = line.split_once(' ').unwrap_or((line, ""));
        if let Ok(pid) = pid.parse::<u32>() {
            map.insert(pid, name.trim().to_string());
        }
    }
    map
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

    /// The test binary is alive but is not Claude Code: the shape of a stale
    /// session file whose PID was handed to another program.
    #[test]
    fn a_reused_pid_held_by_another_program_is_ignored() {
        let dir = tempdir().unwrap();
        session(dir.path(), std::process::id());
        assert!(running_claude_pids(dir.path()).is_empty());
    }

    #[test]
    fn the_names_claude_code_runs_under_are_recognised() {
        for name in [
            "claude",
            "claude.exe",
            "Claude.EXE",
            "node",
            "node.exe",
            "bun",
            "/opt/homebrew/bin/node",
            r"C:\Program Files\nodejs\node.exe",
            "claude\n",
            "/home/x/.local/share/claude/versions/2.1.236",
            "/home/x/.local/share/claude/versions/2.1.236 (deleted)",
            "2.1.236",
        ] {
            assert!(could_be_claude(name), "{name:?}");
        }
        for name in [
            "node-22",
            "/usr/bin/node-22",
            "node18",
            "nodejs-20.1",
            "bun-1.1",
        ] {
            assert!(could_be_claude(name), "{name:?}");
        }
        for name in [
            "chrome.exe",
            "svchost.exe",
            "bash",
            "ccred",
            "nodemon",
            "node-red",
            "bunzip2",
        ] {
            assert!(!could_be_claude(name), "{name:?}");
        }
    }

    #[test]
    fn a_localised_tasklist_line_still_yields_its_pid() {
        let line = r#""claude.exe","13512","Console","1","118,708 K""#;
        assert_eq!(
            csv_fields(line),
            ["claude.exe", "13512", "Console", "1", "118,708 K"]
        );
        let odd = r#""a,b.exe","7","Services","0","1,024 K""#;
        assert_eq!(csv_fields(odd)[..2], ["a,b.exe", "7"]);
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
