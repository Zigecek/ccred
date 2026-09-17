//! `current`, `list`, `save` and `rm`.

use serde::Serialize;

use super::{Ctx, days_until};
use crate::error::CcredError;
use crate::model::AccountSnapshot;
use crate::profile::SaveOutcome;
use crate::store::{CredentialStore, now_ms};
use crate::validate::{ProfileName, validate_credentials};

/// How long `save` waits for the credential store lock.
const SAVE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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

impl ProfileRow {
    /// A row for a profile that could not be read at all.
    fn unreadable(name: &ProfileName, active: Option<&ProfileName>, note: String) -> Self {
        ProfileRow {
            name: name.as_str().to_string(),
            active: active == Some(name),
            account: "<unreadable>".into(),
            subscription: None,
            refresh_days_left: None,
            healthy: false,
            note: Some(note),
            last_synced_at_ms: None,
            needs_login: false,
        }
    }
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
    /// Set when the live credential file exists but could not be read.
    ///
    /// `current` is the first thing anyone runs, so it reports this rather
    /// than refusing to say anything. `doctor` is where it fails loudly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_error: Option<String>,
}

pub fn current(ctx: &Ctx) -> crate::Result<CurrentReport> {
    let now = now_ms();
    let live = ctx.live_store();
    let account = ctx.live_account();
    // As in `list`: reported, not fatal.
    let pointer_damage = ctx.repo().active().err().map(|e| e.to_string());
    let active = ctx.repo().active().unwrap_or(None);

    let (loaded, live_error) = match crate::store::load_unlocked(&live) {
        Ok(l) => (l, None),
        Err(e) => (None, Some(e.to_string())),
    };
    let (logged_in, access_ms_left, refresh_ms_left) = match &loaded {
        Some(l) => match validate_credentials(&l.creds.oauth, now) {
            Ok(_) => (
                true,
                Some(l.creds.oauth.expires_at.saturating_sub(now)),
                l.creds
                    .oauth
                    .refresh_token_expires_at
                    .map(|t| t.saturating_sub(now)),
            ),
            Err(_) => (false, None, None),
        },
        None => (false, None, None),
    };
    // Days are kept alongside the milliseconds so the JSON shape does not
    // change under anyone who is already parsing it.
    // Floored, so a deadline an hour gone reads as -1 rather than 0.
    let access_days_left = access_ms_left.map(|ms| ms.div_euclid(86_400_000));
    let refresh_days_left = refresh_ms_left.map(|ms| ms.div_euclid(86_400_000));

    // Does the pointer agree with who is actually logged in? A pointer that
    // cannot be read at all is the loudest form of disagreement.
    let mut pointer_mismatch = pointer_damage;
    // Unreadable metadata must not end the command: `current` is what someone
    // runs to find out that something is wrong.
    if pointer_mismatch.is_none()
        && let Some(name) = &active
        && let Ok(Some(meta)) = ctx.repo().meta(name)
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
    // The name in `.claude.json` can be stale while the tokens are another
    // profile's -- what a switch leaves when it dies half-way. The tokens say
    // whose they are.
    if pointer_mismatch.is_none()
        && let Some(l) = &loaded
    {
        pointer_mismatch = tokens_belong_elsewhere(ctx, active.as_ref(), l);
    }

    let last_synced_at_ms = match &active {
        Some(name) => ctx
            .repo()
            .meta(name)
            .ok()
            .flatten()
            .and_then(|m| m.last_synced_at_ms),
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
        plan: account.identity.plan(),
        last_synced_at_ms,
        profile_count: ctx.repo().list()?.len(),
        claude_running: crate::proc::running_claude_pids(ctx.paths().claude_config_dir()),
        pointer_mismatch,
        live_error,
    })
}

/// The warning for live tokens that are stored as a profile other than the
/// active one, if they are.
pub fn tokens_belong_elsewhere(
    ctx: &Ctx,
    active: Option<&ProfileName>,
    live: &crate::store::Loaded,
) -> Option<String> {
    let holders = ctx.repo().holders_of(&live.creds.oauth.refresh_token);
    if holders.is_empty() || active.is_some_and(|a| holders.contains(a)) {
        return None;
    }
    Some(format!(
        "the live credentials are the ones stored as profile '{}', but the active profile is {}",
        holders[0],
        active.map_or_else(|| "none".to_string(), |a| format!("'{a}'"))
    ))
}

