//! `current`, `list`, `save` and `rm`.

use serde::Serialize;

use super::{Ctx, days_until};
use crate::error::CcredError;
use crate::model::AccountSnapshot;
use crate::profile::SaveOutcome;
use crate::store::{CredentialStore, now_ms};

/// How long `save` waits for the credential store lock.
const SAVE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
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
    /// When `ccred` last wrote this profile from a live login or a refresh.
    pub last_synced_at_ms: Option<i64>,
    /// Latched by `refresh` when only a person can fix this one.
    pub needs_login: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CurrentReport {
    pub active_profile: Option<String>,
    pub account: String,
    pub logged_in: bool,
    pub access_days_left: Option<i64>,
    pub refresh_days_left: Option<i64>,
    /// Milliseconds, because an access token lives about eight hours and
    /// "0 days left" is not a useful thing to tell someone about it.
    pub access_ms_left: Option<i64>,
    pub refresh_ms_left: Option<i64>,
    /// The plan on the account that is logged in, when it is known.
    pub plan: Option<String>,
    /// When the active profile was last written.
    pub last_synced_at_ms: Option<i64>,
    /// How many profiles exist, so `current` can point at `list`.
    pub profile_count: usize,
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
    let (logged_in, access_ms_left, refresh_ms_left) = match &loaded {
        Some(l) => match validate_credentials(&l.creds.oauth, now) {
            Ok(_) => (
                true,
                Some(l.creds.oauth.expires_at - now),
                l.creds.oauth.refresh_token_expires_at.map(|t| t - now),
            ),
            Err(_) => (false, None, None),
        },
        None => (false, None, None),
    };
    // Days are kept alongside the milliseconds so the JSON shape does not
    // change under anyone who is already parsing it.
    let access_days_left = access_ms_left.map(|ms| ms / 86_400_000);
    let refresh_days_left = refresh_ms_left.map(|ms| ms / 86_400_000);

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

    let last_synced_at_ms = match &active {
        Some(name) => ctx.repo().meta(name)?.and_then(|m| m.last_synced_at_ms),
        None => None,
    };

    Ok(CurrentReport {
        active_profile: active.map(|n| n.as_str().to_string()),
        account: account.label(),
        logged_in,
        access_days_left,
        refresh_days_left,
        access_ms_left,
        refresh_ms_left,
        plan: account.identity.rate_limit_tier.clone(),
        last_synced_at_ms,
        profile_count: ctx.repo().list()?.len(),
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
            last_synced_at_ms: meta.as_ref().and_then(|m| m.last_synced_at_ms),
            needs_login: meta.as_ref().is_some_and(|m| m.refresh.needs_login),
            subscription: meta.and_then(|m| m.account.rate_limit_tier),
            refresh_days_left,
            healthy,
            note,
        });
    }
    Ok(rows)
}

/// What a save did about the refresh schedule, when it did anything.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum ScheduleSetup {
    Installed {
        next_run: Option<String>,
    },
    /// Registration was attempted and did not work. Never fatal -- see
    /// `auto_schedule`.
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SaveReport {
    pub name: String,
    pub account: String,
    pub outcome: String,
    /// Set when this save registered the refresh schedule, or tried to.
    ///
    /// The moment a second profile exists is the moment one of them starts
    /// going stale unattended, and it is the only moment a person is
    /// guaranteed to be watching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleSetup>,
}

/// Does this save call for registering the schedule?
///
/// Pure, so the policy is testable. Auto-registration writes to the platform
/// scheduler, which a test must never do on the machine running it, so the
/// decision is separated from the act.
pub fn should_auto_schedule(
    outcome: SaveOutcome,
    profile_count: usize,
    state: &crate::schedule::State,
) -> bool {
    outcome == SaveOutcome::Created
        && profile_count > 1
        && matches!(state, crate::schedule::State::NotInstalled)
}

/// Register the refresh schedule on the save that first makes it necessary.
///
/// Every failure here is reported rather than returned. The credentials are
/// already stored by the time this runs, and losing that result because a
/// scheduler refused would trade the operation that matters for the one that
/// does not.
fn auto_schedule(ctx: &Ctx, outcome: SaveOutcome) -> Option<ScheduleSetup> {
    // The escape hatch exists for provisioning, for anyone who schedules
    // refreshes their own way, and for this project's own test suite, which
    // must not register a task on whichever machine runs it.
    if std::env::var_os("CCRED_NO_AUTO_SCHEDULE").is_some() {
        return None;
    }
    if outcome != SaveOutcome::Created {
        return None;
    }
    // Querying the scheduler is not free, so ask only once the cheap local
    // conditions already hold.
    let profile_count = ctx.repo().list().map(|l| l.len()).unwrap_or(0);
    if profile_count <= 1 {
        return None;
    }

    let backend = crate::schedule::detect();
    let state = backend.status().ok()?;
    if !should_auto_schedule(outcome, profile_count, &state) {
        return None;
    }

    let spec = match super::schedule::spec_for(ctx) {
        Ok(spec) => spec,
        Err(e) => {
            return Some(ScheduleSetup::Failed {
                reason: e.to_string(),
            });
        }
    };
    // `install_checked` undoes its own work if the scheduler cannot name a
    // next run, so this either produces a schedule that fires or nothing.
    match crate::schedule::install_checked(backend.as_ref(), &spec) {
        Ok(health) => Some(ScheduleSetup::Installed {
            next_run: health.next_run,
        }),
        Err(e) => Some(ScheduleSetup::Failed {
            reason: e.to_string(),
        }),
    }
}

