//! Claude Desktop's login, moved as a whole.
//!
//! The Desktop app does not log in through `~/.claude`. It keeps its own
//! OAuth token, encrypted with the operating system's keyring, inside its
//! data directory, and hands that token to every Claude Code session it
//! opens. So `.credentials.json` can say what it likes: a session started
//! from the Desktop runs as whatever account the Desktop is logged in as.
//!
//! The token cannot be checked, swapped or refreshed from here -- it is
//! ciphertext, and every safety rule in this tool depends on being able to
//! read what it writes. What can be moved safely is the directory as a
//! whole. Each one is a complete logged-in state -- token, cookies, device
//! identity, the app's index of Code sessions -- and all of it belongs to
//! one account. So a Desktop profile is a name, an account identity, and at
//! most one such directory: the live one, when the Desktop is logged in as
//! that account, or a parked one under `~/.ccred/desktop/<name>/data`.
//! Switching parks the live directory under the profile whose account it
//! holds and puts the target's parked directory in its place. Nothing
//! inside is read except `lastKnownAccountUuid`, which is plain text and
//! says whose the directory is: the identity check this tool insists on
//! before it moves anything. Nothing is ever copied, either -- a token that
//! cannot be checked is not one to duplicate -- so `save` records the
//! identity and leaves the live directory where it is. Code sessions are
//! not in there. They are transcripts under `~/.claude/projects/`, shared by
//! every way of opening Claude Code, and a switch does not touch them.
//!
//! On the command line a Desktop profile is its Claude Code namesake with
//! `-desktop` on the end: `ccred save work` records both, `ccred switch
//! work-desktop` moves the Desktop and nothing else. The two log in
//! separately and can legitimately be on different accounts, which is why
//! they switch separately -- and why only the Desktop half ever asks for the
//! Desktop to be closed.
//!
//! It has to be closed for that. A directory renamed under a running
//! Electron app is not moved so much as split -- the process keeps writing
//! into the renamed one -- and on Windows the rename is refused outright.
//! Electron's single-instance lock says whether it is running.
//!
//! A parked login ages. The Desktop refreshes its token only while it is the
//! one running, and this tool cannot refresh it at all. When a parked login
//! has expired, the Desktop asks for a login after the switch. That is the
//! whole cost.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic::write_atomic;
use crate::error::CcredError;
use crate::model::AccountIdentity;
use crate::paths::Paths;
use crate::validate::{ProfileName, validate_profile_name};

/// What turns a profile's name into the name of its Desktop login.
pub const SUFFIX: &str = "-desktop";

/// The parked directory, inside the profile's own.
const DATA_DIR: &str = "data";
const META_FILE: &str = "meta.json";
/// Parking places for directories that belong to no profile.
const UNCLAIMED_PREFIX: &str = "unclaimed-";

/// What a name on the command line refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handle {
    ClaudeCode(ProfileName),
    Desktop(ProfileName),
}

impl Handle {
    /// `work` is a Claude Code profile; `work-desktop` is its Desktop
    /// login. The suffix is stripped before the name is validated, which is
    /// also what leaves the plain validator free to refuse it.
    pub fn parse(raw: &str) -> crate::Result<Handle> {
        // Matched without regard to case, because the validator refuses the
        // suffix that way: `Work-DESKTOP` parsed as a Claude Code name and
        // was then refused for ending in `-desktop`, so it addressed
        // nothing at all.
        let suffix_at = raw
            .len()
            .checked_sub(SUFFIX.len())
            .filter(|at| raw[*at..].eq_ignore_ascii_case(SUFFIX));
        match suffix_at.map(|at| &raw[..at]) {
            Some(stem) => Ok(Handle::Desktop(validate_profile_name(stem)?)),
            None => Ok(Handle::ClaudeCode(validate_profile_name(raw)?)),
        }
    }
}

/// The name a Desktop profile goes by on screen and in JSON.
pub fn display_name(name: &ProfileName) -> String {
    format!("{name}{SUFFIX}")
}

// --- the Desktop's own directory ------------------------------------------

/// The one key this module reads out of the Desktop's `config.json`. The
/// rest of that file is the app's own business, and some of it is a token.
#[derive(Deserialize)]
struct DesktopConfig {
    #[serde(rename = "lastKnownAccountUuid", default)]
    last_known_account_uuid: Option<String>,
    /// Whether the window last showed the app or the login screen: the one
    /// plain-text sign of a sign-out inside the app, which leaves
    /// `lastKnownAccountUuid` where it was.
    #[serde(rename = "windowSizeWasSignedIn", default)]
    window_was_signed_in: Option<bool>,
}

/// What the Desktop's data directory says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub installed: bool,
    pub running: bool,
    /// The account the Desktop last logged in as, when it ever has.
    pub account_uuid: Option<String>,
    /// Why the account could not be read, when a directory is there but its
    /// record is not readable. Distinct from "no account": a live login whose
    /// `config.json` cannot be parsed belongs to somebody, and moving it as
    /// though it belonged to nobody is how it gets lost.
    pub identity_unreadable: Option<String>,
    /// `Some(false)` when the app was signed out from inside, which keeps
    /// the last account's id on record. Unknown for a Desktop that has not
    /// written the flag.
    pub signed_in: Option<bool>,
    /// Other accounts whose Code-session lists this directory holds.
    ///
    /// The Desktop keeps one list per account under `claude-code-sessions/`,
    /// and signing out inside the app leaves the old account's list (and its
    /// sidebar groups, in the app's config) in place. A directory that is
    /// then parked under the new account's name takes them along, which is
    /// how someone's chats vanish from the sidebar without a byte being
    /// deleted. Known here so a switch can say so.
    pub other_accounts: Vec<String>,
}

pub fn inspect(dir: &Path) -> Inspection {
    let installed = dir.is_dir();
    let read = if installed {
        read_config(dir)
    } else {
        Ok(None)
    };
    // Held, not discarded: everything that moves a directory asks this
    // first, and a directory whose owner cannot be read is not one to move.
    let identity_unreadable = read.as_ref().err().map(|e| e.to_string());
    let config = read.ok().flatten();
    let account_uuid = config
        .as_ref()
        .and_then(|c| c.last_known_account_uuid.clone())
        .filter(|u| !u.is_empty());
    let signed_in = config.as_ref().and_then(|c| c.window_was_signed_in);
    let other_accounts = if installed {
        accounts_with_sessions(dir)
            .into_iter()
            .filter(|u| Some(u) != account_uuid.as_ref())
            .collect()
    } else {
        Vec::new()
    };
    Inspection {
        installed,
        running: installed && is_running(dir),
        account_uuid,
        identity_unreadable,
        signed_in,
        other_accounts,
    }
}

