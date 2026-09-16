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

/// How long a reader outside the lock waits before reading a file again.
const SETTLE: Duration = Duration::from_millis(250);

/// Load, for a caller that does not hold the store's lock.
///
/// Claude Code writes under its lock, and a reader outside it can land
/// between the truncate and the write and see a file that does not parse.
/// Reporting that as damage sends a person to `ccred restore` over a file that
/// is whole again a moment later. So a file that does not parse is read once
/// more after a short pause, and only a second failure is reported.
///
/// For the read-only commands. Anything that writes takes the lock instead.
pub fn load_unlocked(store: &dyn CredentialStore) -> crate::Result<Option<Loaded>> {
    match store.load() {
        Err(crate::CcredError::Json { .. } | crate::CcredError::LossyRewrite { .. }) => {
            std::thread::sleep(SETTLE);
            store.load()
        }
        other => other,
    }
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
    // Dropped before anything looks at the bytes, including the lossless
    // check, which re-parses them.
    let raw = without_bom(raw);
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

/// A document without its byte-order mark, if it had one.
///
/// JSON has no BOM: a parser is entitled to refuse one, and serde_json does.
/// Windows hands them out anyway -- Notepad writes one into every UTF-8 file
/// it saves, and so does PowerShell 5.1 redirection -- so someone who opens
/// their credential file to look at it can leave ccred reporting "malformed
/// JSON" about a file that reads perfectly well. RFC 8259 allows ignoring it,
/// which is what every JSON implementation that meets real files does.
///
/// Never written back: the mark is not part of the document, so a file that
/// arrives with one leaves without it.
pub fn without_bom(mut raw: Vec<u8>) -> Vec<u8> {
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        raw.drain(..3);
    }
    raw
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
    // A store that cannot be parsed is treated as absent, not as a reason to
    // refuse: `save` is how a profile whose file was damaged gets repaired,
    // and the caller has already copied the old bytes aside.
    let existing = store.load().unwrap_or(None);
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

    /// Notepad writes one into every UTF-8 file it saves, and so does
    /// PowerShell 5.1 redirection. JSON has no place for it, so it goes --
    /// and only the whole mark, only at the start.
    #[test]
    fn a_byte_order_mark_is_not_part_of_the_document() {
        const BOM: &[u8] = b"\xef\xbb\xbf";

        let mut marked = BOM.to_vec();
        marked.extend_from_slice(b"{}");
        assert_eq!(without_bom(marked), b"{}".to_vec());

        assert_eq!(without_bom(b"{}".to_vec()), b"{}".to_vec());
        assert!(without_bom(Vec::new()).is_empty());

        // Not a mark: two of its three bytes, and a whole one in the middle.
        assert_eq!(without_bom(b"\xef\xbb".to_vec()), b"\xef\xbb".to_vec());
        let mut inside = b"{".to_vec();
        inside.extend_from_slice(BOM);
        inside.push(b'}');
        assert_eq!(without_bom(inside.clone()), inside);
    }

    /// A credential file someone opened in an editor and saved again still
    /// loads, and the copy that goes back out has no mark in it.
    #[test]
    fn a_marked_credential_file_still_loads() {
        let mut raw = b"\xef\xbb\xbf".to_vec();
        raw.extend_from_slice(
            br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                 "refreshToken":"sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
                 "expiresAt":1}}"#,
        );
        let loaded = parse_loaded(raw, Path::new("/tmp/x")).expect("a mark is not a malformation");
        assert!(!loaded.raw.starts_with(b"\xef\xbb\xbf"));
    }

    /// Claude Code adds keys over time, and a rewrite that dropped one would
    /// delete state that belongs to it. Both levels keep a flattened
    /// catch-all, so an unknown key is carried through a load and back out.
    #[test]
    fn unknown_fields_are_carried_through_a_load() {
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

    /// A known field of the wrong type stops the load. Coercing it, or
    /// skipping it, would mean writing back something other than what was
    /// read -- and this is someone else's file.
    #[test]
    fn a_known_field_of_the_wrong_type_fails_at_load_time() {
        let raw = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                       "refreshToken":"sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
                       "expiresAt":"soon"}}"#;
        let err = parse_loaded(raw.to_vec(), Path::new("/tmp/x")).unwrap_err();
        assert!(matches!(err, crate::CcredError::Json { .. }), "got {err:?}");
    }

    /// A store whose file is half-written when first read, and whole by the
    /// time anyone looks again -- the writer finishing in between. No
    /// threads and no sleeps, so the test cannot pass or fail by timing.
    struct HalfWritten {
        inner: file::FileStore,
        path: std::path::PathBuf,
        whole: Vec<u8>,
        loads: std::sync::atomic::AtomicU32,
    }

    impl CredentialStore for HalfWritten {
        fn describe(&self) -> String {
            self.inner.describe()
        }
        fn config_dir(&self) -> &Path {
            self.inner.config_dir()
        }
        fn load(&self) -> crate::Result<Option<Loaded>> {
            let result = self.inner.load();
            if self.loads.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                std::fs::write(&self.path, &self.whole).unwrap();
            }
            self.loads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            result
        }
        fn store(&self, creds: &CredentialsFile) -> crate::Result<()> {
            self.inner.store(creds)
        }
        fn replace(&self, creds: &CredentialsFile) -> crate::Result<()> {
            self.inner.replace(creds)
        }
        fn delete(&self) -> crate::Result<()> {
            self.inner.delete()
        }
        fn lock(&self, timeout: Duration) -> crate::Result<DirLock> {
            self.inner.lock(timeout)
        }
        fn revision(&self) -> crate::Result<Option<Revision>> {
            self.inner.revision()
        }
    }

    /// A file caught mid-write is read again rather than reported as damaged.
    #[test]
    fn a_file_caught_mid_write_is_read_again() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, br#"{"claudeAiOauth":{"accessTo"#).unwrap();
        let store = HalfWritten {
            inner: file::FileStore::new(dir.path().to_path_buf()),
            path,
            whole: br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","refreshToken":"sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB","expiresAt":1}}"#.to_vec(),
            loads: std::sync::atomic::AtomicU32::new(0),
        };

        let loaded = load_unlocked(&store);
        assert!(matches!(loaded, Ok(Some(_))), "{loaded:?}");
        assert_eq!(
            store.loads.into_inner(),
            2,
            "the first read must have failed"
        );
    }

    #[test]
    fn a_file_that_stays_broken_is_still_reported() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = file::FileStore::new(dir.path().to_path_buf());
        std::fs::write(dir.path().join(".credentials.json"), b"{ not json").unwrap();
        assert!(matches!(
            load_unlocked(&store),
            Err(crate::CcredError::Json { .. })
        ));
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