pub fn save(ctx: &Ctx, name: &ProfileName) -> crate::Result<SaveReport> {
    // `save` writes the active pointer, so an interrupted switch must be
    // settled first -- otherwise its journal would outlive the inconsistency
    // it describes and `doctor` would keep reporting a problem that is gone.
    let mut warnings = Vec::new();
    let _ = super::switch::recover_pending(ctx, &mut warnings)?;

    // Read the live store under the lock Claude Code also takes. Without it a
    // refresh landing mid-read stores half of one token pair and half of the
    // next. The lock is taken here rather than inside `save_from`, because
    // `switch` calls that while already holding it and the lock is not
    // reentrant.
    let live = ctx.live_store();
    let _guard = live.lock(SAVE_LOCK_TIMEOUT)?;

    let account = ctx.live_account();
    if !account.is_known() {
        return Err(CcredError::UnsafeWrite(
            "cannot tell which account is logged in; is Claude Code set up in this home?".into(),
        ));
    }
    let outcome = ctx.repo().save_from(name, &live, &account)?;

    // Saving the account that is logged in makes that profile the active one.
    // Without this the pointer would keep naming the previous profile, and
    // every later command would (correctly) report a mismatch that the user
    // had in fact just resolved.
    ctx.repo().set_active(name)?;

    let schedule = auto_schedule(ctx, outcome);

    Ok(SaveReport {
        name: name.as_str().to_string(),
        account: account.label(),
        outcome: match outcome {
            SaveOutcome::Created => "created",
            SaveOutcome::Updated => "updated",
            SaveOutcome::Unchanged => "already up to date",
        }
        .to_string(),
        schedule,
    })
}

/// Put a profile's last-known-good credentials back.
pub fn restore(ctx: &Ctx, name: &ProfileName) -> crate::Result<()> {
    if !ctx.repo().exists(name)? {
        return Err(CcredError::ProfileNotFound(name.as_str().to_string()));
    }
    if !ctx.repo().restore_last_known_good(name)? {
        return Err(CcredError::UnsafeWrite(format!(
            "'{name}' has no usable earlier copy to restore; switch to it and              run `claude auth login`, then `ccred save {name}`"
        )));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{Health, State};

    fn installed() -> State {
        State::Installed(Health {
            enabled: true,
            next_run: Some("Fri 03:00".into()),
            last_run: None,
            warnings: Vec::new(),
        })
    }

    /// The point of auto-registration: the save that creates a second profile
    /// is the moment one of them starts going stale unattended.
    #[test]
    fn the_save_that_creates_a_second_profile_registers_the_schedule() {
        assert!(should_auto_schedule(
            SaveOutcome::Created,
            2,
            &State::NotInstalled
        ));
    }

    #[test]
    fn a_first_profile_does_not_need_a_schedule() {
        // Claude Code refreshes what it is using, so nothing can go stale yet.
        assert!(!should_auto_schedule(
            SaveOutcome::Created,
            1,
            &State::NotInstalled
        ));
    }

    /// Re-saving is routine -- it happens on every switch. Registering from
    /// there would mean a user who deliberately removed the schedule would
    /// silently get it back.
    #[test]
    fn re_saving_an_existing_profile_never_registers_anything() {
        for outcome in [SaveOutcome::Updated, SaveOutcome::Unchanged] {
            assert!(
                !should_auto_schedule(outcome, 5, &State::NotInstalled),
                "{outcome:?} must not register a schedule"
            );
        }
    }

    #[test]
    fn an_existing_schedule_is_left_alone() {
        assert!(!should_auto_schedule(SaveOutcome::Created, 2, &installed()));
    }

    #[test]
    fn an_unsupported_platform_is_not_treated_as_missing() {
        let state = State::Unsupported {
            reason: "no systemd".into(),
            remedy: None,
        };
        assert!(!should_auto_schedule(SaveOutcome::Created, 2, &state));
    }
}