/// Accounts with at least one Code session listed in this directory.
///
/// Reads directory names only: `claude-code-sessions/<account>/<org>/` with
/// a `local_*.json` inside. The files themselves are not opened.
fn accounts_with_sessions(dir: &Path) -> Vec<String> {
    let Ok(accounts) = std::fs::read_dir(dir.join("claude-code-sessions")) else {
        return Vec::new();
    };
    let mut found: Vec<String> = accounts
        .flatten()
        .filter(|a| a.path().is_dir())
        .filter(|a| {
            std::fs::read_dir(a.path())
                .map(|orgs| {
                    orgs.flatten().any(|o| {
                        std::fs::read_dir(o.path())
                            .map(|f| {
                                f.flatten()
                                    .any(|e| e.file_name().to_string_lossy().starts_with("local_"))
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        })
        .map(|a| a.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

/// The Desktop's own record, or why it could not be read.
///
/// `Ok(None)` is a directory with no `config.json` at all: a Desktop that has
/// never been started. Anything else -- a read that failed, a file being
/// rewritten as we looked, one a crash truncated -- is an `Err`, and the
/// difference matters more than it looks: the identity in that file is the
/// only thing that says whose the live login is, and treating "cannot read
/// it" as "nobody's" is how a live login gets parked where nothing will look
/// for it again.
fn read_config(dir: &Path) -> crate::Result<Option<DesktopConfig>> {
    let path = dir.join("config.json");
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CcredError::Io { path, source }),
    };
    if let Some(e) = crate::store::encoding_error(&raw, &path) {
        return Err(e);
    }
    serde_json::from_slice(&crate::store::without_bom(raw))
        .map(Some)
        .map_err(|source| CcredError::Json { path, source })
}

#[cfg(test)]
fn account_uuid(dir: &Path) -> Option<String> {
    read_config(dir)
        .ok()
        .flatten()?
        .last_known_account_uuid
        .filter(|u| !u.is_empty())
}

/// Is the Desktop running out of this directory?
///
/// Electron takes Chromium's single-instance lock in its data directory. On
/// Linux and macOS that is a `SingletonLock` symlink whose target names the
/// holder as `<host>-<pid>`; a target whose process is gone is what a crash
/// leaves behind, and Chromium itself treats it as stale. On Windows it is
/// a `lockfile` held open for writing with read-only sharing and deleted on
/// close, so one that exists and refuses a second writer is held.
/// The file whose holder says the app is running, per platform. Named in the
/// refusal, since a lock that cannot be read is reported as one that is held.
fn lock_file(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        dir.join("lockfile")
    } else {
        dir.join("SingletonLock")
    }
}

fn is_running(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        let Ok(target) = std::fs::read_link(dir.join("SingletonLock")) else {
            return false;
        };
        let target = target.to_string_lossy();
        target
            .rsplit('-')
            .next()
            .and_then(|p| p.parse::<u32>().ok())
            .is_some_and(crate::proc::is_alive)
    }
    #[cfg(windows)]
    {
        match std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join("lockfile"))
        {
            // Present but free: nothing holds it.
            Ok(_) => false,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            // A sharing violation, or anything else: held, as far as a
            // switch is concerned.
            Err(_) => true,
        }
    }
}

// --- the profiles ---------------------------------------------------------

/// What is recorded about a Desktop profile. No token, no path: the
/// directory, when there is one, sits next to this file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopMeta {
    pub schema: u32,
    pub name: String,
    pub created_at_ms: i64,
    /// When the identity was last confirmed against a live directory, or
    /// the directory last parked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at_ms: Option<i64>,
    #[serde(default)]
    pub account: AccountIdentity,
    /// Whatever a newer version wrote that this one does not model.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The Desktop profiles under `~/.ccred/desktop`.
pub struct DesktopRepo<'a> {
    paths: &'a Paths,
}

impl<'a> DesktopRepo<'a> {
    pub fn new(paths: &'a Paths) -> Self {
        DesktopRepo { paths }
    }

    fn dir(&self, name: &ProfileName) -> crate::Result<PathBuf> {
        self.paths.desktop_profile_dir(name)
    }

    fn meta_path(&self, name: &ProfileName) -> crate::Result<PathBuf> {
        Ok(self.dir(name)?.join(META_FILE))
    }

    /// Where this profile's login sits while another one is live.
    pub fn data_dir(&self, name: &ProfileName) -> crate::Result<PathBuf> {
        Ok(self.dir(name)?.join(DATA_DIR))
    }

    pub fn has_data(&self, name: &ProfileName) -> bool {
        self.data_dir(name).is_ok_and(|d| d.is_dir())
    }

    pub fn exists(&self, name: &ProfileName) -> bool {
        self.meta_path(name).is_ok_and(|p| p.is_file())
    }

    /// Every profile with a metadata file, sorted. A directory that is not
    /// a valid name, or has no metadata -- an `unclaimed-<ms>` parking
    /// place -- is not a profile.
    pub fn list(&self) -> Vec<ProfileName> {
        let Ok(entries) = std::fs::read_dir(self.paths.desktop_store_dir()) else {
            return Vec::new();
        };
        let mut names: Vec<ProfileName> = entries
            .flatten()
            .filter_map(|e| validate_profile_name(&e.file_name().to_string_lossy()).ok())
            .filter(|n| self.exists(n))
            .collect();
        names.sort();
        names
    }

    pub fn meta(&self, name: &ProfileName) -> crate::Result<Option<DesktopMeta>> {
        let path = self.meta_path(name)?;
        let raw = match std::fs::read(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(CcredError::Io { path, source }),
        };
        serde_json::from_slice(&raw)
            .map(Some)
            .map_err(|source| CcredError::Json { path, source })
    }

    /// Record a profile, or bring its identity and timestamp up to date.
    pub fn write(
        &self,
        name: &ProfileName,
        account: AccountIdentity,
        now: i64,
    ) -> crate::Result<DesktopMeta> {
        let meta = match self.meta(name)? {
            Some(mut m) => {
                m.account = account;
                m.last_synced_at_ms = Some(now);
                m
            }
            None => DesktopMeta {
                schema: 1,
                name: name.as_str().to_string(),
                created_at_ms: now,
                last_synced_at_ms: Some(now),
                account,
                extra: serde_json::Map::new(),
            },
        };
        let dir = self.dir(name)?;
        std::fs::create_dir_all(&dir).map_err(|source| CcredError::Io { path: dir, source })?;
        let path = self.meta_path(name)?;
        let bytes = serde_json::to_vec_pretty(&meta).map_err(|source| CcredError::Json {
            path: path.clone(),
            source,
        })?;
        write_atomic(&path, &bytes, true)?;
        Ok(meta)
    }

    /// Say the profile's own record under the name it now has.
    ///
    /// For a rename, which moves the directory: the name inside it is what a
    /// person reads, so leaving the old one there is a record that contradicts
    /// its own location. Nothing decides anything by this field -- the
    /// directory name is what every command uses -- so a failure is a warning.
    pub fn set_name(&self, name: &ProfileName) -> crate::Result<()> {
        let Some(mut meta) = self.meta(name)? else {
            return Ok(());
        };
        meta.name = name.as_str().to_string();
        let path = self.meta_path(name)?;
        let bytes = serde_json::to_vec_pretty(&meta).map_err(|source| CcredError::Json {
            path: path.clone(),
            source,
        })?;
        write_atomic(&path, &bytes, true)
    }

    /// Delete the profile, parked login included. `false` when there was
    /// none. Never touches the Desktop's live directory.
    pub fn remove(&self, name: &ProfileName) -> crate::Result<bool> {
        let dir = self.dir(name)?;
        if !dir.exists() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir).map_err(|source| CcredError::Io { path: dir, source })?;
        Ok(true)
    }

    /// Which profile holds the account the live directory is logged in as.
    ///
    /// An account can be saved under more than one name. `prefer` breaks
    /// the tie, in order.
    pub fn owner(&self, inspection: &Inspection, prefer: &[&ProfileName]) -> Owner {
        if !inspection.installed {
            return Owner::Missing;
        }
        let Some(uuid) = &inspection.account_uuid else {
            return Owner::Unclaimed;
        };
        let holders: Vec<ProfileName> = self
            .list()
            .into_iter()
            .filter(|name| {
                self.meta(name)
                    .ok()
                    .flatten()
                    .and_then(|m| m.account.account_uuid)
                    .is_some_and(|u| &u == uuid)
            })
            .collect();
        for p in prefer {
            if holders.contains(p) {
                return Owner::Profile((*p).clone());
            }
        }
        if let Some(name) = holders.into_iter().next() {
            return Owner::Profile(name);
        }
        // Nobody holds this account. A profile started with `--new` is
        // waiting for exactly this: its first login. One such profile, and
        // the account is its; two, and there is no telling which.
        let waiting: Vec<ProfileName> = self
            .list()
            .into_iter()
            .filter(|name| {
                self.meta(name)
                    .ok()
                    .flatten()
                    .is_some_and(|m| m.account.account_uuid.is_none())
            })
            .collect();
        match waiting.as_slice() {
            [only] => Owner::Profile(only.clone()),
            _ => Owner::Unclaimed,
        }
    }
}

/// Whose the live directory is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// No directory: the Desktop is not installed, or its directory is
    /// parked and nothing has replaced it yet.
    Missing,
    /// A directory on an account that is not a saved profile -- or one that
    /// has never logged in.
    Unclaimed,
    Profile(ProfileName),
}

