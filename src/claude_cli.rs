//! Driving the real `claude` binary.
//!
//! `ccred` never contacts an OAuth endpoint itself. Keeping a profile alive
//! means running Claude Code against that profile's own credential store and
//! letting it refresh through its own code path -- with its own lock, its own
//! retry behaviour and its own client identity.
//!
//! That is not squeamishness. A refresh token is single-use and replaying one
//! is treated as theft; Claude Code holds a lock around the whole
//! read-refresh-write cycle for exactly that reason. And refresh requests made
//! from a datacentre address by something that is not Claude Code have been
//! reported to end in a hard block that only a manual login clears. Running
//! their binary sidesteps all of it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::CcredError;
use crate::store::resolve::{SCRUBBED_ENV, StorageScope, env_pairs_for};

/// What `claude auth status --json` reports.
///
/// Every field is optional except `logged_in`: the shape differs between
/// releases (2.1.236 has no `projectsDirectory`, 2.1.260 does), and a missing
/// field must degrade rather than fail.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthStatus {
    #[serde(rename = "loggedIn")]
    pub logged_in: bool,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(rename = "orgId", default)]
    pub org_id: Option<String>,
    #[serde(rename = "subscriptionType", default)]
    pub subscription_type: Option<String>,
    /// Present from 2.1.260. When it is there it doubles as an oracle: it
    /// echoes the *resolved* config directory, so we can prove the
    /// environment we passed was honoured instead of hoping it was.
    #[serde(rename = "projectsDirectory", default)]
    pub projects_directory: Option<String>,
}

/// Which invocation to use when trying to make Claude Code refresh a token.
///
/// Ordered cheapest first. Whether the cheap ones are enough is a question
/// about a specific Claude Code build, so it is answered by observation at
/// runtime rather than assumed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Probe {
    /// Free. Reports auth state; may or may not refresh an expired token.
    AuthStatus,
    /// Free. Touches the auth layer without a model call.
    McpList,
    /// Costs a negligible slice of quota, but hits the API, so it must refresh.
    MinimalPrompt,
}

impl Probe {
    pub fn args(self) -> Vec<&'static str> {
        match self {
            Probe::AuthStatus => vec!["auth", "status", "--json"],
            Probe::McpList => vec!["mcp", "list"],
            Probe::MinimalPrompt => vec![
                "-p",
                "--output-format",
                "json",
                "--max-turns",
                "1",
                "--allowedTools",
                "",
                "hi",
            ],
        }
    }

    /// The ladder, cheapest first.
    pub const LADDER: [Probe; 3] = [Probe::AuthStatus, Probe::McpList, Probe::MinimalPrompt];
}

#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl ProbeOutcome {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }

    /// Does the output say the account needs a human to log in again?
    pub fn needs_login(&self) -> bool {
        if let Ok(status) = serde_json::from_str::<AuthStatus>(self.stdout.trim())
            && !status.logged_in
        {
            return true;
        }
        let text = format!("{} {}", self.stdout, self.stderr).to_ascii_lowercase();
        text.contains("please run /login")
            || text.contains("invalid_grant")
            || text.contains("authentication_error")
            // Observed verbatim from claude 2.1.236 against a profile whose
            // refresh token had already been spent:
            //   "Failed to authenticate: OAuth session expired and could not
            //    be refreshed"
            // None of the patterns above match it, so this read as "the window
            // did not move" -- reported as broken, when what the user needed to
            // be told was to log in.
            || text.contains("could not be refreshed")
            || text.contains("oauth session expired")
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeCli {
    bin: PathBuf,
}

impl ClaudeCli {
    pub fn at(bin: PathBuf) -> Self {
        ClaudeCli { bin }
    }

    pub fn path(&self) -> &Path {
        &self.bin
    }

    /// Find the `claude` binary: an explicit override, then PATH, then the
    /// places the official installer puts it.
    pub fn discover(override_path: Option<&Path>) -> crate::Result<Self> {
        if let Some(p) = override_path {
            if p.is_file() {
                return Ok(ClaudeCli::at(p.to_path_buf()));
            }
            return Err(CcredError::UnsafeWrite(format!(
                "configured claude path does not exist: {}",
                p.display()
            )));
        }

        let exe = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };

        if let Some(path_var) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join(exe);
                if candidate.is_file() {
                    return Ok(ClaudeCli::at(candidate));
                }
            }
        }

        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let mut fallbacks: Vec<PathBuf> = Vec::new();
        if let Some(h) = &home {
            fallbacks.push(h.join(".local").join("bin").join(exe));
        }
        if !cfg!(windows) {
            fallbacks.push(PathBuf::from("/usr/bin/claude"));
            fallbacks.push(PathBuf::from("/usr/local/bin/claude"));
            fallbacks.push(PathBuf::from("/opt/homebrew/bin/claude"));
        }
        for candidate in fallbacks {
            if candidate.is_file() {
                return Ok(ClaudeCli::at(candidate));
            }
        }

        Err(CcredError::UnsafeWrite(
            "cannot find the `claude` binary; put it on PATH or set claude_path in \
             ~/.ccred/config.toml"
                .into(),
        ))
    }

    /// Run one probe against a specific credential store.
    pub fn run(
        &self,
        scope: &StorageScope,
        probe: Probe,
        timeout: Duration,
    ) -> crate::Result<ProbeOutcome> {
        let mut cmd = Command::new(&self.bin);

        // Anything that would make Claude Code authenticate as something else
        // would also mean it never refreshes the token we care about. The most
        // dangerous is CLAUDE_CODE_OAUTH_TOKEN: it forces a plaintext write,
        // which on macOS deletes the Keychain item for every session.
        for key in SCRUBBED_ENV {
            cmd.env_remove(key);
        }
        for (key, value) in env_pairs_for(scope) {
            cmd.env(key, value);
        }

        cmd.args(probe.args())
            // Never let it stop waiting for input: this runs unattended.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = run_with_timeout(cmd, timeout).map_err(|source| CcredError::Io {
            path: self.bin.clone(),
            source,
        })?;

        Ok(output)
    }

    pub fn auth_status(
        &self,
        scope: &StorageScope,
        timeout: Duration,
    ) -> crate::Result<Option<AuthStatus>> {
        let outcome = self.run(scope, Probe::AuthStatus, timeout)?;
        if !outcome.succeeded() {
            return Ok(None);
        }
        Ok(serde_json::from_str(outcome.stdout.trim()).ok())
    }
}

