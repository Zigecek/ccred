//! The plaintext `.credentials.json` backend.
//!
//! This is the only backend on Linux, the current default on Windows, and the
//! fallback on macOS. It is never merely legacy: on macOS the composite store
//! writes here whenever a Keychain write fails, so it has to be treated as a
//! live, authoritative location.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{CredentialStore, Loaded, Revision, check_before_store, now_ms, parse_loaded};
use crate::atomic::write_atomic;
use crate::error::CcredError;
use crate::lockfile::{self, DirLock};
use crate::model::CredentialsFile;
use crate::paths::{credentials_in, storage_write_lock_target};

#[derive(Debug, Clone)]
pub struct FileStore {
    config_dir: PathBuf,
}

impl FileStore {
    pub fn new(config_dir: PathBuf) -> Self {
        FileStore { config_dir }
    }

    pub fn path(&self) -> PathBuf {
        credentials_in(&self.config_dir)
    }
}

impl CredentialStore for FileStore {
    fn describe(&self) -> String {
        format!("file:{}", self.path().display())
    }

    fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    fn load(&self) -> crate::Result<Option<Loaded>> {
        let path = self.path();
        match fs::read(&path) {
            Ok(raw) => {
                // An empty file is "absent", not "broken": Claude Code can
                // leave a zero-length file behind mid-write.
                if raw.iter().all(|b| b.is_ascii_whitespace()) {
                    return Ok(None);
                }
                Ok(Some(parse_loaded(raw, &path)?))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            // A permission error is NOT "absent" -- reporting it as logged out
            // would invite a caller to overwrite a perfectly good file.
            Err(e) => Err(CcredError::Io { path, source: e }),
        }
    }

    fn store(&self, creds: &CredentialsFile) -> crate::Result<()> {
        check_before_store(self, creds, now_ms())?;
        self.write_value(creds)
    }

    fn replace(&self, creds: &CredentialsFile) -> crate::Result<()> {
        // Validity only: see the trait docs for why the window rule is skipped.
        crate::validate::validate_credentials(&creds.oauth, now_ms())?;
        self.write_value(creds)
    }

    fn delete(&self) -> crate::Result<()> {
        let path = self.path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CcredError::Io { path, source: e }),
        }
    }

    fn lock(&self, timeout: Duration) -> crate::Result<DirLock> {
        lockfile::acquire(&storage_write_lock_target(&self.config_dir), timeout)
    }

    fn revision(&self) -> crate::Result<Option<Revision>> {
        let path = self.path();
        match fs::metadata(&path) {
            Ok(meta) => {
                let nanos = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                Ok(Some(Revision(format!("{nanos}:{}", meta.len()))))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CcredError::Io { path, source: e }),
        }
    }
}

impl FileStore {
    fn write_value(&self, creds: &CredentialsFile) -> crate::Result<()> {
        let value = serde_json::to_value(creds).map_err(|source| CcredError::Json {
            path: self.path(),
            source,
        })?;
        let bytes = serde_json::to_vec_pretty(&value).map_err(|source| CcredError::Json {
            path: self.path(),
            source,
        })?;
        write_atomic(&self.path(), &bytes, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::Secret;
    use tempfile::tempdir;

    const REAL_SHAPE: &str = r#"{
      "claudeAiOauth": {
        "accessToken": "sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "refreshToken": "sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        "expiresAt": 4102444800000,
        "refreshTokenExpiresAt": 4102444800000,
        "scopes": ["user:inference"],
        "subscriptionType": "max",
        "rateLimitTier": "default_claude_max_5x"
      },
      "organizationUuid": "b5c1c992-726a-4dab-9d8e-d225ba8ee6d4"
    }"#;

    fn store_with(content: Option<&str>) -> (tempfile::TempDir, FileStore) {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());
        if let Some(c) = content {
            fs::write(store.path(), c).unwrap();
        }
        (dir, store)
    }

    #[test]
    fn missing_file_is_absent_not_an_error() {
        let (_d, store) = store_with(None);
        assert!(store.load().unwrap().is_none());
        assert!(store.revision().unwrap().is_none());
    }

