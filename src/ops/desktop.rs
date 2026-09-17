//! Claude Desktop profiles: the half of `save`, `switch`, `rm` and `list`
//! that deals with the Desktop's own login.
//!
//! Kept apart from the Claude Code half on purpose. The two log in
//! separately, so they can legitimately be on different accounts, and only
//! this half ever needs the Desktop closed. Someone switching the
//! terminal's account under an open Desktop must not be told to quit it.

use std::time::Duration;

use serde::Serialize;

use super::Ctx;
use crate::desktop::{self, Carried, DesktopRepo, DesktopSwitch, Owner, Sidebar, display_name};
use crate::error::CcredError;
use crate::model::AccountIdentity;
use crate::store::now_ms;
use crate::validate::ProfileName;

/// How long to wait for the profiles lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

// --- switch ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DesktopReport {
    /// The display name, `work-desktop`.
    pub to: String,
    pub desktop: DesktopSwitch,
    /// Session lists and groups put back into the sidebar by this switch.
    #[serde(skip_serializing_if = "Carried::is_empty")]
    pub restored_to_sidebar: Carried,
    /// Gathered for this profile but not yet in place: the Desktop has to
    /// be logged in as this account first. The next switch to it does it.
    #[serde(skip_serializing_if = "Carried::is_empty")]
    pub waiting: Carried,
    /// What bringing this account's sidebar up to the shared one changed.
    #[serde(skip_serializing_if = "Sidebar::is_empty")]
    pub sidebar: Sidebar,
    /// Things worth knowing that did not stop the switch.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl DesktopReport {
    /// Did a move stop half-way?
    ///
    /// Then the Desktop has no directory and the live login sits under a
    /// parked name, which a person has to finish by hand. Reported and
    /// exited 0, a script would read it as done.
    pub fn failed(&self) -> bool {
        matches!(self.desktop, DesktopSwitch::Failed { .. })
    }
}

