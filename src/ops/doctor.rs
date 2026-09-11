//! `doctor`: everything that could quietly be wrong, checked in one place.

use serde::Serialize;

use super::{Ctx, days_until};
use crate::journal::SwitchJournal;
use crate::lockfile::lock_path_for;
use crate::paths::storage_write_lock_target;
use crate::proc::running_claude_pids;
use crate::schedule::{State, Warning, detect};
use crate::store::{CredentialStore, now_ms};
use crate::validate::ProfileName;
use crate::validate::validate_credentials;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Finding {
    fn ok(title: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Ok,
            title: title.into(),
            detail: None,
        }
    }
    fn warn(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Warn,
            title: title.into(),
            detail: Some(detail.into()),
        }
    }
    fn error(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Error,
            title: title.into(),
            detail: Some(detail.into()),
        }
    }
}

/// Warn this many days before a refresh token dies.
const EXPIRY_WARN_DAYS: i64 = 5;

pub fn doctor(ctx: &Ctx) -> crate::Result<Vec<Finding>> {
    let now = now_ms();
    let mut findings = Vec::new();

    // --- layout -----------------------------------------------------------
    findings.push(Finding::ok(format!(
        "config directory: {}",
        ctx.paths().claude_config_dir().display()
    )));
    findings.push(Finding::ok(format!(
        "profiles: {}",
        ctx.paths().profiles_dir().display()
    )));

    // --- an interrupted switch -------------------------------------------
    match SwitchJournal::load(&ctx.paths().switch_journal()) {
        Ok(Some(j)) => findings.push(Finding::warn(
            "an earlier switch did not finish",
            format!(
                "it stopped at {:?} while moving to '{}'; the next command will heal it",
                j.phase, j.to
            ),
        )),
        Ok(None) => {}
        Err(e) => findings.push(Finding::error(
            "switch journal is unreadable",
            e.to_string(),
        )),
    }

    // --- a lock nobody is holding ----------------------------------------
    let lock = lock_path_for(&storage_write_lock_target(ctx.paths().claude_config_dir()));
    if lock.exists() {
        findings.push(Finding::warn(
            "the credential store is locked",
            format!(
                "{} exists; if no Claude Code is running it will be reclaimed after 15s",
                lock.display()
            ),
        ));
    }

    // --- running sessions -------------------------------------------------
    let pids = running_claude_pids(ctx.paths().claude_config_dir());
    if !pids.is_empty() {
        findings.push(Finding::warn(
            "Claude Code is running",
            format!(
                "{} live; switching now needs --force and is not advised",
                match pids.len() {
                    1 => "1 session".to_string(),
                    n => format!("{n} sessions"),
                }
            ),
        ));
    }

    // --- who is live ------------------------------------------------------
    let live = ctx.live_store();
    let account = ctx.live_account();
    match live.load() {
        Ok(Some(loaded)) => match validate_credentials(&loaded.creds.oauth, now) {
            Ok(h) => {
                let days = h
                    .refresh_window_left_ms
                    .map(|ms| ms / 86_400_000)
                    .unwrap_or(0);
                if h.refresh_expired {
                    findings.push(Finding::error(
                        "the live refresh token has expired",
                        "run `claude auth login`",
                    ));
                } else {
                    findings.push(Finding::ok(format!(
                        "logged in as {} ({days} days of refresh window left)",
                        account.label()
                    )));
                }
            }
            Err(e) => findings.push(Finding::error(
                "live credentials are not usable",
                e.to_string(),
            )),
        },
        Ok(None) => findings.push(Finding::warn(
            "not logged in",
            "no live credentials; run `claude auth login`".to_string(),
        )),
        Err(e) => findings.push(Finding::error("live credentials unreadable", e.to_string())),
    }

    // --- the pointer versus reality --------------------------------------
    //
    // This is the check that would have caught a real near-miss: after logging
    // in as a second account, the pointer still named the first, and a
    // scheduled sync was minutes away from storing the wrong credentials.
    // A diagnostic command must not fail to diagnose. Anything unreadable
    // here is itself a finding -- and this is exactly the moment someone
    // reaches for `doctor`, so refusing to produce a report is the one
    // response that cannot help.
    let active = match ctx.repo().active() {
        Ok(a) => a,
        Err(e) => {
            findings.push(Finding::error(
                "the active-profile pointer is unreadable",
                e.to_string(),
            ));
            None
        }
    };
    match &active {
        None => findings.push(Finding::warn(
            "no active profile",
            "run `ccred save <name>` to record the account that is logged in",
        )),
        Some(name) => {
            if !ctx.repo().exists(name)? {
                findings.push(Finding::error(
                    "the active profile does not exist",
                    format!("the pointer names '{name}', but there is no such profile"),
                ));
            } else if let Some(meta) = ctx.repo().meta(name)?
                && account.is_known()
                && meta.account != Default::default()
                && account.identity.same_account_as(&meta.account) == Some(false)
            {
                findings.push(Finding::error(
                    "the active profile is not the account that is logged in",
                    format!(
                        "the pointer says '{}' ({}), but {} is logged in. Run \
                         `ccred save <name>` for the account you are actually using, \
                         or switch to the right profile",
                        name,
                        meta.account.label(),
                        account.label()
                    ),
                ));
            } else {
                findings.push(Finding::ok(format!("active profile: {name}")));
            }
        }
    }

    // --- each profile -----------------------------------------------------
    let profiles = match ctx.repo().list() {
        Ok(p) => p,
        Err(e) => {
            findings.push(Finding::error(
                "the profiles directory cannot be read",
                e.to_string(),
            ));
            Vec::new()
        }
    };
    let profile_count = profiles.len();
    if profiles.is_empty() {
        findings.push(Finding::warn(
            "no profiles saved",
            "run `ccred save <name>` while logged in",
        ));
    }
    for name in profiles {
        let Ok(store) = ctx.repo().store(&name) else {
            findings.push(broken_profile(
                ctx,
                &name,
                "its directory cannot be resolved",
            ));
            continue;
        };
        match store.load() {
            Ok(Some(loaded)) => match validate_credentials(&loaded.creds.oauth, now) {
                Ok(h) if h.refresh_expired => findings.push(Finding::error(
                    format!("profile '{name}' has expired"),
                    format!("switch to it and run `claude auth login`, then `ccred save {name}`"),
                )),
                Ok(_) => {
                    let days = loaded
                        .creds
                        .oauth
                        .refresh_token_expires_at
                        .map(|t| days_until(t, now));
                    match days {
                        Some(d) if d < EXPIRY_WARN_DAYS => findings.push(Finding::warn(
                            format!("profile '{name}' expires soon"),
                            format!("{d} days of refresh window left"),
                        )),
                        _ => findings.push(Finding::ok(format!("profile '{name}' is healthy"))),
                    }
                }
                Err(e) => findings.push(broken_profile(ctx, &name, &e.to_string())),
            },
            Ok(None) => findings.push(broken_profile(ctx, &name, "there are no credentials")),
            Err(e) => findings.push(Finding::error(
                format!("profile '{name}' is unreadable"),
                e.to_string(),
            )),
        }
    }

    findings.push(permissions_finding(ctx));
    findings.push(schedule_finding(detect().status(), profile_count));

    Ok(findings)
}

