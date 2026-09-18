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
//!
//! Not every live session is in that danger. Claude Desktop logs in on its
//! own and hands each session it spawns its own access token -- through a
//! file descriptor on Linux and macOS, through `CLAUDE_CODE_OAUTH_TOKEN` on
//! Windows -- which Claude Code prefers over the credential file, and when
//! that token expires the session asks the Desktop for another rather than
//! refreshing from the file. Such a session never reads or writes the store,
//! and its account is whatever the Desktop is logged in as, whatever the file
//! says. Refusing to switch because of one only trains people to pass
//! `--force`, which defeats the check for the sessions it exists for. The
//! session file records which kind a process is, in `entrypoint`.

use std::path::Path;

use serde::Deserialize;

/// Session files live here, relative to the config directory.
const SESSIONS_DIR: &str = "sessions";

// The supervisor in `daemon.status.json` is deliberately NOT counted.
//
// Claude Code 2.1.x keeps a daemon: `daemon.status.json` names its
// supervisor PID, and `daemon-auth-status.json` shows it doing something with
// authentication -- on the machine this was written on it sat at
// `"status": "auth_required"` with no workers, for hours. It writes no
// session file, so nothing here sees it.
//
// Counting it was considered and rejected: the daemon appears to be
// permanently resident, so a switch would need `--force` every time, on every
// machine, which teaches people to pass `--force` and costs the guard its
// meaning. The guard is about a session holding an account *in memory* and
// writing its next rotated token into what is by then another profile's file;
// whether the daemon does that is not known, and blocking on a guess is worse
// than the documented gap. If a daemon ever turns out to refresh credentials
// on its own, this is the place to add it -- and the switch report already
// tells people to restart Claude Code afterwards.

/// The `entrypoint` values Claude Code records when Claude Desktop started
/// it.
///
/// `claude-desktop` was read out of `sessions/<pid>.json` written by Claude
/// Code 2.1.266 under Claude Desktop 1.52386.3. `local-agent` is the one
/// the Desktop composes for an agent-mode session -- in its bundle,
/// `CLAUDE_CODE_ENTRYPOINT:"local-agent"` sits in the same environment as
/// `oauthToken`, which is the Desktop's own -- and `local_agent` is the
/// spelling beside it in the app's list of known entrypoints. Treating
/// those as store-backed made a Cowork session running in the Desktop
/// demand `--force` for a Claude Code switch that has nothing to do with
/// it.
///
/// The other values seen in the binary -- `cli`, `claude-vscode`,
/// `sdk-cli` and so on -- log in through the store.
const DESKTOP_ENTRYPOINTS: [&str; 3] = ["claude-desktop", "local-agent", "local_agent"];

/// The sessions that look alive, split by what a switch means to them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunningSessions {
    /// Logged in through the credential store: a terminal, the VS Code
    /// extension, an SDK. Each holds the live account in memory and writes
    /// its next refreshed token back into the file, so a switch under one
    /// corrupts the new profile.
    pub store: Vec<u32>,
    /// Started by Claude Desktop, on the Desktop's own login. A switch
    /// neither reaches nor endangers them.
    pub desktop: Vec<u32>,
}

impl RunningSessions {
    pub fn is_empty(&self) -> bool {
        self.store.is_empty() && self.desktop.is_empty()
    }
}

/// The part of a session file this module reads.
#[derive(Deserialize)]
struct SessionFile {
    #[serde(default)]
    entrypoint: Option<String>,
}

/// Claude Code processes that look alive for this config directory.
pub fn running_sessions(claude_config_dir: &Path) -> RunningSessions {
    let dir = claude_config_dir.join(SESSIONS_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return RunningSessions::default();
    };

    let mut candidates: Vec<u32> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            pid_in_session_name(&name)
        })
        .collect();
    candidates.sort_unstable();
    candidates.dedup();

    if candidates.is_empty() {
        return RunningSessions::default();
    }
    let alive = live_pids(&candidates);
    let mut out = RunningSessions::default();
    for pid in candidates {
        let running = match alive(pid) {
            Liveness::Gone => false,
            // One candidate per line: any of them may identify it.
            Liveness::Running(Some(names)) => names.lines().any(could_be_claude),
            // Alive, but the name could not be read: assume the worst.
            Liveness::Running(None) => true,
        };
        if !running {
            continue;
        }
        if is_desktop_session(&dir.join(format!("{pid}.json"))) {
            out.desktop.push(pid);
        } else {
            out.store.push(pid);
        }
    }
    out.store.sort_unstable();
    out.desktop.sort_unstable();
    out
}