/// Decide, check, then move. The check is the whole point: a Desktop that
/// is running is refused before a directory is touched, so this command
/// either does the move or does nothing.
///
/// A target that has no Desktop profile yet gets one. From its Claude Code
/// namesake's identity when there is one -- the same account logs in on
/// both sides. Without one, it is an account the Desktop has never logged
/// in as: a profile with no identity yet, filled in by the login that
/// follows. Either way this is how a second Desktop login is brought in:
/// park the first, start the Desktop fresh, log in as the second. Parking
/// the first is only allowed when it is a saved profile, so a mistyped
/// name costs a `switch` back and an `rm`, never a login.
pub fn switch(ctx: &Ctx, target: &ProfileName) -> crate::Result<DesktopReport> {
    let target = &ctx.repo().canonical_name(target);
    // Under the profiles lock: another ccred moving the same directories
    // at the same time is the one race here.
    let _profiles = ctx.lock_profiles(LOCK_TIMEOUT)?;
    let repo = DesktopRepo::new(ctx.paths());
    let now = now_ms();
    let inspection = desktop::inspect(ctx.paths().desktop_dir());
    claim_first_login(&repo, &inspection, now)?;
    // Written before the plan is made, because the plan has to be able to
    // see that the Desktop is already on this account. Taken back if the
    // switch is then refused, so a refusal leaves nothing behind.
    let created_now = !repo.exists(target);
    if created_now {
        let account = match ctx.repo().meta(target)? {
            Some(meta) => meta.account,
            None => {
                if repo.owner(&inspection, &[]) == Owner::Unclaimed {
                    return Err(CcredError::UnsafeWrite(format!(
                        "no profile '{0}' anywhere, and Claude Desktop is logged in as an \
                         account that is not a saved profile. `ccred save <name> \
                         --only-desktop` first, so that login can be brought back; then \
                         `ccred switch {0}` parks it and starts the Desktop fresh for {1}",
                        display_name(target),
                        target
                    )));
                }
                AccountIdentity::default()
            }
        };
        repo.write(target, account, now)?;
    }
    let active = ctx.repo().active().unwrap_or(None);
    let prefer: Vec<&ProfileName> = [Some(target), active.as_ref()]
        .into_iter()
        .flatten()
        .collect();
    let owner = repo.owner(&inspection, &prefer);
    let step = desktop::plan(&owner, target, repo.has_data(target), now);
    let uuid = repo
        .meta(target)?
        .and_then(|m| m.account.account_uuid)
        .unwrap_or_default();
    let live = ctx.paths().desktop_dir();

    // A sign-out inside the app leaves an account's session list in a
    // directory that is no longer its own. Anything of the target's that
    // sits in another profile's parked directory, or would be parked with
    // the live one, is gathered first; anything of another saved profile's
    // that would be parked with the live one goes to that profile. Putting
    // it back into the live directory rewrites the Desktop's config, which
    // needs the Desktop closed just as a move does.
    let holding = repo.parked_holding(&uuid, target);
    let would_put_back = matches!(step, desktop::Step::Leave(DesktopSwitch::AlreadyOn))
        && (!holding.is_empty() || !repo.carry_pending(target).is_empty());
    let refused = desktop::preflight(ctx.paths(), &inspection, &step)
        .err()
        .or_else(|| {
            (would_put_back && inspection.running).then(|| {
                CcredError::UnsafeWrite(
                    "Claude Desktop is running; quit it first, so that this account's chats \
                 can be put back into its sidebar"
                        .into(),
                )
            })
        });
    if let Some(e) = refused {
        if created_now {
            let _ = repo.forget_meta(target);
        }
        return Err(e);
    }

    let mut warnings = Vec::new();
    let mut gathered = Carried::default();
    for (_, data) in &holding {
        gathered.add(repo.carry_out(target, &uuid, data)?);
    }
    if let desktop::Step::Move {
        park_as: Some(_), ..
    } = &step
    {
        for other in &inspection.other_accounts {
            match repo.profile_for(other) {
                Some(p) if &p == target => gathered.add(repo.carry_out(target, other, live)?),
                Some(p) => {
                    let c = repo.carry_out(&p, other, live)?;
                    warnings.push(format!(
                        "{} and {} of '{}' were in the directory being parked (a sign-out inside \
                         the app leaves them behind); kept for it, put back on its next switch",
                        plural(c.sessions, "session"),
                        plural(c.groups, "sidebar group"),
                        display_name(&p)
                    ));
                }
                None => warnings.push(format!(
                    "the directory being parked also holds the session list of account {other}, \
                     which is not a saved profile; it stays in there"
                )),
            }
        }
    }

    // The list the account being parked has now is the freshest word on
    // its sessions: into the shared sidebar before the directory goes.
    if let (
        desktop::Step::Move {
            park_as: Some(_), ..
        },
        Owner::Profile(from),
    ) = (&step, &owner)
        && let Some(f) = repo.meta(from)?.and_then(|m| m.account.account_uuid)
        && let Err(e) = desktop::sidebar_collect(ctx.paths(), live, &f)
    {
        // Not a `?` either, though nothing has moved yet: the shared sidebar
        // is a convenience built out of lists the accounts keep themselves,
        // and refusing to switch a login because one could not be written
        // would trade the operation that matters for one that does not.
        warnings.push(format!(
            "the chats of the account being parked were not added to the shared sidebar ({e}); \
             switching back to it adds them"
        ));
    }

    let outcome = desktop::apply(ctx.paths(), target, step);
    // The profile whose directory was just parked, or just restored, has
    // been seen: that is what the timestamp records.
    if let DesktopSwitch::Moved { .. } = &outcome {
        if let Owner::Profile(from) = &owner
            && let Some(m) = repo.meta(from)?
        {
            repo.write(from, m.account, now)?;
        }
        if let Some(m) = repo.meta(target)? {
            repo.write(target, m.account, now)?;
        }
    }
    let live_is_targets = matches!(
        outcome,
        DesktopSwitch::AlreadyOn | DesktopSwitch::Moved { restored: true, .. }
    );
    let mut restored_to_sidebar = Carried::default();
    let mut sidebar = Sidebar::default();
    if live_is_targets {
        let put_back = repo.carry_in(target, &uuid, live)?;
        restored_to_sidebar = put_back.carried;
        for (what, why) in [
            ("sidebar groups", put_back.groups_kept_back),
            ("chat list", put_back.sessions_kept_back),
        ] {
            if let Some(why) = why {
                warnings.push(format!(
                    "the {what} stayed with the profile ({why}); the next switch to it puts it in"
                ));
            }
        }
        // Its own list first, so nothing it knows is older than the union;
        // then the union into it. Writing into the list needs the Desktop
        // closed, like every other write into its directory; reading does
        // not, so a running Desktop still feeds the union.
        // Neither is a `?`, for the reason the carry above is not: the
        // directories have already moved, so the switch has happened. A
        // sidebar that could not be brought up to date is worth saying and
        // is fixed by switching to this profile again; raised, it would send
        // someone looking for a switch that did not take, and hide the one
        // that did.
        if let Err(e) = desktop::sidebar_collect(ctx.paths(), live, &uuid) {
            warnings.push(format!(
                "this account's chats were not added to the shared sidebar ({e}); \
                 switching to it again does it"
            ));
        }
        if inspection.running {
            warnings.push(
                "Claude Desktop is running, so its sidebar was not brought up to date; \
                 quit it and switch to this profile again"
                    .into(),
            );
        } else {
            match desktop::sidebar_spread(ctx.paths(), live, &uuid) {
                Ok(done) => {
                    sidebar = done.done;
                    if let Some(why) = done.groups_kept_back {
                        warnings.push(format!(
                            "the sidebar groups were not written into this account's config \
                             ({why}); they stay in the shared sidebar, and the next switch to \
                             this profile puts them in"
                        ));
                    }
                }
                Err(e) => warnings.push(format!(
                    "the shared sidebar was not written into this account's list ({e}); \
                     switching to it again does it"
                )),
            }
        }
    }
    Ok(DesktopReport {
        to: display_name(target),
        desktop: outcome,
        restored_to_sidebar,
        waiting: repo.carry_pending(target),
        sidebar,
        warnings,
    })
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// A profile started fresh with `--new` has no account until someone logs
/// in. The Desktop then names an account nobody has saved -- and if exactly
/// one profile is waiting for its first login, that is whose it is.
/// Recorded by the commands that write anyway; `owner` reads it the same
/// way meanwhile, so `list` and `current` agree before it is written.
fn claim_first_login(
    repo: &DesktopRepo,
    inspection: &desktop::Inspection,
    now: i64,
) -> crate::Result<()> {
    if let (Some(uuid), Owner::Profile(p)) = (&inspection.account_uuid, repo.owner(inspection, &[]))
        && let Some(meta) = repo.meta(&p)?
        && meta.account.account_uuid.is_none()
    {
        repo.write(
            &p,
            AccountIdentity {
                account_uuid: Some(uuid.clone()),
                ..meta.account
            },
            now,
        )?;
    }
    Ok(())
}

// --- save -----------------------------------------------------------------

/// What `save` did about the Desktop.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum DesktopSave {
    Created {
        account: String,
    },
    Updated {
        account: String,
    },
    /// Nothing to record: no Desktop here, or one that has never logged in.
    Nothing {
        reason: String,
    },
    /// The name is taken by another account's Desktop login.
    Refused {
        reason: String,
    },
}