/// Can anyone but the owner read the stored credentials?
///
/// The writer asks for 0600, but a mode is only what was requested: an
/// umask cannot loosen it, yet a file restored from a backup, copied with
/// `cp -p`, or synced from another machine can arrive wide open. Checking the
/// result costs a stat and turns an assumption into a fact.
///
/// On Windows there is no mode to read. The inherited DACL under a default
/// profile is already owner-plus-SYSTEM-plus-Administrators -- measured, see
/// `atomic::write_atomic` -- but this cannot confirm it without an ACL API,
/// so it says so rather than implying the check passed.
fn permissions_finding(ctx: &Ctx) -> Finding {
    #[cfg(not(unix))]
    {
        let _ = ctx;
        Finding::ok("credential file permissions: inherited (not checkable here)")
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut exposed = Vec::new();
        let mut checked = 0usize;
        let mut paths = vec![ctx.paths().claude_config_dir().join(".credentials.json")];
        if let Ok(names) = ctx.repo().list() {
            for n in names {
                if let Ok(p) = ctx.paths().profile_credentials(&n) {
                    paths.push(p);
                }
            }
        }

        for path in paths {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            checked += 1;
            let mode = meta.permissions().mode();
            if mode & 0o077 != 0 {
                exposed.push(format!("{} is {:o}", path.display(), mode & 0o777));
            }
        }

        if exposed.is_empty() {
            Finding::ok(format!(
                "credential file permissions: {checked} checked, none readable by anyone else"
            ))
        } else {
            Finding::error(
                "credentials are readable by other users",
                format!("{}; run `chmod 600` on each", exposed.join(", ")),
            )
        }
    }
}