// --- switching ------------------------------------------------------------

/// What a switch did, or did not do, to the Desktop.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum DesktopSwitch {
    /// Already logged in as the target.
    AlreadyOn,
    /// On an account that is not a saved profile, with nothing parked for
    /// the target: left where it is.
    LeftAlone,
    Moved {
        /// Where the previous directory went: a profile's display name, or
        /// an `unclaimed-<ms>` name for one that belonged to no profile.
        #[serde(skip_serializing_if = "Option::is_none")]
        parked_as: Option<String>,
        /// Whether the target's parked directory was brought in. When not,
        /// the Desktop starts fresh and asks for a login.
        restored: bool,
    },
    /// Something refused half-way; the message says what to do by hand.
    Failed {
        error: String,
        /// Where the live login ended up, when it had already been parked.
        /// Without this the directory is somewhere the report does not name
        /// and no command looks: the person finishing the move by hand has
        /// to be told where to find it.
        #[serde(skip_serializing_if = "Option::is_none")]
        parked_as: Option<String>,
    },
}

/// The moves a switch will make, decided before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Leave(DesktopSwitch),
    Move {
        park_as: Option<String>,
        restore: bool,
    },
}

/// Decide, from facts alone.
///
/// The Desktop is moved only when there is something to move: a live
/// directory that belongs to some other profile, or a parked one for the
/// target. A live directory that belongs to no profile is left where it is
/// unless the target has a parked one to bring in -- then it is parked under
/// a name nobody will switch back to, and the report says where.
pub fn plan(owner: &Owner, target: &ProfileName, target_parked: bool, now_ms: i64) -> Step {
    match owner {
        Owner::Profile(p) if p == target => Step::Leave(DesktopSwitch::AlreadyOn),
        // Nothing to park and nothing to restore: the Desktop will start
        // fresh and ask for a login, which is the state asked for.
        Owner::Missing if !target_parked => Step::Move {
            park_as: None,
            restore: false,
        },
        Owner::Unclaimed if !target_parked => Step::Leave(DesktopSwitch::LeftAlone),
        Owner::Profile(p) => Step::Move {
            park_as: Some(p.as_str().to_string()),
            restore: target_parked,
        },
        Owner::Unclaimed => Step::Move {
            park_as: Some(format!("{UNCLAIMED_PREFIX}{now_ms}")),
            restore: target_parked,
        },
        Owner::Missing => Step::Move {
            park_as: None,
            restore: true,
        },
    }
}

/// Refuse a move that cannot be carried out, before anything is touched.
pub fn preflight(paths: &Paths, inspection: &Inspection, step: &Step) -> crate::Result<()> {
    let Step::Move { park_as, .. } = step else {
        return Ok(());
    };
    // Before anything else, because everything else assumes this is known.
    // A directory whose `config.json` cannot be read still belongs to
    // somebody -- and with no id to match, `owner` calls it unclaimed and the
    // plan parks it under `unclaimed-<ms>`, which no command restores. That
    // is a live login lost to a read error.
    if let Some(why) = &inspection.identity_unreadable {
        return Err(CcredError::UnsafeWrite(format!(
            "Claude Desktop's record of its account cannot be read ({why}), so there is no telling whose the live login is; nothing was moved"
        )));
    }
    if inspection.running {
        // The lock is named because the answer is sometimes wrong in the
        // safe direction: on Windows anything but a clean open counts as
        // held, so a lock file that cannot be opened for its own reasons --
        // a permission left by a restored profile, an antivirus holding it --
        // reads as an app that is not running. Then the only way forward is
        // to look at the file, so the message says which one.
        return Err(CcredError::UnsafeWrite(format!(
            concat!(
                "Claude Desktop is running; quit it first. Its login is a directory, ",
                "and one cannot be moved under a running app. If it is closed, the ",
                "lock it leaves behind is {}"
            ),
            lock_file(paths.desktop_dir()).display()
        )));
    }
    if let Some(name) = park_as {
        let dest = parking_place(paths, name);
        if dest.exists() {
            return Err(CcredError::UnsafeWrite(format!(
                "'{name}{SUFFIX}' already has a parked login at {}, and the live one \
                 cannot be parked over it; move one of them away by hand",
                dest.display()
            )));
        }
    }
    Ok(())
}

/// Where a live directory goes when parked under `name` -- a profile's
/// `data` directory, or an `unclaimed-<ms>` one of the same shape.
fn parking_place(paths: &Paths, name: &str) -> PathBuf {
    paths.desktop_store_dir().join(name).join(DATA_DIR)
}

/// Carry the moves out. Two renames at most, each atomic on its own.
pub fn apply(paths: &Paths, target: &ProfileName, step: Step) -> DesktopSwitch {
    match step {
        Step::Leave(outcome) => outcome,
        Step::Move { park_as, restore } => {
            match apply_move(paths, target, park_as.as_deref(), restore) {
                Ok(()) => DesktopSwitch::Moved {
                    parked_as: park_as.map(|n| {
                        if n.starts_with(UNCLAIMED_PREFIX) {
                            n
                        } else {
                            format!("{n}{SUFFIX}")
                        }
                    }),
                    restored: restore,
                },
                Err(f) => DesktopSwitch::Failed {
                    error: f.error.to_string(),
                    // Only when the first rename went through: that is the
                    // case where the live login is no longer where the
                    // Desktop will look for it.
                    parked_as: park_as
                        .filter(|_| f.parked)
                        .map(|n| parking_place(paths, &n).display().to_string()),
                },
            }
        }
    }
}

/// A move that stopped, and whether the live login had already been parked
/// when it did. The second half is what a person needs to finish it by hand.
struct MoveFailure {
    error: CcredError,
    parked: bool,
}

