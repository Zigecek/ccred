//! The profile store: enumerate, save, restore, and the identity gate.
//!
//! Each profile is a directory under `~/.ccred/profiles/<name>/` holding its
//! own `.credentials.json`, a last-known-good copy, and `ccred.json` metadata
//! that only this tool reads.
//!
//! # The identity gate
//!
//! [`ProfileRepo::save_from`] refuses to write credentials into a profile that
//! belongs to a different account. This is not redundant with the validity and
//! refresh-window checks in [`crate::validate`]: those compare a credential
//! against *itself over time*, and two different accounts can both be perfectly
//! valid with advancing windows.
//!
//! A live near-miss went exactly that way. After logging in as a second
//! account, the active-profile pointer still named the first one. The window
//! check would have waved the write through, because the incoming account's
//! window happened to be *longer* -- and the first account's credentials
//! existed nowhere else, the login having already replaced the live file.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic::write_atomic;
use crate::error::CcredError;
use crate::model::credentials::JsonMap;
use crate::model::{AccountIdentity, AccountSnapshot, CredentialsFile};
use crate::paths::Paths;
use crate::store::file::FileStore;
use crate::store::{CredentialStore, now_ms};
use crate::validate::{ProfileName, validate_credentials, validate_profile_name};

/// How many timestamped backups to keep per profile.
const BACKUPS_KEPT: usize = 10;

/// How a profile's automatic refresh is going.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RefreshState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_ms: Option<i64>,
    #[serde(default)]
    pub consecutive_failures: u32,
    /// Set by the backoff. Nothing is attempted before this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_after_ms: Option<i64>,
    /// Which invocation last actually moved the refresh window on this
    /// machine. Learned by observation, so a build where the cheap probe is
    /// enough never pays for the expensive one twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_probe: Option<String>,
    /// Latched when a human is needed. Cleared by a successful `save`.
    #[serde(default)]
    pub needs_login: bool,
}

/// Metadata `ccred` keeps about a profile. Claude Code never reads this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub schema: u32,
    pub name: String,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at_ms: Option<i64>,
    #[serde(default)]
    pub account: AccountIdentity,
    #[serde(default)]
    pub refresh: RefreshState,
    #[serde(flatten)]
    pub extra: JsonMap,
}

impl ProfileMeta {
    fn new(name: &ProfileName, account: AccountIdentity, now: i64) -> Self {
        ProfileMeta {
            schema: 1,
            name: name.as_str().to_string(),
            created_at_ms: now,
            last_synced_at_ms: Some(now),
            account,
            refresh: RefreshState::default(),
            extra: JsonMap::new(),
        }
    }
}

/// What a save actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    Created,
    Updated,
    /// The stored credentials already matched; nothing was written.
    Unchanged,
}

pub struct ProfileRepo {
    paths: Paths,
}

