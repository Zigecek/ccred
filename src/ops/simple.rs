//! `current`, `list`, `save` and `rm`.

use serde::Serialize;

use super::{Ctx, days_until};
use crate::error::CcredError;
use crate::model::AccountSnapshot;
use crate::profile::SaveOutcome;
use crate::store::{CredentialStore, now_ms};
use crate::validate::{ProfileName, validate_credentials};

/// One row of `ccred list`. Contains no secret.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileRow {
    pub name: String,
    pub active: bool,
    pub account: String,
    pub subscription: Option<String>,
    /// `None` when the profile has no usable credentials at all.
    pub refresh_days_left: Option<i64>,
    pub healthy: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CurrentReport {
    pub active_profile: Option<String>,
    pub account: String,
    pub logged_in: bool,
    pub access_days_left: Option<i64>,
    pub refresh_days_left: Option<i64>,
    pub claude_running: Vec<u32>,
    /// Set when the active pointer disagrees with the live account. This is
    /// the shape of a near-miss that once nearly destroyed a profile.
    pub pointer_mismatch: Option<String>,
}

pub fn current(ctx: &Ctx) -> crate::Result<CurrentReport> {
    let now = now_ms();
    let live = ctx.live_store();
    let account = ctx.live_account();
    let active = ctx.repo().active()?;

    let loaded = live.load()?;
    let (logged_in, access_days_left, refresh_days_left) = match &loaded {
        Some(l) => match validate_credentials(&l.creds.oauth, now) {
            Ok(_) => (
                true,
                Some(days_until(l.creds.oauth.expires_at, now)),
                l.creds
                    .oauth
                    .refresh_token_expires_at
                    .map(|t| days_until(t, now)),
            ),
            Err(_) => (false, None, None),
        },
        None => (false, None, None),
    };

    // Does the pointer agree with who is actually logged in?
    let mut pointer_mismatch = None;
    if let Some(name) = &active
        && let Some(meta) = ctx.repo().meta(name)?
        && account.is_known()
        && meta.account != Default::default()
        && account.identity.same_account_as(&meta.account) == Some(false)
    {
        pointer_mismatch = Some(format!(
            "the active profile is '{}' ({}), but {} is logged in",
            name,
            meta.account.label(),
            account.label()
        ));
    }

    Ok(CurrentReport {
        active_profile: active.map(|n| n.as_str().to_string()),
        account: account.label(),
        logged_in,
        access_days_left,
        refresh_days_left,
        claude_running: crate::proc::running_claude_pids(ctx.paths().claude_config_dir()),
        pointer_mismatch,
    })
}

pub fn list(ctx: &Ctx) -> crate::Result<Vec<ProfileRow>> {
    let now = now_ms();
    let active = ctx.repo().active()?;
    let mut rows = Vec::new();

    for name in ctx.repo().list()? {
        let meta = ctx.repo().meta(&name)?;
        let store = ctx.repo().store(&name)?;

        let (healthy, refresh_days_left, note) = match store.load() {
            Ok(Some(loaded)) => match validate_credentials(&loaded.creds.oauth, now) {
                Ok(health) => {
                    let days = loaded
                        .creds
                        .oauth
                        .refresh_token_expires_at
                        .map(|t| days_until(t, now));
                    if health.refresh_expired {
                        (
                            false,
                            days,
                            Some("refresh token expired, needs login".into()),
                        )
                    } else {
                        (true, days, None)
                    }
                }
                Err(e) => (false, None, Some(e.to_string())),
            },
            Ok(None) => (false, None, Some("no credentials stored".into())),
            Err(e) => (false, None, Some(e.to_string())),
        };

        rows.push(ProfileRow {
            name: name.as_str().to_string(),
            active: active.as_ref() == Some(&name),
            account: meta
                .as_ref()
                .map(|m| m.account.label())
                .unwrap_or_else(|| "<unknown account>".into()),
            subscription: meta.and_then(|m| m.account.rate_limit_tier),
            refresh_days_left,
            healthy,
            note,
        });
    }
    Ok(rows)
}

#[derive(Debug, Clone, Serialize)]
pub struct SaveReport {
    pub name: String,
    pub account: String,
    pub outcome: String,
}

pub fn save(ctx: &Ctx, name: &ProfileName) -> crate::Result<SaveReport> {
    // `save` writes the active pointer, so an interrupted switch must be
    // settled first -- otherwise its journal would outlive the inconsistency
    // it describes and `doctor` would keep reporting a problem that is gone.
    let mut warnings = Vec::new();
    let _ = super::switch::recover_pending(ctx, &mut warnings)?;

    let account = ctx.live_account();
    if !account.is_known() {
        return Err(CcredError::UnsafeWrite(
            "cannot tell which account is logged in; is Claude Code set up in this home?".into(),
        ));
    }
    let outcome = ctx.repo().save_from(name, &ctx.live_store(), &account)?;

    // Saving the account that is logged in makes that profile the active one.
    // Without this the pointer would keep naming the previous profile, and
    // every later command would (correctly) report a mismatch that the user
    // had in fact just resolved.
    ctx.repo().set_active(name)?;

    Ok(SaveReport {
        name: name.as_str().to_string(),
        account: account.label(),
        outcome: match outcome {
            SaveOutcome::Created => "created",
            SaveOutcome::Updated => "updated",
            SaveOutcome::Unchanged => "already up to date",
        }
        .to_string(),
    })
}

pub fn remove(ctx: &Ctx, name: &ProfileName) -> crate::Result<()> {
    if !ctx.repo().exists(name)? {
        return Err(CcredError::ProfileNotFound(name.as_str().to_string()));
    }
    if ctx.repo().active()?.as_ref() == Some(name) {
        return Err(CcredError::UnsafeWrite(format!(
            "'{name}' is the active profile; switch to another one first"
        )));
    }
    let dir = ctx.paths().profile_dir(name)?;
    std::fs::remove_dir_all(&dir).map_err(|source| CcredError::Io { path: dir, source })?;
    Ok(())
}

/// The snapshot a save would use. Exposed for `doctor`.
pub fn live_account_of(ctx: &Ctx) -> AccountSnapshot {
    ctx.live_account()
}