fn apply_move(
    paths: &Paths,
    target: &ProfileName,
    park_as: Option<&str>,
    restore: bool,
) -> Result<(), MoveFailure> {
    let live = paths.desktop_dir();
    let mut parked = false;
    if let Some(name) = park_as {
        let dest = parking_place(paths, name);
        // A parking place is built by joining, so it has a parent -- but this
        // runs while the live login is about to be moved, and a panic there
        // would leave someone with no message and a directory in mid-air.
        let parent = dest.parent().unwrap_or(&dest);
        std::fs::create_dir_all(parent).map_err(|source| MoveFailure {
            error: CcredError::Io {
                path: parent.to_path_buf(),
                source,
            },
            parked: false,
        })?;
        rename(live, &dest).map_err(|error| MoveFailure {
            error,
            parked: false,
        })?;
        parked = true;
    }
    if restore {
        let from = DesktopRepo::new(paths)
            .data_dir(target)
            .map_err(|error| MoveFailure { error, parked })?;
        rename(&from, live).map_err(|error| MoveFailure { error, parked })?;
    }
    Ok(())
}

/// A rename that names both ends when it fails: the one error worth
/// expecting is a parking place on another file system, which `rename`
/// cannot cross and this tool will not copy a token across.
fn rename(from: &Path, to: &Path) -> crate::Result<()> {
    std::fs::rename(from, to).map_err(|source| CcredError::Io {
        path: PathBuf::from(format!("{} -> {}", from.display(), to.display())),
        source,
    })
}

/// Parked directories that belong to no profile: what a switch put aside
/// when the Desktop was on an account nobody had saved.
pub fn unclaimed(paths: &Paths) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(paths.desktop_store_dir()) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(UNCLAIMED_PREFIX)
        })
        .map(|e| e.path().join(DATA_DIR))
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    found
}

// --- session lists left behind by a sign-out inside the app ---------------
//
// The Desktop keeps the sidebar's list of Code sessions per account, under
// `claude-code-sessions/<account>/<org>/local_*.json`, and the sidebar's
// groups in `claude_desktop_config.json` under a key named for the same
// account and organization. Signing out inside the app and in as someone
// else keeps both where they are. Then the directory belongs to the new
// account, gets parked under that name, and the first account's chats are
// gone from its sidebar although not a byte of them was deleted -- the
// transcripts were always in `~/.claude/projects/`, and the list and the
// groups are sitting in another profile's parked directory.
//
// So a switch to a profile gathers that account's list and groups from
// wherever they have ended up -- the live directory about to be parked,
// any parked one -- into the profile's `carry/`, and puts them into the
// live directory once that is the account's own. Two kinds of file are
// touched inside the Desktop's directory for this, and nothing else: the
// per-account list directory is moved as a whole, and one key in the
// config is filled in where it is empty.

const SESSIONS_DIR: &str = "claude-code-sessions";
const DESKTOP_CONFIG: &str = "claude_desktop_config.json";
const CARRY_DIR: &str = "carry";
const CARRY_GROUPS: &str = "groups.json";
const GROUP_SCOPES: &str = "dframe-group-scopes";

/// What was gathered or put back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Carried {
    pub sessions: usize,
    pub groups: usize,
}

/// What a carry put back, and what it could not.
#[derive(Debug, Clone, Default)]
pub struct CarriedIn {
    pub carried: Carried,
    /// Why the sidebar groups stayed in the carry. They are kept for the
    /// next switch rather than dropped, and this says so.
    pub groups_kept_back: Option<String>,
    /// The same for the session lists.
    pub sessions_kept_back: Option<String>,
}

impl Carried {
    pub fn is_empty(&self) -> bool {
        self.sessions == 0 && self.groups == 0
    }
    pub fn add(&mut self, other: Carried) {
        self.sessions += other.sessions;
        self.groups += other.groups;
    }
}

impl<'a> DesktopRepo<'a> {
    fn carry_dir(&self, name: &ProfileName) -> crate::Result<PathBuf> {
        Ok(self.dir(name)?.join(CARRY_DIR))
    }

    /// What is waiting in a profile's carry, if anything.
    pub fn carry_pending(&self, name: &ProfileName) -> Carried {
        let Ok(dir) = self.carry_dir(name) else {
            return Carried::default();
        };
        Carried {
            sessions: count_sessions(&dir.join("sessions")),
            groups: read_scopes(&dir.join(CARRY_GROUPS))
                .values()
                .map(|s| s.groups.len())
                .sum(),
        }
    }

    /// Gather account `uuid`'s session list and groups out of `from`, a
    /// Desktop data directory, into the profile's carry. The list is moved
    /// -- it is this account's, not that directory's -- and the groups are
    /// copied; the config is left as it was.
    pub fn carry_out(&self, name: &ProfileName, uuid: &str, from: &Path) -> crate::Result<Carried> {
        if uuid.is_empty() {
            return Ok(Carried::default());
        }
        let list = from.join(SESSIONS_DIR).join(uuid);
        let scopes = read_scopes(&from.join(DESKTOP_CONFIG));
        let mine: Scopes = scopes
            .into_iter()
            .filter(|(k, _)| k.starts_with(&format!("{uuid}/")))
            .collect();
        if !list.is_dir() && mine.is_empty() {
            return Ok(Carried::default());
        }
        let carry = self.carry_dir(name)?;
        let mut out = Carried::default();
        if list.is_dir() {
            out.sessions = count_sessions(&list);
            move_merge(&list, &carry.join("sessions").join(uuid))?;
        }
        if !mine.is_empty() {
            let path = carry.join(CARRY_GROUPS);
            let mut have = read_scopes(&path);
            for (k, v) in mine {
                out.groups += v.groups.len();
                merge_scope(have.entry(k).or_default(), v);
            }
            std::fs::create_dir_all(&carry).map_err(|source| CcredError::Io {
                path: carry.clone(),
                source,
            })?;
            let bytes = serde_json::to_vec_pretty(&have).map_err(|source| CcredError::Json {
                path: path.clone(),
                source,
            })?;
            write_atomic(&path, &bytes, true)?;
        }
        Ok(out)
    }

    /// Put the profile's carry into `live`, which must be that account's
    /// directory, and forget the carry. Files already there are never
    /// overwritten; a group scope already filled in is merged into.
    pub fn carry_in(
        &self,
        name: &ProfileName,
        uuid: &str,
        live: &Path,
    ) -> crate::Result<CarriedIn> {
        let carry = self.carry_dir(name)?;
        if !carry.is_dir() || uuid.is_empty() {
            return Ok(CarriedIn::default());
        }
        let mut out = CarriedIn::default();
        let list = carry.join("sessions").join(uuid);
        if list.is_dir() {
            // Not a `?`, for the reason the groups below are not: the
            // directories have already moved. A carry on another file
            // system is the likeliest way this fails, since a rename cannot
            // cross one, and the chats are better left waiting than lost to
            // an error nobody can act on mid-switch.
            match move_merge(&list, &live.join(SESSIONS_DIR).join(uuid)) {
                Ok(moved) => out.carried.sessions = moved,
                Err(e) => out.sessions_kept_back = Some(e.to_string()),
            }
        }
        let groups = carry.join(CARRY_GROUPS);
        if groups.is_file() {
            // Not a `?`. By the time this runs the directories have already
            // moved, so raising here reported a switch that had happened as
            // a failure -- and the Desktop's config is a file that can be
            // absent (a fresh install), be mid-write, or have a shape this
            // version does not know. The groups stay in the carry for the
            // next switch, which is what the carry is for.
            match fill_in_scopes(&live.join(DESKTOP_CONFIG), read_scopes(&groups)) {
                Ok(added) => out.carried.groups = added,
                Err(e) => out.groups_kept_back = Some(e.to_string()),
            }
        }
        if out.groups_kept_back.is_some() || out.sessions_kept_back.is_some() {
            // Only what went in is cleared; the groups file is the thing
            // being kept, and dropping it would lose them for good.
            let _ = std::fs::remove_dir_all(carry.join("sessions"));
        } else {
            std::fs::remove_dir_all(&carry).map_err(|source| CcredError::Io {
                path: carry,
                source,
            })?;
        }
        Ok(out)
    }