impl DesktopSave {
    pub fn saved(&self) -> bool {
        matches!(
            self,
            DesktopSave::Created { .. } | DesktopSave::Updated { .. }
        )
    }
}

/// Record the account the Desktop is logged in as, under this name.
///
/// Only the identity is written: the live directory stays where it is,
/// because it is the login, and the first switch away parks it. The email
/// is not in the Desktop's files in plain text; it is taken from Claude
/// Code's side when that is the same account, or from a Claude Code
/// profile that is.
pub fn save(ctx: &Ctx, name: &ProfileName, live_code: &AccountIdentity) -> DesktopSave {
    // Under the profiles lock, like `switch` and `remove`: this writes the
    // profile's record and collects its sidebar, and the Claude Code half of
    // the same save has already let its own lock go by the time this runs.
    let _profiles = match ctx.lock_profiles(LOCK_TIMEOUT) {
        Ok(guard) => guard,
        Err(e) => {
            return DesktopSave::Refused {
                reason: e.to_string(),
            };
        }
    };
    let inspection = desktop::inspect(ctx.paths().desktop_dir());
    if !inspection.installed {
        return DesktopSave::Nothing {
            reason: "no Claude Desktop here".into(),
        };
    }
    let Some(uuid) = inspection.account_uuid.clone() else {
        return DesktopSave::Nothing {
            reason: "Claude Desktop is not logged in".into(),
        };
    };
    let repo = DesktopRepo::new(ctx.paths());
    if let Err(e) = claim_first_login(&repo, &inspection, now_ms()) {
        return DesktopSave::Refused {
            reason: e.to_string(),
        };
    }
    match repo.meta(name) {
        Ok(Some(existing))
            if existing
                .account
                .account_uuid
                .as_ref()
                .is_some_and(|u| u != &uuid) =>
        {
            return DesktopSave::Refused {
                reason: format!(
                    "'{}' belongs to {}, and Claude Desktop is logged in as another account",
                    display_name(name),
                    existing.account.label()
                ),
            };
        }
        Ok(_) => {}
        Err(e) => {
            return DesktopSave::Refused {
                reason: e.to_string(),
            };
        }
    }
    let account = identity_for(ctx, &uuid, live_code);
    let existed = repo.exists(name);
    // A save is a moment the account's list is certainly current. Reading
    // it is safe with the Desktop open; it is the writes that are not.
    if let Err(e) = desktop::sidebar_collect(ctx.paths(), ctx.paths().desktop_dir(), &uuid) {
        return DesktopSave::Refused {
            reason: e.to_string(),
        };
    }
    match repo.write(name, account.clone(), now_ms()) {
        Ok(_) if existed => DesktopSave::Updated {
            account: account.label(),
        },
        Ok(_) => DesktopSave::Created {
            account: account.label(),
        },
        Err(e) => DesktopSave::Refused {
            reason: e.to_string(),
        },
    }
}