impl ProfileRepo {
    pub fn new(paths: Paths) -> Self {
        ProfileRepo { paths }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Every profile that has a metadata file, sorted.
    pub fn list(&self) -> crate::Result<Vec<ProfileName>> {
        let dir = self.paths.profiles_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(CcredError::Io { path: dir, source }),
        };

        let mut names = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let raw = entry.file_name().to_string_lossy().to_string();
            // A directory whose name we would not accept cannot have been
            // created by us; skip it rather than failing the whole listing.
            let Ok(name) = validate_profile_name(&raw) else {
                continue;
            };
            if self.meta_path(&name)?.exists() {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn exists(&self, name: &ProfileName) -> crate::Result<bool> {
        Ok(self.meta_path(name)?.exists())
    }

    /// The stored spelling of a profile, matched the way the file system
    /// matches names.
    ///
    /// Windows and a default macOS volume do not tell `Work` from `work`.
    /// A profile saved as `work` and then switched to as `WORK` wrote `WORK`
    /// into the pointer while the directory stayed `work`, so `list` showed
    /// no active profile at all -- and `rm work`, the profile whose
    /// credentials were live, walked straight past the guard that refuses to
    /// delete the active one.
    ///
    /// An unknown name is returned as given, so the caller still reports
    /// "no such profile" in the spelling the person typed.
    pub fn canonical_name(&self, name: &ProfileName) -> ProfileName {
        const CASE_INSENSITIVE_PATHS: bool = cfg!(windows) || cfg!(target_os = "macos");
        // Deliberately not short-circuited on `exists`: on these platforms
        // `profiles/WORK/ccred.json` *does* exist when the directory is
        // `work`, which is the whole problem.
        if !CASE_INSENSITIVE_PATHS {
            return name.clone();
        }
        self.list()
            .unwrap_or_default()
            .into_iter()
            .find(|stored| stored.as_str().eq_ignore_ascii_case(name.as_str()))
            .unwrap_or_else(|| name.clone())
    }

    fn meta_path(&self, name: &ProfileName) -> crate::Result<PathBuf> {
        Ok(self.paths.profile_dir(name)?.join("ccred.json"))
    }

    pub fn meta(&self, name: &ProfileName) -> crate::Result<Option<ProfileMeta>> {
        let path = self.meta_path(name)?;
        match fs::read(&path) {
            Ok(raw) => {
                let meta = serde_json::from_slice(&raw)
                    .map_err(|source| CcredError::Json { path, source })?;
                Ok(Some(meta))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CcredError::Io { path, source }),
        }
    }

    /// The profile's own credential store.
    pub fn store(&self, name: &ProfileName) -> crate::Result<FileStore> {
        Ok(FileStore::new(self.paths.profile_dir(name)?))
    }

    /// Read-modify-write of a profile's metadata.
    ///
    /// Used by the refresh loop to record attempts and backoff without
    /// touching the credentials themselves.
    pub fn update_meta<F>(&self, name: &ProfileName, edit: F) -> crate::Result<()>
    where
        F: FnOnce(&mut ProfileMeta),
    {
        let Some(mut meta) = self.meta(name)? else {
            return Err(CcredError::ProfileNotFound(name.as_str().to_string()));
        };
        edit(&mut meta);
        let bytes = serde_json::to_vec_pretty(&meta).map_err(|source| CcredError::Json {
            path: self.meta_path(name).unwrap_or_default(),
            source,
        })?;
        write_atomic(&self.meta_path(name)?, &bytes, true)
    }

    /// The profile's stored `oauthAccount` blob, if it has one.
    pub fn oauth_account(&self, name: &ProfileName) -> crate::Result<Option<serde_json::Value>> {
        let path = self.paths.profile_oauth_account(name)?;
        match fs::read(&path) {
            Ok(raw) => {
                let value = serde_json::from_slice(&raw)
                    .map_err(|source| CcredError::Json { path, source })?;
                Ok(Some(value))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CcredError::Io { path, source }),
        }
    }

    // ---------------------------------------------------------------- active

    pub fn active(&self) -> crate::Result<Option<ProfileName>> {
        let path = self.paths.active_pointer();
        match fs::read_to_string(&path) {
            Ok(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Ok(None);
                }
                // In the stored spelling: a pointer written as `WORK` for a
                // directory named `work` otherwise matches nothing.
                let name = validate_profile_name(trimmed)
                    .map_err(|e| damaged_pointer(&path, &e.to_string()))?;
                Ok(Some(self.canonical_name(&name)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            // Not a bare I/O error. One file of six bytes decides which
            // profile is live, and when it turns to nonsense every command
            // that reads it said "I/O error at <path>" and exited 1. What it
            // is and what to do about it fit in the message.
            Err(source) => Err(damaged_pointer(&path, &source.to_string())),
        }
    }

    pub fn set_active(&self, name: &ProfileName) -> crate::Result<()> {
        write_atomic(&self.paths.active_pointer(), name.as_str().as_bytes(), true)
    }

    // ------------------------------------------------------------------ save

    /// Refuse credentials that another profile already holds.
    ///
    /// The identity check compares names: what `.claude.json` says against
    /// what the profile was saved as. It cannot see a live store whose tokens
    /// belong to one account while `.claude.json` still names another -- the
    /// state a switch leaves when it dies between writing the two -- and
    /// three separate paths reached exactly that state. The tokens
    /// themselves can: a refresh token is issued to one login, so finding it
    /// stored under another profile says whose it is.
    ///
    /// It also keeps two profiles from sharing a refresh token at all, which
    /// is dangerous on its own: refreshing one rotates the token and kills
    /// the other.
    fn assert_not_held_elsewhere(
        &self,
        name: &ProfileName,
        incoming: &crate::model::OAuthCredentials,
    ) -> crate::Result<()> {
        // A blank token is refused by the validity gate, with a better
        // message than "it matches another blank token".
        if incoming.refresh_token.is_blank() {
            return Ok(());
        }
        if let Some(other) = self
            .holders_of(&incoming.refresh_token)
            .into_iter()
            .find(|other| other != name)
        {
            return Err(CcredError::UnsafeWrite(format!(
                concat!(
                    "these credentials are the ones stored as profile '{}', not '{}'; ",
                    "check `ccred current`, and log in again if the account shown is wrong"
                ),
                other, name
            )));
        }
        Ok(())
    }

    /// The profiles whose stored refresh token is `token`.
    ///
    /// A refresh token is issued to one login, so this is the one reliable
    /// answer to "whose credentials are these" -- `.claude.json` can name a
    /// different account than the tokens beside it belong to.
    ///
    /// A profile that cannot be read is left out rather than failing the
    /// question: one damaged profile would otherwise refuse every save.
    pub fn holders_of(&self, token: &crate::redact::Secret) -> Vec<ProfileName> {
        if token.is_blank() {
            return Vec::new();
        }
        self.list()
            .unwrap_or_default()
            .into_iter()
            .filter(|name| {
                matches!(
                    self.store(name).and_then(|s| s.load()),
                    Ok(Some(theirs)) if &theirs.creds.oauth.refresh_token == token
                )
            })
            .collect()
    }

    /// Copy credentials from `source` into the named profile.
    ///
    /// `account` is the account those credentials belong to. Pass what the
    /// live configuration says, or better, what `claude auth status` reports.
    pub fn save_from(
        &self,
        name: &ProfileName,
        source: &dyn CredentialStore,
        account: &AccountSnapshot,
    ) -> crate::Result<SaveOutcome> {
        let Some(loaded) = source.load()? else {
            return Err(CcredError::UnsafeWrite(format!(
                "{} holds no credentials -- nothing to save",
                source.describe()
            )));
        };

        let existing_meta = self.meta(name)?;
        self.assert_same_account(name, existing_meta.as_ref(), &account.identity)?;

        let target = self.store(name)?;

        // Nothing to do if the content already matches. Keeps a scheduled
        // sync from touching the disk every few minutes for no reason.
        //
        // The comparison is semantic, not byte-for-byte: `store` re-serialises
        // with pretty printing, so the stored bytes never equal the source
        // bytes and a raw comparison would rewrite the profile on every run.
        // `Value` equality ignores key order, so this is a true content check.
        // `unwrap_or(None)`, not `?`: a file that will not parse is exactly
        // what a save is being asked to replace.
        if let Some(current) = target.load().unwrap_or(None)
            && let (Ok(a), Ok(b)) = (
                serde_json::to_value(&current.creds),
                serde_json::to_value(&loaded.creds),
            )
            && a == b
        {
            // Identical content, but the metadata may still be wrong: a
            // latched needs_login has to be clearable by saving credentials
            // that work, and returning early here meant it never was.
            self.update_meta(name, |m| {
                m.refresh.needs_login = false;
                m.refresh.consecutive_failures = 0;
                m.refresh.next_attempt_after_ms = None;
            })?;
            return Ok(SaveOutcome::Unchanged);
        }

        // After the unchanged case, which writes nothing: two profiles that
        // already share a token -- allowed before this check existed -- would
        // otherwise be reported broken on every run until the token rotates.
        self.assert_not_held_elsewhere(name, &loaded.creds.oauth)?;

        let outcome = if existing_meta.is_some() {
            SaveOutcome::Updated
        } else {
            SaveOutcome::Created
        };

        self.backup(name)?;

        // store() runs the validity and refresh-window gates.
        target.store(&loaded.creds)?;

        // Only advance last-known-good once the write has been accepted.
        let lkg = self.paths.profile_lkg(name)?;
        write_atomic(&lkg, &loaded.raw, true)?;

        // Keep the whole account blob, not just the projection: a later switch
        // has to put all of it back.
        if let Some(raw_account) = &account.raw {
            let bytes =
                serde_json::to_vec_pretty(raw_account).map_err(|source| CcredError::Json {
                    path: self.paths.profile_oauth_account(name).unwrap_or_default(),
                    source,
                })?;
            write_atomic(&self.paths.profile_oauth_account(name)?, &bytes, true)?;
        }

        let now = now_ms();
        let meta = match existing_meta {
            Some(mut m) => {
                m.account = account.identity.clone();
                m.last_synced_at_ms = Some(now);
                // A successful save is the human intervention the refresh loop
                // was waiting for, so the latch and the backoff both clear.
                m.refresh.needs_login = false;
                m.refresh.consecutive_failures = 0;
                m.refresh.next_attempt_after_ms = None;
                m
            }
            None => ProfileMeta::new(name, account.identity.clone(), now),
        };
        let bytes = serde_json::to_vec_pretty(&meta).map_err(|source| CcredError::Json {
            path: self.meta_path(name).unwrap_or_default(),
            source,
        })?;
        write_atomic(&self.meta_path(name)?, &bytes, true)?;

        Ok(outcome)
    }

    /// The identity gate. See the module docs for why this is not redundant.
    fn assert_same_account(
        &self,
        name: &ProfileName,
        existing: Option<&ProfileMeta>,
        incoming: &AccountIdentity,
    ) -> crate::Result<()> {
        let Some(existing) = existing else {
            return Ok(()); // a new profile can belong to anyone
        };
        // A profile with no recorded owner cannot contradict anything.
        if existing.account == AccountIdentity::default() {
            return Ok(());
        }
        match incoming.same_account_as(&existing.account) {
            Some(true) => Ok(()),
            Some(false) => Err(CcredError::AccountMismatch {
                profile: name.as_str().to_string(),
                stored: existing.account.label(),
                incoming: incoming.label(),
            }),
            // Unknown is not a match. Refusing is the safe direction: the cost
            // is an error message, the cost of guessing wrong is a lost account.
            None => Err(CcredError::AccountUnverifiable {
                profile: name.as_str().to_string(),
                stored: existing.account.label(),
            }),
        }
    }

    // --------------------------------------------------------------- recovery

    /// What a last-known-good copy holds, if it holds anything usable.
    ///
    /// The `.lkg` file is written after every accepted store, so it lags the
    /// live copy by at most one successful save. Until now nothing read it --
    /// which was fine right up to the day a spawned `claude` emptied a profile
    /// and this was the only surviving copy.
    pub fn last_known_good(&self, name: &ProfileName) -> crate::Result<Option<CredentialsFile>> {
        let path = self.paths.profile_lkg(name)?;
        let raw = match fs::read(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(CcredError::Io { path, source }),
        };
        let parsed: CredentialsFile = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            // A corrupt copy is the same as no copy. Reporting it as an error
            // would turn "you have no fallback" into "the command failed".
            Err(_) => return Ok(None),
        };
        if validate_credentials(&parsed.oauth, now_ms()).is_err() {
            return Ok(None);
        }
        Ok(Some(parsed))
    }

    /// Put the last-known-good copy back into the profile's store.
    ///
    /// Deliberately `replace` rather than `store`: the monotonic refresh-window
    /// rule exists to stop an older credential overwriting a newer one, and
    /// restoring is precisely the case where going backwards is the intent.
    pub fn restore_last_known_good(&self, name: &ProfileName) -> crate::Result<bool> {
        let Some(good) = self.last_known_good(name)? else {
            return Ok(false);
        };
        // Keep whatever is there now before overwriting it, even though it is
        // believed broken -- a wrong diagnosis should not be the end of it.
        let _ = self.backup(name);
        self.store(name)?.replace(&good)?;
        // Clear the latch, or the profile stays excluded from every future
        // refresh despite now holding working credentials. Only `save_from`
        // cleared it, and only on a write it did not skip as unchanged -- so
        // a repaired profile could never get out.
        self.update_meta(name, |m| {
            m.refresh.needs_login = false;
            m.refresh.consecutive_failures = 0;
            m.refresh.next_attempt_after_ms = None;
        })?;
        Ok(true)
    }

    /// Put a removed profile back from the newest copy `rm` kept.
    ///
    /// `rm` says where it put the copy precisely because deleting the only
    /// stored copy of an account should not be the one mistake here that
    /// cannot be undone -- and until now nothing could use it. Reading the
    /// file back by hand was the whole recovery.
    ///
    /// What comes back is the credentials and nothing else: the account
    /// details a switch restores into `.claude.json` are not in the copy, so
    /// the profile reads as an unknown account until the next save fills it
    /// in, which is the same state as a profile whose blob was never stored.
    pub fn restore_removed(&self, name: &ProfileName) -> crate::Result<Option<PathBuf>> {
        let dir = self.paths.backups_dir().join(name.as_str());
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(None);
        };
        let mut copies: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("credentials."))
                    .unwrap_or(false)
            })
            .collect();
        // Named by epoch milliseconds, which sorts the same either way until
        // the year 2286.
        copies.sort();
        let Some(newest) = copies.pop() else {
            return Ok(None);
        };