    /// The profile that holds an account, if any. Ties go to the first
    /// name in order; `owner` is the one to ask when a preference matters.
    pub fn profile_for(&self, uuid: &str) -> Option<ProfileName> {
        self.list().into_iter().find(|n| {
            self.meta(n)
                .ok()
                .flatten()
                .and_then(|m| m.account.account_uuid)
                .as_deref()
                == Some(uuid)
        })
    }

    /// Every parked directory of another profile that holds this account's
    /// session list: where a sign-out inside the app leaves it.
    pub fn parked_holding(&self, uuid: &str, except: &ProfileName) -> Vec<(ProfileName, PathBuf)> {
        if uuid.is_empty() {
            return Vec::new();
        }
        self.list()
            .into_iter()
            .filter(|n| n != except)
            .filter_map(|n| {
                let data = self.data_dir(&n).ok()?;
                (data.join(SESSIONS_DIR).join(uuid).is_dir()).then_some((n, data))
            })
            .collect()
    }
}

/// One account-and-organization's sidebar groups, as the Desktop stores
/// them. Anything else in the object is carried along untouched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Scope {
    #[serde(default)]
    groups: Vec<serde_json::Value>,
    #[serde(default)]
    assignments: serde_json::Map<String, serde_json::Value>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

type Scopes = std::collections::BTreeMap<String, Scope>;

/// Add what `from` has that `into` lacks: groups by id, assignments by key.
fn merge_scope(into: &mut Scope, from: Scope) {
    for g in from.groups {
        let id = g.get("id").cloned();
        if !into.groups.iter().any(|h| h.get("id").cloned() == id) {
            into.groups.push(g);
        }
    }
    for (k, v) in from.assignments {
        into.assignments.entry(k).or_insert(v);
    }
}

/// The group scopes in a Desktop config, or a carry file. Unreadable is
/// empty: there is nothing to carry from a file that cannot be read.
fn read_scopes(path: &Path) -> Scopes {
    let Ok(raw) = std::fs::read(path) else {
        return Scopes::new();
    };
    // Same treatment as every other JSON this program reads: a mark a
    // Windows editor left on the front is not a malformation.
    let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&crate::store::without_bom(raw))
    else {
        return Scopes::new();
    };
    let scopes = if path.file_name().is_some_and(|f| f == DESKTOP_CONFIG) {
        doc.pointer(&format!("/preferences/epitaxyPrefs/{GROUP_SCOPES}"))
    } else {
        Some(&doc)
    };
    scopes
        .and_then(|v| serde_json::from_value::<Scopes>(v.clone()).ok())
        .unwrap_or_default()
}

/// Merge scopes into a Desktop config's group key, and write it back in
/// the app's own shape: two-space JSON, no trailing newline, private. The
/// rest of the file is what it was. Returns how many groups were added.
fn fill_in_scopes(config: &Path, scopes: Scopes) -> crate::Result<usize> {
    let raw = std::fs::read(config).map_err(|source| CcredError::Io {
        path: config.to_path_buf(),
        source,
    })?;
    let raw = crate::store::without_bom(raw);
    if let Some(e) = crate::store::encoding_error(&raw, config) {
        return Err(e);
    }
    let mut doc: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|source| CcredError::Json {
            path: config.to_path_buf(),
            source,
        })?;
    let slot = doc
        .pointer_mut("/preferences")
        .and_then(|p| p.as_object_mut())
        .map(|p| {
            p.entry("epitaxyPrefs")
                .or_insert_with(|| serde_json::json!({}))
        })
        .and_then(|e| e.as_object_mut())
        .map(|e| {
            e.entry(GROUP_SCOPES)
                .or_insert_with(|| serde_json::json!({}))
        });
    let Some(slot) = slot else {
        return Err(CcredError::UnsafeWrite(format!(
            "{} does not have the shape this version knows; the groups were left in the carry",
            config.display()
        )));
    };
    let mut have: Scopes = serde_json::from_value(slot.clone()).unwrap_or_default();
    let mut added = 0;
    for (k, v) in scopes {
        let before = have.get(&k).map(|s| s.groups.len()).unwrap_or(0);
        merge_scope(have.entry(k.clone()).or_default(), v);
        added += have[&k].groups.len() - before;
    }
    *slot = serde_json::to_value(&have).map_err(|source| CcredError::Json {
        path: config.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|source| CcredError::Json {
        path: config.to_path_buf(),
        source,
    })?;
    write_atomic(config, &bytes, true)?;
    Ok(added)
}

/// `local_*.json` files under a session-list directory, at any depth.
fn count_sessions(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                count_sessions(&p)
            } else {
                usize::from(e.file_name().to_string_lossy().starts_with("local_"))
            }
        })
        .sum()
}

/// Move a tree into another, file by file, never replacing what is there.
/// What was moved is gone from `from`; what was not fits in the report.
/// Move a tree into another, keeping whatever is already there, and say how
/// many sessions actually went.
///
/// The count is of what moved, not of what was there: a file whose name is
/// already taken on the other side is deliberately left where it is -- the
/// one on the other side is the account's own and newer -- and reporting it
/// as restored would be a claim about somebody's chats that is not true.
fn move_merge(from: &Path, into: &Path) -> crate::Result<usize> {
    std::fs::create_dir_all(into).map_err(|source| CcredError::Io {
        path: into.to_path_buf(),
        source,
    })?;
    let entries = std::fs::read_dir(from).map_err(|source| CcredError::Io {
        path: from.to_path_buf(),
        source,
    })?;
    let mut moved = 0;
    for entry in entries.flatten() {
        let src = entry.path();
        let dst = into.join(entry.file_name());
        if src.is_dir() {
            moved += move_merge(&src, &dst)?;
        } else if !dst.exists() {
            rename(&src, &dst)?;
            if entry.file_name().to_string_lossy().starts_with("local_") {
                moved += 1;
            }
        }
    }
    // Empty now, or holding only files that already existed on the other
    // side: either way, not worth leaving.
    let _ = std::fs::remove_dir(from);
    Ok(moved)
}

// --- one sidebar for every account -----------------------------------------
//
// The Desktop lists Code sessions per account. The sessions themselves are
// not per account -- they are transcripts under `~/.claude/projects/`, and
// Claude Code opens any of them under whatever login it has -- so someone
// who treats accounts as nothing but a source of tokens wants the same
// sidebar whichever account the Desktop is on. This keeps a union of every
// account's list under `~/.ccred/desktop/sidebar/` and, on each switch,
// writes into the target account's list whatever it lacks.
//
// What is copied is the part of an entry that means the same under any
// account: which transcript, where, what it is called, when. What is bound
// to the account -- remote bridge ids, connector configuration, permission
// grants -- is left out, and the Desktop fills it in for the new account
// when the session is opened. Verified against a live Desktop: an entry
// reduced to these fields is listed, opens, and gets the rest written back.
//
// Titles, archiving and deletion follow the most recent word: an entry
// changed under one account changes under the others on the next switch,
// and one deleted under any account is deleted everywhere.