/// The PID a session file is named for, if it is one.
///
/// Measured against 2.1.x, a live session leaves two files: `<pid>.json` and
/// `<pid>.<64 hex>.key`. Only the first was read, so a session that had
/// written its key and not yet its json was invisible to the guard that
/// refuses to switch under a live session -- the one situation where
/// switching corrupts a profile. Both are counted now; a number belonging to
/// nothing is thrown out by the liveness check either way.
///
/// A session known only by its key file has no `entrypoint` to read, so
/// `running_sessions` counts it as store-backed: the answer that refuses
/// a switch rather than risks one.
fn pid_in_session_name(name: &str) -> Option<u32> {
    if !(name.ends_with(".json") || name.ends_with(".key")) {
        return None;
    }
    name.split('.').next()?.parse::<u32>().ok()
}

/// Is there a process with this PID at all, whatever it is?
///
/// For lock files that name their holder by PID: a lock whose holder is gone
/// is stale, and a PID that was reused is still "alive" here, which is the
/// answer that refuses rather than risks.
pub fn is_alive(pid: u32) -> bool {
    live_pids(&[pid])(pid) != Liveness::Gone
}

/// Did Claude Desktop start the session this file describes?
///
/// Anything short of a clear yes is a no: a file that cannot be read or
/// parsed, or one without the field, describes a session that is treated as
/// store-backed -- the answer that blocks a switch rather than risks one.
fn is_desktop_session(session_file: &Path) -> bool {
    std::fs::read(session_file)
        .ok()
        .and_then(|raw| serde_json::from_slice::<SessionFile>(&raw).ok())
        .and_then(|s| s.entrypoint)
        .is_some_and(|e| DESKTOP_ENTRYPOINTS.contains(&e.as_str()))
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

    /// Both names a live session writes, and nothing else.
    #[test]
    fn a_session_is_recognised_by_either_file_it_writes() {
        assert_eq!(pid_in_session_name("13512.json"), Some(13512));
        assert_eq!(
            pid_in_session_name(
                "13512.3c4ebf68dab1c2747b77a6616bb46165723ee8a9fcc422336fc2bf8ca4c459ea.key"
            ),
            Some(13512)
        );
        assert_eq!(pid_in_session_name("abc.json"), None);
        assert_eq!(pid_in_session_name("13512.txt"), None);
        assert_eq!(pid_in_session_name("13512"), None);
        assert_eq!(pid_in_session_name(".json"), None);
    }

    fn session(dir: &Path, pid: u32) {
        let s = dir.join(SESSIONS_DIR);
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join(format!("{pid}.json")), b"{}").unwrap();
    }

    #[test]
    fn no_sessions_directory_means_nothing_running() {
        let dir = tempdir().unwrap();
        assert!(running_sessions(dir.path()).is_empty());
    }

    /// The test binary is alive but is not Claude Code: the shape of a stale
    /// session file whose PID was handed to another program.
    #[test]
    fn a_reused_pid_held_by_another_program_is_ignored() {
        let dir = tempdir().unwrap();
        session(dir.path(), std::process::id());
        assert!(running_sessions(dir.path()).is_empty());
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
        assert!(running_sessions(dir.path()).is_empty());
    }

    /// Only an entrypoint the Desktop composes clears a session. Every
    /// other answer -- another entrypoint, none, a file that is not JSON,
    /// no file -- is "store-backed", because that is the reading that
    /// refuses a switch instead of risking one.
    #[test]
    fn only_a_desktop_entrypoint_marks_a_session_as_the_desktops() {
        let dir = tempdir().unwrap();
        let file = |name: &str, body: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, body).unwrap();
            p
        };
        assert!(is_desktop_session(&file(
            "desktop.json",
            br#"{"pid":1,"entrypoint":"claude-desktop","kind":"interactive"}"#
        )));
        // An agent-mode session: the Desktop's own token in its
        // environment, and nothing of the store's.
        assert!(is_desktop_session(&file(
            "agent.json",
            br#"{"pid":2,"entrypoint":"local-agent","kind":"bg"}"#
        )));
        assert!(is_desktop_session(&file(
            "agent_underscore.json",
            br#"{"pid":3,"entrypoint":"local_agent"}"#
        )));
        for (name, body) in [
            ("cli.json", &br#"{"pid":1,"entrypoint":"cli"}"#[..]),
            ("vscode.json", br#"{"entrypoint":"claude-vscode"}"#),
            ("prefix.json", br#"{"entrypoint":"claude-desktop-3p"}"#),
            ("agentish.json", br#"{"entrypoint":"local-agentic"}"#),
            ("bare.json", b"{}"),
            ("broken.json", b"{\"entrypoint\":"),
            ("empty.json", b""),
        ] {
            assert!(!is_desktop_session(&file(name, body)), "{name}");
        }
        assert!(!is_desktop_session(&dir.path().join("missing.json")));
    }

    #[test]
    fn non_pid_filenames_are_skipped() {
        let dir = tempdir().unwrap();
        let s = dir.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("notes.txt"), b"x").unwrap();
        std::fs::write(s.join("abc.json"), b"{}").unwrap();
        assert!(running_sessions(dir.path()).is_empty());
    }
}