/// A profile that cannot be used, and whether there is a way back.
///
/// The last-known-good copy is written after every accepted store and was,
/// on one real machine, the only thing standing between a spawned `claude`
/// and a destroyed account. Reporting a broken profile without mentioning it
/// tells the user to log in again when they may not have to.
fn broken_profile(ctx: &Ctx, name: &ProfileName, why: &str) -> Finding {
    let recoverable = ctx.repo().last_known_good(name).ok().flatten().is_some();
    Finding::error(
        format!("profile '{name}' is unusable"),
        if recoverable {
            format!("{why}; a good earlier copy exists -- run `ccred restore {name}`")
        } else {
            format!("{why}; switch to it and run `claude auth login`, then `ccred save {name}`")
        },
    )
}

/// Is anything actually keeping the idle profiles alive?
///
/// Nothing else in this function notices when the answer is no. Every check
/// above reports on a profile as it stands today, so a machine with the
/// refresh schedule switched off passes them all and then quietly loses an
/// account a fortnight later. That is precisely the class of failure this
/// command exists to catch.
///
/// Severity tracks whether it matters rather than whether it is on: with a
/// single profile the live credentials are refreshed by Claude Code itself,
/// and a scheduler would have nothing to do.
///
/// The probe is a parameter rather than a call, so the policy can be tested
/// without a scheduler. Reading the host's real one from a test would make
/// the assertion depend on the machine running it.
fn schedule_finding(status: crate::Result<State>, profile_count: usize) -> Finding {
    let state = match status {
        Ok(state) => state,
        // Not being able to ask is itself worth reporting: it leaves the same
        // observable state as "installed but never fires".
        Err(e) => {
            return Finding::warn(
                "could not query the refresh schedule",
                format!("{e}; check it by hand with `ccred schedule status`"),
            );
        }
    };

    match state {
        State::Installed(h) if h.next_run.is_none() => Finding::error(
            "the refresh schedule is registered but will never run",
            "reinstall it with `ccred schedule install`, which verifies the next run".to_string(),
        ),
        State::Installed(h) if !h.enabled => Finding::warn(
            "the refresh schedule is installed but disabled",
            "profiles will go stale until it is enabled again".to_string(),
        ),
        State::Installed(h) => {
            // Running only while signed in is the normal outcome of a
            // non-elevated install. It is worth stating once, not worth
            // colouring the whole report yellow for ever.
            let signed_in_only = h
                .warnings
                .iter()
                .any(|w| matches!(w, Warning::RunsOnlyWhenSignedIn));
            let blocking = h
                .warnings
                .iter()
                .find(|w| !matches!(w, Warning::RunsOnlyWhenSignedIn));

            match blocking {
                Some(w) => Finding::warn(
                    "the refresh schedule may not fire",
                    format!("{w}; see `ccred schedule status`"),
                ),
                None => Finding::ok(format!(
                    "refresh scheduled, next run {}{}",
                    h.next_run.as_deref().unwrap_or("unknown"),
                    if signed_in_only {
                        " (while you are signed in)"
                    } else {
                        ""
                    }
                )),
            }
        }
        State::NotInstalled if profile_count > 1 => Finding::warn(
            "nothing is refreshing the profiles you are not using",
            "an idle profile's refresh token expires and then needs a manual login; \
             run `ccred schedule install`"
                .to_string(),
        ),
        State::NotInstalled => {
            Finding::ok("no refresh schedule, and with one profile nothing goes stale".to_string())
        }
        State::Unsupported { reason, remedy } => Finding::warn(
            "no refresh schedule is possible here",
            match remedy {
                Some(r) => format!("{reason}; {r}"),
                None => reason,
            },
        ),
    }
}