const SIDEBAR_DIR: &str = "sidebar";
const ARCHIVED_IDX: &str = "archived-sessions.idx";
const ENTRY_PREFIX: &str = "local_";
const DELETED_PREFIX: &str = "deleted_";

/// The fields of a session entry that are the session's, not the account's.
const PORTABLE_FIELDS: &[&str] = &[
    "sessionId",
    "cliSessionId",
    "priorCliSessionIds",
    "cwd",
    "originCwd",
    "title",
    "titleSource",
    "previousTitles",
    "titleTurn",
    "createdAt",
    "lastActivityAt",
    "lastFocusedAt",
    "model",
    "effort",
    "permissionMode",
    "isArchived",
    "completedTurns",
];

/// What a sidebar pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Sidebar {
    /// Entries the union gained or updated from this account's list.
    pub collected: usize,
    /// Entries written into this account's list, new or brought up to date.
    pub written: usize,
    /// Entries removed from this account's list because another account
    /// deleted them.
    pub removed: usize,
    /// Groups this account's config gained.
    pub groups: usize,
}

impl Sidebar {
    pub fn is_empty(&self) -> bool {
        self.written == 0 && self.removed == 0 && self.groups == 0
    }
}

/// The shared union of every account's sidebar.
struct SidebarStore {
    dir: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SidebarState {
    /// Session ids deleted under some account, so no other account's list
    /// resurrects them.
    #[serde(default)]
    deleted: std::collections::BTreeSet<String>,
    /// Session ids archived under some account.
    #[serde(default)]
    archived: std::collections::BTreeSet<String>,
    /// Sidebar groups and assignments, the union over accounts.
    #[serde(default)]
    groups: Scope,
}

impl SidebarStore {
    fn new(paths: &Paths) -> Self {
        SidebarStore {
            dir: paths.desktop_store_dir().join(SIDEBAR_DIR),
        }
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    fn state(&self) -> SidebarState {
        std::fs::read(self.state_path())
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default()
    }

    fn write_state(&self, state: &SidebarState) -> crate::Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|source| CcredError::Io {
            path: self.dir.clone(),
            source,
        })?;
        let path = self.state_path();
        let bytes = serde_json::to_vec_pretty(state).map_err(|source| CcredError::Json {
            path: path.clone(),
            source,
        })?;
        write_atomic(&path, &bytes, true)
    }

    fn entries_dir(&self) -> PathBuf {
        self.dir.join("sessions")
    }

    fn entry_path(&self, id: &str) -> PathBuf {
        self.entries_dir().join(format!("{id}.json"))
    }

