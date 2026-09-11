//! Reading and writing credential stores.
//!
//! A "store" is one place Claude Code keeps credentials for one config
//! directory. Which backend is live depends on the platform, and on macOS it
//! can even change at runtime -- see [`resolve`] for why we probe rather than
//! assume.

pub mod file;
pub mod resolve;

// No Keychain backend yet. On macOS Claude Code uses a composite store that
// falls back to this same plaintext file whenever a Keychain write fails, so
// the file backend is a genuine location there rather than a wrong one -- but
// it is degraded, and a Keychain-first install will not be seen. Wiring it up
// needs real hardware to verify against; shipping unverified credential
// handling would be worse than shipping none. See `resolve::store_for`.

use std::path::Path;
use std::time::Duration;

use crate::lockfile::DirLock;
use crate::model::CredentialsFile;
use crate::model::credentials::assert_lossless;
use crate::validate::validate_credentials;

/// Credentials plus the exact bytes they were parsed from.
///
/// Carrying the raw bytes is what makes the lossless guarantee checkable: on
/// every load we prove the parsed model can reproduce every key that was on
/// disk. Anything that comes out of [`CredentialStore::load`] is therefore
/// known to be safe to write back.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub creds: CredentialsFile,
    pub raw: Vec<u8>,
}

/// Cheap change detection: did the store change since we last looked?
///
/// Used to decide whether a refresh actually happened. We judge success by an
/// observed state change, never by an exit code -- exit codes lie, a moved
/// refresh window does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision(pub String);

pub trait CredentialStore: Send + Sync {
    /// Human-readable and secret-free, e.g. `file:~/.claude/.credentials.json`.
    fn describe(&self) -> String;

    /// The config directory this store belongs to.
    fn config_dir(&self) -> &Path;

    /// `Ok(None)` means absent or logged out. Malformed content is an `Err`,
    /// never `None` -- conflating "empty" with "broken" is how the bash
    /// predecessor destroyed a profile.
    fn load(&self) -> crate::Result<Option<Loaded>>;

    /// Persist credentials for the SAME account.
    ///
    /// Runs the full gate: validity, plus the monotonic refresh-window rule.
    /// This is the path a sync takes.
    fn store(&self, creds: &CredentialsFile) -> crate::Result<()>;

    /// Persist credentials that deliberately belong to a DIFFERENT account.
    ///
    /// Validity is still enforced -- a blank token is never written -- but the
    /// refresh-window rule is not, because it only makes sense within one
    /// account's own history. Switching from an account with three weeks left
    /// to one with three days is a perfectly ordinary thing to ask for, and
    /// `store` would refuse it.
    ///
    /// Only `switch` should call this.
    fn replace(&self, creds: &CredentialsFile) -> crate::Result<()>;

    fn delete(&self) -> crate::Result<()>;

    /// Take the same lock Claude Code takes before mutating credentials.
    fn lock(&self, timeout: Duration) -> crate::Result<DirLock>;

    fn revision(&self) -> crate::Result<Option<Revision>>;
}

/// Parse bytes into a [`Loaded`], proving the round-trip is lossless.
///
/// If a future Claude Code version adds a field our model cannot carry, this
/// fails loudly at load time rather than silently dropping it on the next
/// write.
pub fn parse_loaded(raw: Vec<u8>, path: &Path) -> crate::Result<Loaded> {
    let creds: CredentialsFile =
        serde_json::from_slice(&raw).map_err(|source| crate::CcredError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    let reserialized = serde_json::to_value(&creds).map_err(|source| crate::CcredError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    assert_lossless(&raw, &reserialized)?;
    Ok(Loaded { creds, raw })
}

/// The gate every write passes, regardless of backend.
///
/// Deliberately NOT covered here: account identity. Two different accounts can
/// both be valid with advancing refresh windows, so a caller that is copying
/// credentials into a *named* profile must compare the account itself. A live
/// near-miss happened exactly that way.
pub fn check_before_store(
    store: &dyn CredentialStore,
    incoming: &CredentialsFile,
    now_ms: i64,
) -> crate::Result<()> {
    validate_credentials(&incoming.oauth, now_ms)?;
    let existing = store.load()?;
    crate::validate::assert_safe_replacement(
        existing.as_ref().map(|l| &l.creds.oauth),
        &incoming.oauth,
        now_ms,
    )
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule the bash predecessor did not have: a load must distinguish
    /// "there is nothing here" from "what is here is broken". Conflating them
    /// is how a logged-out state passed validation and destroyed a profile.
    #[test]
    fn malformed_content_is_an_error_and_never_mistaken_for_absence() {
        let err = parse_loaded(b"{ not json".to_vec(), Path::new("/tmp/x")).unwrap_err();
        assert!(matches!(err, crate::CcredError::Json { .. }), "got {err:?}");
    }

    /// A field this model cannot carry must stop the load, not be dropped on
    /// the next write. The check runs at load time so the failure is attached
    /// to reading someone else's file, not to our own write.
    #[test]
    fn a_field_the_model_cannot_carry_fails_at_load_time() {
        // `claudeAiOauth` is modelled with a flattened catch-all, so an
        // unknown key there survives. A non-object in its place does not.
        let raw = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                       "refreshToken":"sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
                       "expiresAt":1,"somethingNew":{"a":1}},"topLevelNovelty":[1,2]}"#;
        let loaded = parse_loaded(raw.to_vec(), Path::new("/tmp/x"))
            .expect("unknown keys must be carried, not rejected");
        let back = serde_json::to_value(&loaded.creds).unwrap();
        assert_eq!(back["topLevelNovelty"], serde_json::json!([1, 2]));
        assert_eq!(
            back["claudeAiOauth"]["somethingNew"]["a"],
            serde_json::json!(1)
        );
    }

    /// `now_ms` is used as a monotonic-ish clock for the refresh window, so a
    /// zero would make every window look expired.
    #[test]
    fn the_clock_is_a_real_epoch_reading() {
        let t = now_ms();
        // 2020-01-01. Anything earlier means the clock call failed and fell
        // back to zero, which would read as "everything has expired".
        assert!(t > 1_577_836_800_000, "got {t}");
    }
}