        // Parsed and judged before anything is created: a copy that will not
        // load, or that holds a logged-out blob, is not a profile.
        let raw = fs::read(&newest).map_err(|source| CcredError::Io {
            path: newest.clone(),
            source,
        })?;
        let loaded = crate::store::parse_loaded(raw, &newest)?;
        validate_credentials(&loaded.creds.oauth, now_ms())?;
        // The same guard a save meets: these tokens may have been re-saved
        // under another name since, and two profiles sharing a refresh token
        // is how refreshing one kills the other.
        self.assert_not_held_elsewhere(name, &loaded.creds.oauth)?;

        // `replace`, not `store`: there is nothing here to compare a window
        // against, and the file that was kept is by definition older.
        self.store(name)?.replace(&loaded.creds)?;
        write_atomic(&self.paths.profile_lkg(name)?, &loaded.raw, true)?;
        let meta = ProfileMeta::new(name, AccountIdentity::default(), now_ms());
        let bytes = serde_json::to_vec_pretty(&meta).map_err(|source| CcredError::Json {
            path: self.meta_path(name).unwrap_or_default(),
            source,
        })?;
        write_atomic(&self.meta_path(name)?, &bytes, true)?;
        Ok(Some(newest))
    }

    // --------------------------------------------------------------- backups

    /// Copy the profile's current credentials aside, keeping the newest few.
    ///
    /// Public because `rm` needs it: the backups live outside the profile
    /// directory, so a copy taken here survives the directory being deleted.
    /// Returns where the copy went, or `None` when there was nothing to copy.
    ///
    /// The distinction matters: a caller that is about to delete the profile
    /// has to know whether anything survives. Returning `Ok(())` for both
    /// cases let `rm` promise a backup directory it had never created.
    pub fn backup(&self, name: &ProfileName) -> crate::Result<Option<PathBuf>> {
        let store = self.store(name)?;
        let Ok(Some(loaded)) = store.load() else {
            // The live copy is unreadable, but the last-known-good one may
            // not be -- and `rm` is about to take that with it too.
            return self.backup_last_known_good(name);
        };
        self.backup_raw(name.as_str(), &loaded.raw).map(Some)
    }

    /// Copy the last-known-good file aside, for when the live one is gone.
    fn backup_last_known_good(&self, name: &ProfileName) -> crate::Result<Option<PathBuf>> {
        let path = self.paths.profile_lkg(name)?;
        match fs::read(&path) {
            Ok(raw) if !raw.is_empty() => self.backup_raw(name.as_str(), &raw).map(Some),
            _ => Ok(None),
        }
    }

    /// Copy live credentials that belong to no profile out of harm's way.
    ///
    /// A switch overwrites the live store. Normally the outgoing credentials
    /// are mirrored into their profile first, but that mirror is deliberately
    /// skipped when the live account is not the one the pointer names -- which
    /// is exactly the case where those credentials exist nowhere else. Without
    /// this they would be destroyed by the very command meant to organise
    /// them.
    ///
    /// The label cannot collide with a profile: profile names must start with
    /// a letter or digit.
    ///
    /// Each account gets its own rotation under `.orphaned/`. With one shared
    /// rotation, ten orphaned copies of one account pushed out the only copy
    /// of another -- and an orphaned copy is by definition the only one.
    pub fn backup_orphaned_live(
        &self,
        store: &dyn CredentialStore,
        account: &AccountIdentity,
    ) -> crate::Result<Option<PathBuf>> {
        let Ok(Some(loaded)) = store.load() else {
            return Ok(None); // logged out; nothing to lose
        };
        // Only what is usable is worth keeping. Preserving a logged-out blob
        // would push a real backup out of the rotation for nothing.
        if validate_credentials(&loaded.creds.oauth, now_ms()).is_err() {
            return Ok(None);
        }
        let label = format!(".orphaned/{}", orphan_key(account));
        self.backup_raw(&label, &loaded.raw).map(Some)
    }

    /// Every directory of copies this profile's credentials are in, deleted.
    ///
    /// Two of them: the rotation `rm` writes under the profile's own name,
    /// and the one keyed by account that a switch writes when the live
    /// credentials belong to nobody. Both hold the account being removed, and
    /// "removed" that leaves credentials on disk is not what the word means.
    ///
    /// The account is read before the profile goes, so this is called first.
    pub fn purge_copies(&self, name: &ProfileName) -> Vec<PathBuf> {
        let mut gone = Vec::new();
        let mut remove = |dir: PathBuf| {
            if dir.is_dir() && fs::remove_dir_all(&dir).is_ok() {
                gone.push(dir);
            }
        };

        let account = self
            .meta(name)
            .ok()
            .flatten()
            .map(|m| m.account)
            .unwrap_or_default();
        if account != AccountIdentity::default() {
            remove(
                self.paths
                    .backups_dir()
                    .join(".orphaned")
                    .join(orphan_key(&account)),
            );
        }
        remove(self.paths.backups_dir().join(name.as_str()));
        gone
    }

    fn backup_raw(&self, label: &str, raw: &[u8]) -> crate::Result<PathBuf> {
        debug_assert!(!label.split('/').any(|part| part.is_empty() || part == ".."));
        // Pushed one component at a time. Joining the whole label at once
        // leaves the `/` inside it verbatim on Windows, so the path came
        // out with a lone forward slash among the backslashes -- and it is
        // a path someone has to read and copy after a switch went sideways.
        let mut dir = self.paths.backups_dir();
        for part in label.split('/') {
            dir.push(part);
        }
        let stamp = now_ms();
        let path = dir.join(format!("credentials.{stamp}.json"));
        write_atomic(&path, raw, true)?;

        // Prune oldest. Names sort lexicographically the same as numerically
        // for as long as epoch-ms has 13 digits, i.e. until the year 2286.
        let mut backups: Vec<_> = fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().starts_with("credentials."))
                            .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        backups.sort();
        if backups.len() > BACKUPS_KEPT {
            for old in &backups[..backups.len() - BACKUPS_KEPT] {
                let _ = fs::remove_file(old);
            }
        }
        Ok(path)
    }
}