/// Something wrong with the pointer that names the active profile.
///
/// `doctor` has always reported both of these. `list` showed a table with no
/// active row and called it "all healthy", and a damaged pointer ended the
/// command outright -- in the one place someone looks after a profile goes
/// missing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "problem")]
pub enum PointerNote {
    /// It names a profile that is not there.
    Missing { name: String },
    /// It cannot be read at all.
    Damaged { why: String },
}

pub fn pointer_note(ctx: &Ctx) -> Option<PointerNote> {
    match ctx.repo().active() {
        Err(e) => Some(PointerNote::Damaged { why: e.to_string() }),
        Ok(None) => None,
        Ok(Some(active)) => {
            let known = ctx.repo().list().unwrap_or_default();
            (!known.contains(&active)).then(|| PointerNote::Missing {
                name: active.as_str().to_string(),
            })
        }
    }
}

pub fn list(ctx: &Ctx) -> crate::Result<Vec<ProfileRow>> {
    let now = now_ms();
    // A pointer too damaged to read leaves every row unmarked, which is worth
    // far more than the error it used to be: `list` is what someone runs to
    // find out what is wrong. `pointer_note` reports it alongside.
    let active = ctx.repo().active().unwrap_or(None);
    let mut rows = Vec::new();

    for name in ctx.repo().list()? {
        // A profile whose metadata will not parse is a row that says so, not
        // a reason to refuse the whole listing. `list` is how someone finds
        // out which profile is the broken one; it has to survive meeting it.
        let meta = match ctx.repo().meta(&name) {
            Ok(m) => m,
            Err(e) => {
                rows.push(ProfileRow::unreadable(
                    &name,
                    active.as_ref(),
                    e.to_string(),
                ));
                continue;
            }
        };
        let Ok(store) = ctx.repo().store(&name) else {
            rows.push(ProfileRow::unreadable(
                &name,
                active.as_ref(),
                "its directory cannot be resolved".into(),
            ));
            continue;
        };

        let (healthy, refresh_days_left, note) = match crate::store::load_unlocked(&store) {
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
            subscription: meta.and_then(|m| m.account.plan()),
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
    /// Anything worth saying that did not stop the save, such as an
    /// interrupted switch settled on the way.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// The credentials just stored are already past their refresh deadline.
    ///
    /// Saving them is still right -- they are what is logged in -- but a
    /// report that says only "created" leaves someone believing they have a
    /// working profile, and they find out otherwise from `list` a moment
    /// later. The fix is a login, and that has to be said here.
    pub already_expired: bool,
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
    // In the spelling the profile is stored under, where the file system
    // does not tell spellings apart.
    let name = &ctx.repo().canonical_name(name);
    // Read the live store under the lock Claude Code also takes. Without it a
    // refresh landing mid-read stores half of one token pair and half of the
    // next. The lock is taken here rather than inside `save_from`, because
    // `switch` calls that while already holding it and the lock is not
    // reentrant.
    //
    // Scoped to the read, and no wider. Held to the end of the function it
    // also spanned `auto_schedule`, which shells out to schtasks, systemctl
    // or launchctl with no timeout of its own -- so a hung scheduler would
    // have blocked Claude Code's own credential refresh for as long as it
    // hung, with our heartbeat keeping the lock from ever looking stale.
    let live = ctx.live_store();
    let mut warnings = Vec::new();
    // Held to the end, unlike the live lock: a scheduled refresh must not
    // probe this profile between the store being written and the pointer
    // naming it.
    let _profiles = ctx.lock_profiles(SAVE_LOCK_TIMEOUT)?;
    let (outcome, account) = {
        let _guard = live.lock(SAVE_LOCK_TIMEOUT)?;

        // An interrupted switch is settled first, under this same lock.
        // Settling it separately gave up when the lock was busy -- and a
        // switch killed mid-way leaves exactly that lock behind -- after
        // which this save took the lock once it went stale and stored the
        // live credentials, which by then belonged to the switch's target,
        // under the identity `.claude.json` still named: one account's
        // tokens in another account's profile.
        super::switch::recover_locked(ctx, &mut warnings)?;

        // Read after recovery, which may have rewritten it.
        let account = ctx.live_account();
        if !account.is_known() {
            if let Some(why) = ctx.live_account_problem() {
                return Err(CcredError::UnsafeWrite(format!(
                    "cannot tell which account is logged in: {why}"
                )));
            }
            return Err(CcredError::UnsafeWrite(
                "cannot tell which account is logged in; is Claude Code set up in this home?"
                    .into(),
            ));
        }
        (ctx.repo().save_from(name, &live, &account)?, account)
    };

    // Saving the account that is logged in makes that profile the active one.
    // Without this the pointer would keep naming the previous profile, and
    // every later command would (correctly) report a mismatch that the user
    // had in fact just resolved.
    ctx.repo().set_active(name)?;
    // A person saved the account that is logged in under a name they chose:
    // that answers what an unreadable switch journal left open.
    crate::journal::SwitchJournal::clear_unsettled(&ctx.paths().unsettled_switch())?;

    let already_expired = live
        .load()
        .ok()
        .flatten()
        .and_then(|l| validate_credentials(&l.creds.oauth, now_ms()).ok())
        .is_some_and(|h| h.refresh_expired);

    // Released before the scheduler is touched: registering it can start the
    // job, which runs `ccred refresh`, which would wait on this lock.
    drop(_profiles);
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
        warnings,
        already_expired,
    })
}