    fn entries(&self) -> Vec<(String, serde_json::Map<String, serde_json::Value>)> {
        let Ok(dir) = std::fs::read_dir(self.entries_dir()) else {
            return Vec::new();
        };
        let mut out: Vec<_> = dir
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let id = name.strip_suffix(".json")?.to_string();
                let entry = read_entry(&e.path())?;
                Some((id, entry))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn write_entry(
        &self,
        id: &str,
        entry: &serde_json::Map<String, serde_json::Value>,
    ) -> crate::Result<()> {
        let dir = self.entries_dir();
        std::fs::create_dir_all(&dir).map_err(|source| CcredError::Io {
            path: dir.clone(),
            source,
        })?;
        let path = self.entry_path(id);
        let bytes = serde_json::to_vec_pretty(entry).map_err(|source| CcredError::Json {
            path: path.clone(),
            source,
        })?;
        write_atomic(&path, &bytes, true)
    }
}

fn read_entry(path: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let raw = std::fs::read(path).ok()?;
    match serde_json::from_slice::<serde_json::Value>(&raw).ok()? {
        serde_json::Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Just the fields that travel.
fn portable(
    entry: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    PORTABLE_FIELDS
        .iter()
        .filter_map(|k| entry.get(*k).map(|v| ((*k).to_string(), v.clone())))
        .collect()
}

fn activity(entry: &serde_json::Map<String, serde_json::Value>) -> i64 {
    entry
        .get("lastActivityAt")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// The `<uuid>/<org>/` list directories of an account in a Desktop data
/// directory. Usually one.
fn list_dirs(data: &Path, uuid: &str) -> Vec<(String, PathBuf)> {
    let Ok(orgs) = std::fs::read_dir(data.join(SESSIONS_DIR).join(uuid)) else {
        return Vec::new();
    };
    let mut out: Vec<_> = orgs
        .flatten()
        .filter(|o| o.path().is_dir())
        .map(|o| (o.file_name().to_string_lossy().into_owned(), o.path()))
        .collect();
    out.sort();
    out
}

fn read_archived(dir: &Path) -> Vec<String> {
    #[derive(Deserialize)]
    struct Idx {
        #[serde(default)]
        archived: Vec<String>,
    }
    std::fs::read(dir.join(ARCHIVED_IDX))
        .ok()
        .and_then(|raw| serde_json::from_slice::<Idx>(&raw).ok())
        .map(|i| i.archived)
        .unwrap_or_default()
}

/// Take account `uuid`'s sidebar in `data` into the union: entries by most
/// recent activity, deletions and archiving as facts, groups merged.
pub fn sidebar_collect(paths: &Paths, data: &Path, uuid: &str) -> crate::Result<Sidebar> {
    if uuid.is_empty() {
        return Ok(Sidebar::default());
    }
    let store = SidebarStore::new(paths);
    let mut state = store.state();
    let mut out = Sidebar::default();
    let mut touched = false;
    for (org, dir) in list_dirs(data, uuid) {
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_prefix(DELETED_PREFIX) {
                let id = format!("{ENTRY_PREFIX}{id}");
                if state.deleted.insert(id.clone()) {
                    touched = true;
                }
                let _ = std::fs::remove_file(store.entry_path(&id));
                continue;
            }
            let Some(id) = name
                .strip_suffix(".json")
                .filter(|s| s.starts_with(ENTRY_PREFIX))
            else {
                continue;
            };
            if state.deleted.contains(id) {
                continue;
            }
            let Some(entry) = read_entry(&f.path()) else {
                continue;
            };
            let mine = portable(&entry);
            let newer = read_entry(&store.entry_path(id))
                .is_none_or(|have| activity(&mine) >= activity(&have));
            if newer {
                store.write_entry(id, &mine)?;
                out.collected += 1;
            }
        }
        for id in read_archived(&dir) {
            if state.archived.insert(id) {
                touched = true;
            }
        }
        let scopes = read_scopes(&data.join(DESKTOP_CONFIG));
        if let Some(scope) = scopes.get(&format!("{uuid}/{org}")) {
            let before = state.groups.groups.len() + state.groups.assignments.len();
            merge_scope(&mut state.groups, scope.clone());
            if state.groups.groups.len() + state.groups.assignments.len() != before {
                touched = true;
            }
        }
    }
    if touched {
        store.write_state(&state)?;
    }
    Ok(out)
}

/// Bring account `uuid`'s sidebar in `data` up to the union: missing entries
/// written, stale ones brought up to date, deleted ones removed, the
/// archived list and the groups filled in. Every file the Desktop wrote
/// for the account keeps its account-bound fields.
pub fn sidebar_spread(paths: &Paths, data: &Path, uuid: &str) -> crate::Result<Sidebar> {
    if uuid.is_empty() {
        return Ok(Sidebar::default());
    }
    let store = SidebarStore::new(paths);
    let state = store.state();
    let entries = store.entries();
    let mut out = Sidebar::default();
    let mut scopes = Scopes::new();
    for (org, dir) in list_dirs(data, uuid) {
        for (id, want) in &entries {
            let path = dir.join(format!("{id}.json"));
            match read_entry(&path) {
                Some(have) if activity(&have) >= activity(want) => {}
                Some(mut have) => {
                    for (k, v) in want {
                        have.insert(k.clone(), v.clone());
                    }
                    write_entry_file(&path, &have)?;
                    out.written += 1;
                }
                None => {
                    if dir
                        .join(format!(
                            "{DELETED_PREFIX}{}",
                            id.trim_start_matches(ENTRY_PREFIX)
                        ))
                        .exists()
                    {
                        continue;
                    }
                    write_entry_file(&path, want)?;
                    out.written += 1;
                }
            }
        }
        for id in &state.deleted {
            let path = dir.join(format!("{id}.json"));
            if path.exists() {
                std::fs::remove_file(&path).map_err(|source| CcredError::Io { path, source })?;
                out.removed += 1;
            }
        }
        let mut archived: Vec<String> = read_archived(&dir);
        let mut grew = false;
        for id in &state.archived {
            if !archived.contains(id) && dir.join(format!("{id}.json")).exists() {
                archived.push(id.clone());
                grew = true;
            }
        }
        if grew {
            // Strings in, JSON out: this cannot fail, and saying so in a
            // panic would still end an unattended run with nothing to read.
            let bytes = serde_json::to_vec(&serde_json::json!({"v": 1, "archived": archived}))
                .map_err(|source| CcredError::Json {
                    path: dir.join(ARCHIVED_IDX),
                    source,
                })?;
            write_atomic(&dir.join(ARCHIVED_IDX), &bytes, true)?;
        }
        if !state.groups.groups.is_empty() {
            scopes.insert(format!("{uuid}/{org}"), state.groups.clone());
        }
    }
    if !scopes.is_empty() {
        out.groups = fill_in_scopes(&data.join(DESKTOP_CONFIG), scopes)?;
    }
    Ok(out)
}

fn write_entry_file(
    path: &Path,
    entry: &serde_json::Map<String, serde_json::Value>,
) -> crate::Result<()> {
    let bytes = serde_json::to_vec_pretty(entry).map_err(|source| CcredError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    write_atomic(path, &bytes, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> ProfileName {
        validate_profile_name(s).unwrap()
    }

    fn moved(step: &Step) -> (Option<String>, bool) {
        match step {
            Step::Move { park_as, restore } => (park_as.clone(), *restore),
            Step::Leave(o) => panic!("expected a move, got {o:?}"),
        }
    }

    #[test]
    fn the_suffix_routes_a_name_and_the_plain_validator_refuses_it() {
        assert_eq!(
            Handle::parse("work-desktop").unwrap(),
            Handle::Desktop(name("work"))
        );
        assert_eq!(
            Handle::parse("work").unwrap(),
            Handle::ClaudeCode(name("work"))
        );
        assert!(Handle::parse("-desktop").is_err(), "an empty stem");
        assert!(validate_profile_name("work-desktop").is_err());
        assert_eq!(display_name(&name("work")), "work-desktop");
    }

    #[test]
    fn a_desktop_already_on_the_target_is_left_where_it_is() {
        let step = plan(&Owner::Profile(name("work")), &name("work"), true, 1);
        assert_eq!(step, Step::Leave(DesktopSwitch::AlreadyOn));
    }

    #[test]
    fn an_empty_slot_with_nothing_parked_means_a_fresh_start() {
        let step = plan(&Owner::Missing, &name("work"), false, 1);
        assert_eq!(moved(&step), (None, false));
    }

    #[test]
    fn a_foreign_account_is_left_alone_unless_the_target_has_a_parked_login() {
        let step = plan(&Owner::Unclaimed, &name("work"), false, 1);
        assert_eq!(step, Step::Leave(DesktopSwitch::LeftAlone));
        let step = plan(&Owner::Unclaimed, &name("work"), true, 1_700);
        assert_eq!(moved(&step), (Some("unclaimed-1700".into()), true));
    }

    #[test]
    fn another_profiles_login_is_parked_under_its_own_name() {
        // Under the name of the account it holds -- never the target's.
        let step = plan(&Owner::Profile(name("personal")), &name("work"), false, 1);
        assert_eq!(moved(&step), (Some("personal".into()), false));
        let step = plan(&Owner::Profile(name("personal")), &name("work"), true, 1);
        assert_eq!(moved(&step), (Some("personal".into()), true));
    }

    #[test]
    fn a_parked_login_is_restored_into_an_empty_slot() {
        let step = plan(&Owner::Missing, &name("work"), true, 1);
        assert_eq!(moved(&step), (None, true));
    }

    /// The validator refuses the suffix without regard to case, so parsing
    /// it with regard to case left `Work-DESKTOP` addressing nothing: read
    /// as a Claude Code name, then refused for ending in `-desktop`.
    #[test]
    fn the_desktop_suffix_is_read_whatever_its_case() {
        assert_eq!(
            Handle::parse("work-DESKTOP").unwrap(),
            Handle::Desktop(validate_profile_name("work").unwrap())
        );
        assert_eq!(
            Handle::parse("work-Desktop").unwrap(),
            Handle::Desktop(validate_profile_name("work").unwrap())
        );
        assert_eq!(
            Handle::parse("work").unwrap(),
            Handle::ClaudeCode(validate_profile_name("work").unwrap())
        );
        // The suffix alone is not a name.
        assert!(Handle::parse("-desktop").is_err());
    }

    /// The live directory belongs to somebody even when the file that says
    /// who cannot be read. With no id to match, `owner` calls it unclaimed
    /// and the plan parks it under `unclaimed-<ms>`, which no command
    /// restores -- a live login lost to a read error.
    #[test]
    fn a_login_whose_owner_cannot_be_read_is_not_moved() {
        let paths = Paths::with_overrides(PathBuf::from("/home/user"), None, None);
        let unreadable = Inspection {
            installed: true,
            running: false,
            account_uuid: None,
            identity_unreadable: Some("malformed JSON in config.json".into()),
            signed_in: None,
            other_accounts: Vec::new(),
        };
        let move_it = Step::Move {
            park_as: Some(format!("{UNCLAIMED_PREFIX}17")),
            restore: true,
        };
        let err = preflight(&paths, &unreadable, &move_it).unwrap_err();
        let said = err.to_string();
        assert!(said.contains("cannot be read"), "{said}");
        assert!(said.contains("nothing was moved"), "{said}");

        // Reading nothing because there is nothing to read is a different
        // thing, and still allowed: that is a Desktop that never logged in.
        let never_logged_in = Inspection {
            installed: true,
            running: false,
            account_uuid: None,
            identity_unreadable: None,
            signed_in: None,
            other_accounts: Vec::new(),
        };
        assert!(preflight(&paths, &never_logged_in, &move_it).is_ok());

        // And a step that moves nothing is never refused for this.
        let stay = Step::Leave(DesktopSwitch::AlreadyOn);
        assert!(preflight(&paths, &unreadable, &stay).is_ok());
    }

    /// Parking over a directory that is already there would bury one login
    /// under another, and neither could be told apart afterwards. Refused
    /// before anything moves, which is the whole point of a preflight.
    #[test]
    fn a_parking_place_that_is_taken_is_refused_before_anything_moves() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::with_overrides(home.path().to_path_buf(), None, None);
        let idle = Inspection {
            installed: true,
            running: false,
            account_uuid: Some("u".into()),
            identity_unreadable: None,
            signed_in: None,
            other_accounts: Vec::new(),
        };
        let step = Step::Move {
            park_as: Some("work".into()),
            restore: false,
        };
        assert!(preflight(&paths, &idle, &step).is_ok(), "nothing there yet");

        std::fs::create_dir_all(paths.desktop_store_dir().join("work").join(DATA_DIR)).unwrap();
        let err = preflight(&paths, &idle, &step).unwrap_err().to_string();
        assert!(err.contains("already has a parked login"), "{err}");
        assert!(err.contains("by hand"), "the way out is named: {err}");
    }

    /// The second rename of a move failing leaves the Desktop with no
    /// directory at all and the live login under a parked name. Whoever
    /// finishes it by hand has to be told which name that is -- and the
    /// command must not report success, which is what it did.
    #[test]
    fn a_move_that_stops_half_way_says_where_the_login_went() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::with_overrides(home.path().to_path_buf(), None, None);
        std::fs::create_dir_all(paths.desktop_dir()).unwrap();
        std::fs::write(paths.desktop_dir().join("marker"), b"live").unwrap();

        // Park, then restore something that is not there: the first rename
        // goes through and the second cannot.
        let outcome = apply(
            &paths,
            &validate_profile_name("work").unwrap(),
            Step::Move {
                park_as: Some("personal".into()),
                restore: true,
            },
        );
        let DesktopSwitch::Failed { error, parked_as } = outcome else {
            panic!("expected a failure, got {outcome:?}");
        };
        assert!(!error.is_empty());
        let parked_as = parked_as.expect("the live login was parked, so it is named");
        assert!(
            Path::new(&parked_as).join("marker").is_file(),
            "the live directory is at the name reported: {parked_as}"
        );
        assert!(
            !paths.desktop_dir().exists(),
            "and it is no longer where the Desktop looks"
        );
    }

    #[test]
    fn a_running_desktop_refuses_only_when_it_would_be_moved() {
        let paths = Paths::with_overrides(PathBuf::from("/home/user"), None, None);
        let running = Inspection {
            installed: true,
            running: true,
            account_uuid: Some("u".into()),
            identity_unreadable: None,
            signed_in: None,
            other_accounts: Vec::new(),
        };
        let stay = Step::Leave(DesktopSwitch::AlreadyOn);
        assert!(preflight(&paths, &running, &stay).is_ok());
        let go = Step::Move {
            park_as: Some("work".into()),
            restore: false,
        };
        let err = preflight(&paths, &running, &go).unwrap_err().to_string();
        assert!(err.contains("Claude Desktop is running"), "{err}");
        assert!(err.contains("quit it first"), "{err}");
    }

    #[test]
    fn the_account_key_is_read_and_nothing_else_is_needed() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(
            &cfg,
            br#"{"oauth:tokenCache":"djExopaque","lastKnownAccountUuid":"abc-123","x":1}"#,
        )
        .unwrap();
        assert_eq!(account_uuid(dir.path()).as_deref(), Some("abc-123"));
        std::fs::write(&cfg, br#"{"lastKnownAccountUuid":""}"#).unwrap();
        assert_eq!(account_uuid(dir.path()), None);
        std::fs::write(&cfg, b"{not json").unwrap();
        assert_eq!(account_uuid(dir.path()), None);
        std::fs::remove_file(&cfg).unwrap();
        assert_eq!(account_uuid(dir.path()), None);
        let none = inspect(dir.path());
        assert!(none.installed && !none.running && none.account_uuid.is_none());
        assert!(!inspect(&dir.path().join("absent")).installed);
    }

    /// A sign-out inside the app leaves the old account's session list in
    /// the directory. Only accounts with sessions count; the logged-in one
    /// is not "other".
    #[test]
    fn session_lists_of_other_accounts_are_noticed_by_directory_name_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            br#"{"lastKnownAccountUuid":"new"}"#,
        )
        .unwrap();
        let lists = dir.path().join("claude-code-sessions");
        for (acc, files) in [
            ("old", &["local_1.json", "scheduled-tasks.json"][..]),
            ("new", &["local_2.json"][..]),
            ("empty", &["scheduled-tasks.json"][..]),
        ] {
            let org = lists.join(acc).join("org");
            std::fs::create_dir_all(&org).unwrap();
            for f in files {
                std::fs::write(org.join(f), b"{}").unwrap();
            }
        }
        assert_eq!(inspect(dir.path()).other_accounts, vec!["old".to_string()]);
    }

    #[test]
    fn a_profile_is_its_metadata_and_a_parking_place_without_one_is_not() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::with_overrides(home.path().to_path_buf(), None, None);
        let repo = DesktopRepo::new(&paths);
        assert!(repo.list().is_empty());
        let identity = AccountIdentity {
            account_uuid: Some("u-1".into()),
            email: Some("ada@example.com".into()),
            ..Default::default()
        };
        repo.write(&name("work"), identity.clone(), 10).unwrap();
        // A parking place with no metadata: not a profile.
        std::fs::create_dir_all(paths.desktop_store_dir().join("unclaimed-5").join(DATA_DIR))
            .unwrap();
        assert_eq!(repo.list(), vec![name("work")]);
        assert_eq!(unclaimed(&paths).len(), 1);
        let meta = repo.meta(&name("work")).unwrap().unwrap();
        assert_eq!(meta.account, identity);
        assert_eq!(meta.created_at_ms, 10);
        // A rewrite keeps the creation time and moves the sync time.
        let again = repo.write(&name("work"), identity, 20).unwrap();
        assert_eq!(again.created_at_ms, 10);
        assert_eq!(again.last_synced_at_ms, Some(20));
        // Ownership is by account id.
        let live = Inspection {
            installed: true,
            running: false,
            account_uuid: Some("u-1".into()),
            identity_unreadable: None,
            signed_in: None,
            other_accounts: Vec::new(),
        };
        assert_eq!(repo.owner(&live, &[]), Owner::Profile(name("work")));
        let other = Inspection {
            account_uuid: Some("u-2".into()),
            ..live.clone()
        };
        assert_eq!(repo.owner(&other, &[]), Owner::Unclaimed);
        assert!(repo.remove(&name("work")).unwrap());
        assert!(!repo.remove(&name("work")).unwrap());
        assert!(repo.list().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_singleton_lock_names_its_holder_and_a_dead_one_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("SingletonLock");
        // Alive: this very process.
        std::os::unix::fs::symlink(format!("host-{}", std::process::id()), &lock).unwrap();
        assert!(is_running(dir.path()));
        // Dead: a PID nothing can hold.
        std::fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink("host-4294967294", &lock).unwrap();
        assert!(!is_running(dir.path()));
        // Nonsense: not a claim of running.
        std::fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink("garbage", &lock).unwrap();
        assert!(!is_running(dir.path()));
    }
}
