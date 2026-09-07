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
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::atomic::write_atomic;
use crate::error::CcredError;
use crate::model::credentials::JsonMap;
use crate::model::{AccountIdentity, AccountSnapshot};
use crate::paths::Paths;
use crate::store::file::FileStore;
use crate::store::{CredentialStore, now_ms};
use crate::validate::{ProfileName, validate_profile_name};

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
                Ok(Some(validate_profile_name(trimmed)?))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CcredError::Io { path, source }),
        }
    }

    pub fn set_active(&self, name: &ProfileName) -> crate::Result<()> {
        write_atomic(&self.paths.active_pointer(), name.as_str().as_bytes(), true)
    }

    // ------------------------------------------------------------------ save

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
        if let Some(current) = target.load()?
            && let (Ok(a), Ok(b)) = (
                serde_json::to_value(&current.creds),
                serde_json::to_value(&loaded.creds),
            )
            && a == b
        {
            return Ok(SaveOutcome::Unchanged);
        }

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

    // --------------------------------------------------------------- backups

    /// Copy the profile's current credentials aside, keeping the newest few.
    fn backup(&self, name: &ProfileName) -> crate::Result<()> {
        let store = self.store(name)?;
        let Ok(Some(loaded)) = store.load() else {
            return Ok(()); // nothing worth keeping
        };

        let dir = self.paths.backups_dir().join(name.as_str());
        let stamp = now_ms();
        write_atomic(
            &dir.join(format!("credentials.{stamp}.json")),
            &loaded.raw,
            true,
        )?;

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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