/// Put a profile's last-known-good credentials back.
/// What `restore` put back.
#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    pub name: String,
    /// The file the credentials came from.
    pub from: String,
    /// Whether the profile itself was put back, rather than its credentials
    /// repaired in place. What comes back then is the credentials alone: the
    /// account details a switch restores are not in the copy.
    pub recreated: bool,
}

/// What a rename moved.
#[derive(Debug, Clone, Serialize)]
pub struct RenameReport {
    pub from: String,
    pub to: String,
    /// Whether the pointer had to follow it.
    pub was_active: bool,
    /// Anything that did not stop the rename, such as copies that could not
    /// be moved with it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Move a profile to another name.
///
/// There was no way to do this: `rm` and `save` again only works for the
/// account that happens to be logged in, so a profile named in haste could
/// not be renamed at all without moving directories by hand.
///
/// Nothing here writes a credential. The directory move is one rename, and
/// the pointer follows it -- in that order, because a crash between them
/// leaves a pointer naming something that is not there, which `list`,
/// `current` and `doctor` all report and the next `switch` repairs. The
/// other order would leave the profile unreachable instead.
pub fn rename(ctx: &Ctx, from: &ProfileName, to: &ProfileName) -> crate::Result<RenameReport> {
    let _profiles = ctx.lock_profiles(SAVE_LOCK_TIMEOUT)?;
    let from = &ctx.repo().canonical_name(from);
    if !ctx.repo().exists(from)? {
        return Err(CcredError::ProfileNotFound(from.as_str().to_string()));
    }
    // A case-only change is a rename of the same profile, which is the one
    // way to fix a spelling on a file system that does not distinguish them.
    let target_exists = ctx.repo().exists(to)? && &ctx.repo().canonical_name(to) != from;
    if target_exists {
        return Err(CcredError::UnsafeWrite(format!(
            "a profile called '{to}' already exists; remove it first, or pick another name"
        )));
    }
    // An interrupted switch names the old profile in its journal. Renaming
    // under it would leave the recovery looking for something that is gone.
    if !matches!(
        crate::journal::SwitchJournal::load(&ctx.paths().switch_journal()),
        Ok(None)
    ) {
        return Err(CcredError::UnsafeWrite(
            "an interrupted switch is still pending; run `ccred switch <name>` first".into(),
        ));
    }

    let mut warnings = Vec::new();
    // Read before the move. Afterwards the pointer's spelling is resolved
    // against the directories that exist now, so a rename that only changes
    // case -- `work` to `WORK`, the one way to fix a spelling where the file
    // system ignores it -- compared the new name against the old and decided
    // the profile had not been active. The pointer then kept the old
    // spelling: invisible on Windows, a pointer to nothing on Linux.
    let was_active = ctx.repo().active().unwrap_or(None).as_ref() == Some(from);
    let old_dir = ctx.paths().profile_dir(from)?;
    let new_dir = ctx.paths().profile_dir(to)?;
    std::fs::rename(&old_dir, &new_dir).map_err(|source| CcredError::Io {
        path: new_dir,
        source,
    })?;

    if was_active {
        ctx.repo().set_active(to)?;
    }

    // Cosmetic, so a failure is a warning: the directory name is what every
    // command reads, and this field is what a person reads.
    if let Err(e) = ctx
        .repo()
        .update_meta(to, |m| m.name = to.as_str().to_string())
    {
        warnings.push(format!("the profile's own record still says '{from}': {e}"));
    }

    // The copies belong to the profile, so they move with it. Merging into
    // an existing directory is not attempted: that would only arise from a
    // half-finished rename, and silently mixing two accounts' copies is
    // worse than saying so.
    let old_copies = ctx.paths().backups_dir().join(from.as_str());
    let new_copies = ctx.paths().backups_dir().join(to.as_str());
    if old_copies.is_dir() && !new_copies.exists() {
        if let Err(e) = std::fs::rename(&old_copies, &new_copies) {
            warnings.push(format!(
                "earlier copies stayed at {}: {e}",
                old_copies.display()
            ));
        }
    } else if old_copies.is_dir() {
        warnings.push(format!(
            "earlier copies stayed at {}, because {} already exists",
            old_copies.display(),
            new_copies.display()
        ));
    }

    Ok(RenameReport {
        from: from.as_str().to_string(),
        to: to.as_str().to_string(),
        was_active,
        warnings,
    })
}

pub fn restore(ctx: &Ctx, name: &ProfileName) -> crate::Result<RestoreReport> {
    let _profiles = ctx.lock_profiles(SAVE_LOCK_TIMEOUT)?;
    let name = &ctx.repo().canonical_name(name);
    if !ctx.repo().exists(name)? {
        // A profile that is gone is not the end of the question: `rm` keeps a
        // copy and says where, and this is what that copy is for.
        if let Some(from) = ctx.repo().restore_removed(name)? {
            return Ok(RestoreReport {
                name: name.as_str().to_string(),
                from: from.display().to_string(),
                recreated: true,
            });
        }
        return Err(CcredError::ProfileNotFound(name.as_str().to_string()));
    }
    if !ctx.repo().restore_last_known_good(name)? {
        return Err(CcredError::UnsafeWrite(format!(
            concat!(
                "'{0}' has no usable earlier copy to restore; switch to it and ",
                "run `claude auth login`, then `ccred save {0}`"
            ),
            name
        )));
    }
    Ok(RestoreReport {
        name: name.as_str().to_string(),
        from: ctx
            .paths()
            .profile_lkg(name)
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        recreated: false,
    })
}

pub fn remove(ctx: &Ctx, name: &ProfileName, purge: bool) -> crate::Result<RemoveReport> {
    // A refresh may be running `claude` against this very directory.
    let _profiles = ctx.lock_profiles(SAVE_LOCK_TIMEOUT)?;
    let name = &ctx.repo().canonical_name(name);
    if !ctx.repo().exists(name)? {
        return Err(CcredError::ProfileNotFound(name.as_str().to_string()));
    }
    if ctx.repo().active()?.as_ref() == Some(name) {
        return Err(CcredError::UnsafeWrite(format!(
            "'{name}' is the active profile; switch to another one first"
        )));
    }
    // Copy it aside first. The backups live outside the profile directory, so
    // this survives the deletion -- and `rm` is the one command here whose
    // mistake is a mistyped name that cannot be taken back. Every other write
    // in this tool is recoverable; deleting the only stored copy of an account
    // should not be the exception.
    //
    // A backup that fails does not stop the removal the user asked for. It is
    // reported instead.
    // Unless the point is to leave nothing: the copies are the thing being
    // got rid of, so making one more of them first would be absurd.
    let backup = if purge {
        None
    } else {
        ctx.repo().backup(name).unwrap_or(None)
    };

    // Before the profile goes: the copies are found through its account, and
    // the account is in the metadata about to be deleted.
    let purged: Vec<String> = if purge {
        ctx.repo()
            .purge_copies(name)
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    } else {
        Vec::new()
    };

    let dir = ctx.paths().profile_dir(name)?;
    std::fs::remove_dir_all(&dir).map_err(|source| CcredError::Io { path: dir, source })?;

    Ok(RemoveReport {
        name: name.as_str().to_string(),
        // The path of the file that was actually written. Reporting a
        // directory that was never created is worse than reporting nothing:
        // it tells someone their account is recoverable when it is not.
        backup_dir: backup.map(|p| p.display().to_string()),
        purged,
        purged_asked: purge,
    })
}

/// What `rm` did, and whether anything survives it.
#[derive(Debug, Clone, Serialize)]
pub struct RemoveReport {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
    /// The directories of earlier copies that `--purge` deleted: the
    /// rotation under this name, and the one keyed by account. Empty means
    /// there were none to find.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub purged: Vec<String>,
    /// Whether `--purge` was asked for at all, which is what tells "nothing
    /// was found to delete" apart from "deleting was not the request".
    pub purged_asked: bool,
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
            command: None,
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