/// The worst severity present, for the exit code.
pub fn worst(findings: &[Finding]) -> Severity {
    if findings.iter().any(|f| f.severity == Severity::Error) {
        Severity::Error
    } else if findings.iter().any(|f| f.severity == Severity::Warn) {
        Severity::Warn
    } else {
        Severity::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CcredError;
    use crate::schedule::Health;

    fn health(enabled: bool, next_run: Option<&str>) -> Health {
        Health {
            enabled,
            next_run: next_run.map(str::to_string),
            last_run: None,
            warnings: Vec::new(),
        }
    }

    /// The gap this check was added to close: every other finding reports on
    /// a profile as it stands today, so a machine with no refresh schedule
    /// passed `doctor` cleanly and then lost the idle account a fortnight
    /// later. Two profiles and no schedule must not be reported as healthy.
    #[test]
    fn an_idle_profile_with_nothing_refreshing_it_is_a_warning() {
        let f = schedule_finding(Ok(State::NotInstalled), 2);
        assert_eq!(f.severity, Severity::Warn, "{f:?}");
        assert!(f.detail.unwrap().contains("ccred schedule install"));
    }

    #[test]
    fn one_profile_needs_no_schedule() {
        // Claude Code refreshes the credentials it is actually using, so a
        // single profile cannot go stale and a warning would be noise.
        assert_eq!(
            schedule_finding(Ok(State::NotInstalled), 1).severity,
            Severity::Ok
        );
    }

    /// `install_checked` already refuses to register a job with no next run,
    /// but one can stop firing later -- a disabled timer, a deleted binary.
    #[test]
    fn registered_but_never_firing_is_an_error_not_a_warning() {
        let f = schedule_finding(Ok(State::Installed(health(true, None))), 2);
        assert_eq!(f.severity, Severity::Error, "{f:?}");
    }

    #[test]
    fn a_working_schedule_reports_when_it_next_runs() {
        let f = schedule_finding(Ok(State::Installed(health(true, Some("Fri 03:00")))), 2);
        assert_eq!(f.severity, Severity::Ok);
        assert!(f.title.contains("Fri 03:00"), "{f:?}");
    }

    #[test]
    fn a_disabled_schedule_is_not_reported_as_working() {
        let f = schedule_finding(Ok(State::Installed(health(false, Some("Fri 03:00")))), 2);
        assert_eq!(f.severity, Severity::Warn, "{f:?}");
    }

    /// Failing to ask must not read as "nothing is wrong".
    #[test]
    fn an_unanswerable_probe_is_reported_rather_than_swallowed() {
        let f = schedule_finding(Err(CcredError::Schedule("no session bus".into())), 2);
        assert_eq!(f.severity, Severity::Warn, "{f:?}");
        assert!(f.detail.unwrap().contains("no session bus"));
    }
}