/// The fullest identity on record for an account id.
fn identity_for(ctx: &Ctx, uuid: &str, live_code: &AccountIdentity) -> AccountIdentity {
    if live_code.account_uuid.as_deref() == Some(uuid) {
        return live_code.clone();
    }
    let from_profiles = ctx
        .repo()
        .list()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|n| ctx.repo().meta(&n).ok().flatten())
        .map(|m| m.account)
        .find(|a| a.account_uuid.as_deref() == Some(uuid));
    from_profiles.unwrap_or_else(|| AccountIdentity {
        account_uuid: Some(uuid.to_string()),
        ..Default::default()
    })
}

// --- rm -------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DesktopRemoveReport {
    /// The display name, `work-desktop`.
    pub name: String,
    /// A parked login was deleted with it. Not copied aside: there is no
    /// way to check what a copy holds, and a login is a thing one can get
    /// again.
    pub parked_login_removed: bool,
    /// The Desktop is logged in as this account right now. That stays as
    /// it is; the live directory is never touched. It just belongs to no
    /// profile any more.
    pub still_logged_in: bool,
}

/// Delete a Desktop profile and its parked login, and nothing else.
pub fn remove(ctx: &Ctx, name: &ProfileName, purge: bool) -> crate::Result<DesktopRemoveReport> {
    let name = &ctx.repo().canonical_name(name);
    let _profiles = ctx.lock_profiles(LOCK_TIMEOUT)?;
    let repo = DesktopRepo::new(ctx.paths());
    if !repo.exists(name) {
        return Err(CcredError::ProfileNotFound(display_name(name)));
    }
    let inspection = desktop::inspect(ctx.paths().desktop_dir());
    let still_logged_in = repo.owner(&inspection, &[name]) == Owner::Profile(name.clone());
    let parked_login_removed = repo.has_data(name);
    // A parked directory IS the login: the token inside is encrypted, so
    // there is no copy to take first and nothing to put back afterwards.
    // `rm` on the Claude Code half keeps a copy and says where; this half
    // cannot, so it asks instead. Forgetting a profile whose login is
    // elsewhere stays a plain `rm`.
    if parked_login_removed && !purge {
        return Err(CcredError::UnsafeWrite(format!(
            "'{}' has a parked login, and it is the only copy: its token is encrypted, so nothing can be kept aside and nothing can put it back. `ccred rm {} --purge` deletes it, or `ccred switch {}` makes it the live one first",
            display_name(name),
            display_name(name),
            display_name(name)
        )));
    }
    repo.remove(name)?;
    Ok(DesktopRemoveReport {
        name: display_name(name),
        parked_login_removed,
        still_logged_in,
    })
}

// --- list and current -----------------------------------------------------

/// Where a Desktop profile's login is.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopState {
    /// The Desktop is logged in as this account right now.
    LoggedIn,
    /// The Desktop's directory is this account's, but the app was signed
    /// out from inside: it will ask for a login, and the list and groups
    /// in there are the account's still.
    SignedOut,
    /// Waiting under `~/.ccred/desktop/<name>/data`.
    Parked,
    /// Recorded, but its login is nowhere: the Desktop has to log in as it.
    NoLogin,
}