    #[test]
    fn an_empty_file_is_absent() {
        let (_d, store) = store_with(Some("   \n  "));
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn malformed_json_is_an_error_not_absent() {
        // This distinction matters: "absent" invites an overwrite.
        let (_d, store) = store_with(Some("{ not json"));
        assert!(matches!(store.load().unwrap_err(), CcredError::Json { .. }));
    }

    #[test]
    fn loads_and_keeps_the_raw_bytes() {
        let (_d, store) = store_with(Some(REAL_SHAPE));
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.raw, REAL_SHAPE.as_bytes());
        assert!(loaded.creds.extra.contains_key("organizationUuid"));
    }

    #[test]
    fn store_then_load_preserves_unknown_keys() {
        let (_d, store) = store_with(Some(REAL_SHAPE));
        let loaded = store.load().unwrap().unwrap();
        store.store(&loaded.creds).unwrap();

        let again = store.load().unwrap().unwrap();
        assert!(again.creds.extra.contains_key("organizationUuid"));
        assert!(again.creds.oauth.extra.contains_key("rateLimitTier"));
    }

    #[test]
    fn refuses_to_store_a_blank_token() {
        let (_d, store) = store_with(Some(REAL_SHAPE));
        let mut loaded = store.load().unwrap().unwrap();
        loaded.creds.oauth.refresh_token = Secret::new("");

        let err = store.store(&loaded.creds).unwrap_err();
        assert!(matches!(err, CcredError::InvalidCredentials(_)), "{err}");

        // The good file must still be there, byte for byte.
        assert_eq!(fs::read(store.path()).unwrap(), REAL_SHAPE.as_bytes());
    }

    #[test]
    fn refuses_a_write_that_regresses_the_refresh_window() {
        let (_d, store) = store_with(Some(REAL_SHAPE));
        let mut loaded = store.load().unwrap().unwrap();
        loaded.creds.oauth.refresh_token_expires_at = Some(4_102_444_800_000 - 86_400_000);
        loaded.creds.oauth.refresh_token = Secret::new(format!("sk-ant-ort01-{}", "C".repeat(90)));

        let err = store.store(&loaded.creds).unwrap_err();
        assert!(matches!(err, CcredError::UnsafeWrite(_)), "{err}");
    }

    #[test]
    fn replace_allows_a_shorter_window_that_store_refuses() {
        // Switching to an account with less time left is ordinary; the
        // monotonic rule only makes sense inside one account's history.
        let (_d, store) = store_with(Some(REAL_SHAPE));
        let mut loaded = store.load().unwrap().unwrap();
        loaded.creds.oauth.refresh_token_expires_at = Some(4_102_444_800_000 - 86_400_000);
        loaded.creds.oauth.refresh_token = Secret::new(format!("sk-ant-ort01-{}", "C".repeat(90)));

        assert!(store.store(&loaded.creds).is_err(), "store must refuse");
        assert!(store.replace(&loaded.creds).is_ok(), "replace must allow");
    }

    #[test]
    fn replace_still_refuses_a_blank_token() {
        let (_d, store) = store_with(Some(REAL_SHAPE));
        let mut loaded = store.load().unwrap().unwrap();
        loaded.creds.oauth.access_token = Secret::new("");
        assert!(store.replace(&loaded.creds).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), REAL_SHAPE.as_bytes());
    }

    #[test]
    fn revision_changes_after_a_write() {
        let (_d, store) = store_with(Some(REAL_SHAPE));
        let before = store.revision().unwrap();
        let loaded = store.load().unwrap().unwrap();
        // to_vec_pretty reformats, so length alone already differs.
        store.store(&loaded.creds).unwrap();
        let after = store.revision().unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn delete_is_idempotent() {
        let (_d, store) = store_with(Some(REAL_SHAPE));
        store.delete().unwrap();
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn lock_target_is_the_one_claude_code_uses() {
        let (_d, store) = store_with(None);
        let lock = store.lock(Duration::from_millis(500)).unwrap();
        let expected = store.config_dir().join(".storage-write.lock");
        assert_eq!(lock.path(), expected);
    }

    #[test]
    fn describe_leaks_no_secret() {
        let (_d, store) = store_with(Some(REAL_SHAPE));
        let text = store.describe();
        assert!(!text.contains("sk-ant"));
    }
}
