//! `doctor`: everything that could quietly be wrong, checked in one place.

use serde::Serialize;

use super::{Ctx, days_until};
use crate::journal::SwitchJournal;
use crate::lockfile::lock_path_for;
use crate::paths::storage_write_lock_target;
use crate::proc::running_claude_pids;
use crate::store::{CredentialStore, now_ms};
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
    let active = ctx.repo().active()?;
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
    let profiles = ctx.repo().list()?;
    if profiles.is_empty() {
        findings.push(Finding::warn(
            "no profiles saved",
            "run `ccred save <name>` while logged in",
        ));
    }
    for name in profiles {
        match ctx.repo().store(&name)?.load() {
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
                Err(e) => findings.push(Finding::error(
                    format!("profile '{name}' holds unusable credentials"),
                    e.to_string(),
                )),
            },
            Ok(None) => findings.push(Finding::error(
                format!("profile '{name}' has no credentials"),
                "it will need a login before it can be used".to_string(),
            )),
            Err(e) => findings.push(Finding::error(
                format!("profile '{name}' is unreadable"),
                e.to_string(),
            )),
        }
    }

    Ok(findings)
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