/// One row of the Desktop table in `ccred list`. Contains no secret.
#[derive(Debug, Clone, Serialize)]
pub struct DesktopRow {
    /// The display name, `work-desktop`.
    pub name: String,
    /// The live login is this profile's.
    pub active: bool,
    pub account: String,
    pub state: DesktopState,
    /// Set on the active row when the Desktop is running.
    pub running: bool,
    pub last_synced_at_ms: Option<i64>,
    /// Sessions and groups gathered for this profile that are not in its
    /// directory yet, because the Desktop has to be logged in as it first.
    #[serde(skip_serializing_if = "Carried::is_empty")]
    pub waiting: Carried,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn rows(ctx: &Ctx) -> Vec<DesktopRow> {
    let repo = DesktopRepo::new(ctx.paths());
    let inspection = desktop::inspect(ctx.paths().desktop_dir());
    let active = ctx.repo().active().unwrap_or(None);
    let prefer: Vec<&ProfileName> = active.iter().collect();
    let live = match repo.owner(&inspection, &prefer) {
        Owner::Profile(p) => Some(p),
        _ => None,
    };
    repo.list()
        .into_iter()
        .map(|name| {
            match repo.meta(&name) {
                // Every profile on the account the Desktop is logged in as
                // is logged in -- two names for one account are both live,
                // as they are for Claude Code profiles.
                Ok(Some(meta)) => {
                    let is_live = live.as_ref() == Some(&name)
                        || (meta.account.account_uuid.is_some()
                            && meta.account.account_uuid == inspection.account_uuid);
                    let waiting = repo.carry_pending(&name);
                    DesktopRow {
                        name: display_name(&name),
                        active: is_live,
                        account: meta.account.label(),
                        state: if is_live && inspection.signed_in == Some(false) {
                            DesktopState::SignedOut
                        } else if is_live {
                            DesktopState::LoggedIn
                        } else if repo.has_data(&name) {
                            DesktopState::Parked
                        } else {
                            DesktopState::NoLogin
                        },
                        running: is_live && inspection.running,
                        last_synced_at_ms: meta.last_synced_at_ms,
                        // Counts, not a sentence: the words live in
                        // `render`, which had its own copy of them
                        // while this built a 128-column version.
                        waiting,
                        note: None,
                    }
                }
                // A profile `list` named and whose metadata then read as
                // absent: a removal racing this listing, or a directory
                // someone emptied. `list` is what a person runs to find out
                // which profile is the broken one, so it says so in a row --
                // the same answer as metadata that will not parse, and one
                // that cannot end an unattended run in a panic.
                Ok(None) => DesktopRow {
                    name: display_name(&name),
                    active: false,
                    account: "<unknown account>".into(),
                    state: DesktopState::NoLogin,
                    running: false,
                    last_synced_at_ms: None,
                    waiting: Carried::default(),
                    note: Some("its record is gone".into()),
                },
                Err(e) => DesktopRow {
                    name: display_name(&name),
                    active: false,
                    account: "<unreadable>".into(),
                    state: DesktopState::NoLogin,
                    running: false,
                    last_synced_at_ms: None,
                    waiting: Carried::default(),
                    note: Some(e.to_string()),
                },
            }
        })
        .collect()
}

/// The Desktop as `current` and `doctor` report it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DesktopStatus {
    pub installed: bool,
    pub running: bool,
    /// The app was signed out from inside; `profile` is then the account
    /// it was last logged in as.
    pub signed_out_in_app: bool,
    /// The Desktop profile whose account the Desktop is logged in as, by
    /// display name, when it is a saved one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Whether the Desktop has logged in at all. False with no profile
    /// means a fresh install, or one still on its first screen.
    pub logged_in: bool,
    /// Desktop profiles with a parked login, by display name.
    pub parked: Vec<String>,
}

/// `None` when there is no Desktop here and no Desktop profile either:
/// then it is not part of this machine's picture.
pub fn status(ctx: &Ctx, active: Option<&ProfileName>) -> Option<DesktopStatus> {
    let repo = DesktopRepo::new(ctx.paths());
    let inspection = desktop::inspect(ctx.paths().desktop_dir());
    let profiles = repo.list();
    if !inspection.installed && profiles.is_empty() {
        return None;
    }
    let prefer: Vec<&ProfileName> = active.into_iter().collect();
    let profile = match repo.owner(&inspection, &prefer) {
        Owner::Profile(name) => Some(display_name(&name)),
        Owner::Missing | Owner::Unclaimed => None,
    };
    Some(DesktopStatus {
        installed: inspection.installed,
        running: inspection.running,
        signed_out_in_app: inspection.signed_in == Some(false),
        profile,
        logged_in: inspection.account_uuid.is_some(),
        parked: profiles
            .iter()
            .filter(|n| repo.has_data(n))
            .map(display_name)
            .collect(),
    })
}