/// Run a command, killing it if it overruns.
///
/// Implemented with a thread rather than a dependency: the child is waited on
/// in the background so the caller can give up on it, and a stuck `claude`
/// must never wedge a scheduled run forever.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<ProbeOutcome> {
    use std::sync::mpsc;

    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut out = String::new();
        let mut err = String::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_string(&mut out);
        }
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_string(&mut err);
        }
        let _ = tx.send((out, err));
    });

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    let (out, err) = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    let _ = reader.join();

    Ok(ProbeOutcome {
        exit_code: status.and_then(|s| s.code()),
        stdout: out,
        stderr: err,
        timed_out: status.is_none(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_arguments_are_stable() {
        assert_eq!(
            Probe::AuthStatus.args(),
            vec!["auth", "status", "--json"],
            "the JSON flag is what makes the output parseable"
        );
        // The expensive rung must stay minimal: one turn, no tools.
        let prompt = Probe::MinimalPrompt.args();
        assert!(prompt.contains(&"--max-turns"));
        assert!(prompt.contains(&"1"));
        assert!(prompt.contains(&"--allowedTools"));
    }

    #[test]
    fn the_ladder_goes_cheapest_first() {
        assert_eq!(Probe::LADDER[0], Probe::AuthStatus);
        assert_eq!(*Probe::LADDER.last().unwrap(), Probe::MinimalPrompt);
    }

    #[test]
    fn auth_status_parses_the_2_1_260_shape() {
        let json = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty",
            "email":"a@example.com","orgId":"o","orgName":"Org","subscriptionType":"max",
            "analyticsDisabled":false,"projectsDirectory":"/home/user/.claude/projects"}"#;
        let s: AuthStatus = serde_json::from_str(json).unwrap();
        assert!(s.logged_in);
        assert_eq!(
            s.projects_directory.as_deref(),
            Some("/home/user/.claude/projects")
        );
    }

    #[test]
    fn auth_status_parses_the_older_shape_without_projects_directory() {
        // 2.1.236 has no projectsDirectory. A missing field must degrade, not fail.
        let json = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty",
            "email":"a@example.com","orgId":"o","orgName":"Org","subscriptionType":"max"}"#;
        let s: AuthStatus = serde_json::from_str(json).unwrap();
        assert!(s.logged_in);
        assert!(s.projects_directory.is_none());
    }

    #[test]
    fn a_logged_out_status_is_recognised_as_needing_login() {
        let outcome = ProbeOutcome {
            exit_code: Some(0),
            stdout: r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#.into(),
            stderr: String::new(),
            timed_out: false,
        };
        assert!(outcome.needs_login());
    }

    #[test]
    fn an_auth_error_on_stderr_is_recognised() {
        let outcome = ProbeOutcome {
            exit_code: Some(1),
            stdout: String::new(),
            stderr: "OAuth error: invalid_grant".into(),
            timed_out: false,
        };
        assert!(outcome.needs_login());
    }

    #[test]
    fn a_healthy_status_does_not_look_like_a_login_problem() {
        let outcome = ProbeOutcome {
            exit_code: Some(0),
            stdout: r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#
                .into(),
            stderr: String::new(),
            timed_out: false,
        };
        assert!(!outcome.needs_login());
        assert!(outcome.succeeded());
    }

    #[test]
    fn a_timeout_is_neither_success_nor_a_login_problem() {
        let outcome = ProbeOutcome {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
        };
        assert!(!outcome.succeeded());
        assert!(
            !outcome.needs_login(),
            "a hang must be retried, not escalated"
        );
    }

    #[test]
    fn discovery_reports_a_missing_override_clearly() {
        let err = ClaudeCli::discover(Some(Path::new("/nope/claude"))).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    /// Observed verbatim from claude 2.1.236 against a profile whose refresh
    /// token had already been spent. None of the older patterns matched it, so
    /// the run reported "broken" when what the user needed to be told was to
    /// log in.
    #[test]
    fn a_spent_refresh_token_is_reported_as_needing_a_login() {
        let outcome = ProbeOutcome {
            exit_code: Some(1),
            stdout: r#"{"is_error":true,"subtype":"success","result":"Failed to authenticate: OAuth session expired and could not be refreshed"}"#.to_string(),
            stderr: String::new(),
            timed_out: false,
        };
        assert!(
            outcome.needs_login(),
            "the message a real Claude Code prints must be recognised"
        );
    }
}