/// A directory name for an account's orphaned backups.
///
/// The account id where there is one, since an email can change hands. Only
/// characters that are safe in a path on every platform survive, and a name
/// that reduces to nothing, or to dots, becomes `unknown` -- this is joined
/// onto a path, and `..` must not be one of the outcomes.
fn orphan_key(account: &AccountIdentity) -> String {
    let raw = account
        .account_uuid
        .as_deref()
        .or(account.email.as_deref())
        .unwrap_or("");
    let key: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if key.trim_matches('.').is_empty() {
        "unknown".to_string()
    } else {
        key
    }
}

/// The pointer is damaged rather than merely absent. Never carries the file's
/// contents: they are arbitrary bytes, and a message is a place they could
/// end up being echoed.
fn damaged_pointer(path: &Path, why: &str) -> CcredError {
    CcredError::UnsafeWrite(format!(
        "the active-profile pointer at {} is damaged: {why}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_orphaned_account_gets_a_directory_that_stays_inside() {
        let id = |uuid: Option<&str>, email: Option<&str>| AccountIdentity {
            account_uuid: uuid.map(str::to_string),
            email: email.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(orphan_key(&id(Some("uuid-c"), Some("c@x.com"))), "uuid-c");
        assert_eq!(orphan_key(&id(None, Some("c@x.com"))), "c@x.com");
        assert_eq!(orphan_key(&id(None, None)), "unknown");
        for hostile in ["..", ".", "../..", "a/../b", r"C:\x", ""] {
            let key = orphan_key(&id(Some(hostile), None));
            assert!(
                !key.contains(['/', '\\', ':']) && !key.trim_matches('.').is_empty(),
                "{hostile:?} became {key:?}"
            );
        }
    }
    use crate::store::CredentialStore;
    use tempfile::TempDir;

    const CREDS_A: &str = r#"{
      "claudeAiOauth": {
        "accessToken": "sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "refreshToken": "sk-ant-ort01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "expiresAt": 4102444800000,
        "refreshTokenExpiresAt": 4102444800000,
        "subscriptionType": "max"
      },
      "organizationUuid": "org-a"
    }"#;

    /// A different account whose refresh window is LONGER -- the shape of the
    /// real near-miss, and invisible to the window check.
    const CREDS_B: &str = r#"{
      "claudeAiOauth": {
        "accessToken": "sk-ant-oat01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        "refreshToken": "sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        "expiresAt": 4102444800000,
        "refreshTokenExpiresAt": 4202444800000,
        "subscriptionType": "max"
      },
      "organizationUuid": "org-b"
    }"#;

    fn ident(email: &str, uuid: &str) -> AccountSnapshot {
        AccountSnapshot {
            identity: AccountIdentity {
                email: Some(email.to_string()),
                account_uuid: Some(uuid.to_string()),
                ..Default::default()
            },
            raw: Some(serde_json::json!({
                "emailAddress": email,
                "accountUuid": uuid,
                "profileFetchedAt": 1788000000000_i64
            })),
        }
    }

    struct Fixture {
        _home: TempDir,
        repo: ProfileRepo,
        live: FileStore,
    }

    fn fixture(live_content: &str) -> Fixture {
        let home = TempDir::new().unwrap();
        let paths = Paths::with_overrides(
            home.path().to_path_buf(),
            Some(home.path().join(".ccred")),
            Some(home.path().join(".claude")),
        );
        let live = FileStore::new(paths.claude_config_dir().to_path_buf());
        fs::create_dir_all(live.config_dir()).unwrap();
        fs::write(live.path(), live_content).unwrap();
        Fixture {
            _home: home,
            repo: ProfileRepo::new(paths),
            live,
        }
    }

    fn name(s: &str) -> ProfileName {
        validate_profile_name(s).unwrap()
    }

    #[test]
    fn saving_creates_then_updates() {
        let f = fixture(CREDS_A);
        let n = name("work");
        let a = ident("a@example.com", "uuid-a");

        assert_eq!(
            f.repo.save_from(&n, &f.live, &a).unwrap(),
            SaveOutcome::Created
        );
        assert_eq!(
            f.repo.save_from(&n, &f.live, &a).unwrap(),
            SaveOutcome::Unchanged
        );
        assert_eq!(f.repo.list().unwrap(), vec![n]);
    }

    /// The regression test for the live near-miss.
    #[test]
    fn refuses_to_store_a_different_account_into_an_existing_profile() {
        let f = fixture(CREDS_A);
        let n = name("work");
        f.repo
            .save_from(&n, &f.live, &ident("a@example.com", "uuid-a"))
            .unwrap();

        let before = fs::read(f.repo.store(&n).unwrap().path()).unwrap();

        // Now the live store holds a DIFFERENT account, with a longer window.
        fs::write(f.live.path(), CREDS_B).unwrap();
        let err = f
            .repo
            .save_from(&n, &f.live, &ident("b@example.com", "uuid-b"))
            .unwrap_err();

        assert!(matches!(err, CcredError::AccountMismatch { .. }), "{err}");

        let after = fs::read(f.repo.store(&n).unwrap().path()).unwrap();
        assert_eq!(before, after, "the profile must not have been touched");
    }

    #[test]
    fn the_window_check_alone_would_have_allowed_it() {
        // Proves the identity gate is load-bearing, not belt-and-braces: the
        // incoming credentials pass every other check we have.
        use crate::validate::assert_safe_replacement;
        let a: crate::model::CredentialsFile = serde_json::from_str(CREDS_A).unwrap();
        let b: crate::model::CredentialsFile = serde_json::from_str(CREDS_B).unwrap();
        assert!(assert_safe_replacement(Some(&a.oauth), &b.oauth, 1_788_000_000_000).is_ok());
    }

    #[test]
    fn refuses_when_the_incoming_account_is_unknown() {
        let f = fixture(CREDS_A);
        let n = name("work");
        f.repo
            .save_from(&n, &f.live, &ident("a@example.com", "uuid-a"))
            .unwrap();

        let err = f
            .repo
            .save_from(&n, &f.live, &AccountSnapshot::default())
            .unwrap_err();
        assert!(
            matches!(err, CcredError::AccountUnverifiable { .. }),
            "unknown must not be treated as a match: {err}"
        );
    }

    #[test]
    fn a_matching_account_may_update_the_profile() {
        let f = fixture(CREDS_A);
        let n = name("work");
        let a = ident("a@example.com", "uuid-a");
        f.repo.save_from(&n, &f.live, &a).unwrap();

        // Same account, refreshed credentials (window moved forward).
        let refreshed = CREDS_A.replace(
            "4102444800000,\n        \"refreshTokenExpiresAt\": 4102444800000",
            "4102444800000,\n        \"refreshTokenExpiresAt\": 4202444800000",
        );
        fs::write(f.live.path(), &refreshed).unwrap();
        assert_eq!(
            f.repo.save_from(&n, &f.live, &a).unwrap(),
            SaveOutcome::Updated
        );
    }

    #[test]
    fn an_email_change_on_the_same_account_is_accepted() {
        let f = fixture(CREDS_A);
        let n = name("work");
        f.repo
            .save_from(&n, &f.live, &ident("old@example.com", "uuid-a"))
            .unwrap();

        fs::write(f.live.path(), CREDS_B).unwrap();
        // Same account UUID, new address: the UUID wins.
        let outcome = f
            .repo
            .save_from(&n, &f.live, &ident("new@example.com", "uuid-a"))
            .unwrap();
        assert_eq!(outcome, SaveOutcome::Updated);
    }

    #[test]
    fn a_repeated_save_does_not_touch_the_disk() {
        // A scheduled sync runs often; it must not rewrite the profile every
        // time just because the serialiser reformats.
        let f = fixture(CREDS_A);
        let n = name("work");
        let a = ident("a@example.com", "uuid-a");
        f.repo.save_from(&n, &f.live, &a).unwrap();

        let store = f.repo.store(&n).unwrap();
        let before = store.revision().unwrap();
        assert_eq!(
            f.repo.save_from(&n, &f.live, &a).unwrap(),
            SaveOutcome::Unchanged
        );
        assert_eq!(
            before,
            store.revision().unwrap(),
            "the profile was rewritten"
        );
    }

    #[test]
    fn last_known_good_is_written_alongside() {
        let f = fixture(CREDS_A);
        let n = name("work");
        f.repo
            .save_from(&n, &f.live, &ident("a@example.com", "uuid-a"))
            .unwrap();
        let lkg = f.repo.paths().profile_lkg(&n).unwrap();
        assert_eq!(fs::read(&lkg).unwrap(), CREDS_A.as_bytes());
    }

    #[test]
    fn the_whole_account_blob_is_stored_for_later_restore() {
        let f = fixture(CREDS_A);
        let n = name("work");
        f.repo
            .save_from(&n, &f.live, &ident("a@example.com", "uuid-a"))
            .unwrap();

        let stored = f.repo.oauth_account(&n).unwrap().unwrap();
        // A field outside the projection must still be there.
        assert_eq!(
            stored["profileFetchedAt"],
            serde_json::json!(1788000000000_i64)
        );
    }

    #[test]
    fn active_pointer_roundtrips_and_is_validated() {
        let f = fixture(CREDS_A);
        assert!(f.repo.active().unwrap().is_none());

        let n = name("work");
        f.repo.set_active(&n).unwrap();
        assert_eq!(f.repo.active().unwrap(), Some(n));

        // A hand-mangled pointer must not become a path.
        fs::write(f.repo.paths().active_pointer(), "../../etc").unwrap();
        assert!(f.repo.active().is_err());
    }

    #[test]
    fn listing_ignores_directories_we_did_not_create() {
        let f = fixture(CREDS_A);
        let n = name("work");
        f.repo
            .save_from(&n, &f.live, &ident("a@example.com", "uuid-a"))
            .unwrap();
        fs::create_dir_all(f.repo.paths().profiles_dir().join("stray")).unwrap();

        assert_eq!(f.repo.list().unwrap(), vec![n], "no ccred.json, no profile");
    }

    #[test]
    fn saving_from_an_empty_store_is_refused() {
        let f = fixture(CREDS_A);
        fs::remove_file(f.live.path()).unwrap();
        let err = f
            .repo
            .save_from(&name("work"), &f.live, &ident("a@example.com", "uuid-a"))
            .unwrap_err();
        assert!(matches!(err, CcredError::UnsafeWrite(_)), "{err}");
    }
}
