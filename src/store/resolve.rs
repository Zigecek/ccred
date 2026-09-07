//! Choosing a backend, and deriving the keys Claude Code uses to find it.
//!
//! # Why the service name matters
//!
//! On macOS and (behind a feature flag) Windows, Claude Code addresses its
//! credential item by a *service name* derived from the config directory:
//!
//! ```text
//! service = "Claude Code" + OAUTH_FILE_SUFFIX + "-credentials" + tail
//! tail    = ""  when the store is the default one
//!         = "-" + sha256(dir).hex[0..8]  otherwise
//! ```
//!
//! `OAUTH_FILE_SUFFIX` is `""` in production (`-staging-oauth`,
//! `-local-oauth`, `-custom-oauth` exist for other environments).
//!
//! This is what makes per-profile isolation cheap: point
//! `CLAUDE_SECURESTORAGE_CONFIG_DIR` at a profile directory and that profile
//! automatically gets its own Keychain item, with no Keychain writes from us
//! at all.
//!
//! # Why we always set the variable explicitly
//!
//! When the variable is unset, the hash input is whatever Claude Code's own
//! path resolution produces, and we would have to reproduce that byte for byte
//! -- trailing slash, symlink resolution and Unicode normalisation included.
//! By always passing an explicit, canonical string we hash exactly what the
//! other side hashes, and the ambiguity disappears.

use std::path::{Path, PathBuf};

use super::CredentialStore;
use super::file::FileStore;

/// Which store a config directory refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageScope {
    /// The default store (`~/.claude`), with no environment override.
    Default,
    /// An explicit store directory, exactly as it will be passed in the
    /// environment. This exact string is the hash input.
    Custom(String),
}

/// The account name of the Keychain / Credential Manager item.
///
/// Verified in the shipped binaries: macOS and Linux use `$USER` with a
/// fallback, while **Windows hard-codes the constant** -- the platforms really
/// do differ, so do not unify them.
pub fn keychain_account_name() -> String {
    const FALLBACK: &str = "claude-code-user";

    if cfg!(target_os = "windows") {
        return FALLBACK.to_string();
    }
    let user = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_default();

    let acceptable = !user.is_empty()
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if acceptable {
        user
    } else {
        FALLBACK.to_string()
    }
}

/// Derive the Keychain service name for a scope.
///
/// CAVEAT: the hash input should be NFC-normalised. For ASCII paths that is a
/// no-op, and profile names are restricted to ASCII, so the only way to hit
/// this is a home directory containing non-ASCII characters. Fixing it means
/// pulling in Unicode normalisation; until macOS support is verified on real
/// hardware this is a documented limitation rather than a silent one.
pub fn keychain_service_name(scope: &StorageScope) -> String {
    const BASE: &str = "Claude Code";
    const OAUTH_FILE_SUFFIX: &str = ""; // production
    const CREDENTIALS: &str = "-credentials";

    match scope {
        StorageScope::Default => format!("{BASE}{OAUTH_FILE_SUFFIX}{CREDENTIALS}"),
        StorageScope::Custom(dir) => {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(dir.as_bytes());
            let hex = hex::encode(digest);
            format!("{BASE}{OAUTH_FILE_SUFFIX}{CREDENTIALS}-{}", &hex[..8])
        }
    }
}

/// Environment to hand a spawned `claude` so it uses a specific store.
///
/// Both variables are set on purpose. `CLAUDE_SECURESTORAGE_CONFIG_DIR` moves
/// only the credential store, leaving `.claude.json`, `projects/` and MCP
/// config shared -- which is what we want, since accounts should be separate
/// but project history should not be. Setting `CLAUDE_CONFIG_DIR` too removes
/// any doubt about which value feeds the service-name hash.
pub fn env_pairs_for(scope: &StorageScope) -> Vec<(&'static str, String)> {
    match scope {
        StorageScope::Default => Vec::new(),
        StorageScope::Custom(dir) => vec![
            ("CLAUDE_SECURESTORAGE_CONFIG_DIR", dir.clone()),
            ("CLAUDE_CONFIG_DIR", dir.clone()),
        ],
    }
}

/// Variables that would make a spawned `claude` authenticate as something else
/// -- and therefore NOT refresh the OAuth token we care about.
///
/// `CLAUDE_CODE_OAUTH_TOKEN` is the dangerous one: it triggers a plaintext
/// write, which on macOS deletes the Keychain item for every session.
pub const SCRUBBED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
];

/// Pick the backend for a config directory.
///
/// macOS is deliberately not wired to the Keychain yet: that code cannot be
/// verified without real hardware, and shipping unverified credential handling
/// is worse than shipping none. The file backend is a genuine location on
/// macOS too -- the composite store falls back to it whenever a Keychain write
/// fails -- so this is degraded, not wrong.
pub fn store_for(config_dir: PathBuf) -> Box<dyn CredentialStore> {
    Box::new(FileStore::new(config_dir))
}

/// Canonical string form of a path, for use as the hash input and in the
/// environment. Kept in one place so both always agree.
pub fn scope_for(config_dir: &Path, is_default: bool) -> StorageScope {
    if is_default {
        StorageScope::Default
    } else {
        StorageScope::Custom(config_dir.to_string_lossy().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scope_has_no_hash_suffix() {
        assert_eq!(
            keychain_service_name(&StorageScope::Default),
            "Claude Code-credentials"
        );
    }

    #[test]
    fn custom_scope_appends_eight_hex_characters() {
        let name = keychain_service_name(&StorageScope::Custom("/home/user/.ccred".into()));
        let suffix = name.strip_prefix("Claude Code-credentials-").unwrap();
        assert_eq!(suffix.len(), 8, "expected 8 hex chars, got {suffix:?}");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn service_name_matches_a_hand_computed_sha256() {
        // Pins the derivation against the algorithm read out of the binary:
        // first 8 hex characters of sha256 over the directory string.
        use sha2::{Digest, Sha256};
        let dir = "/Users/example/.ccred/profiles/work";
        let expected_hex = hex::encode(Sha256::digest(dir.as_bytes()));
        let expected = format!("Claude Code-credentials-{}", &expected_hex[..8]);
        assert_eq!(
            keychain_service_name(&StorageScope::Custom(dir.into())),
            expected
        );
    }

    #[test]
    fn different_directories_get_different_items() {
        let a = keychain_service_name(&StorageScope::Custom("/a".into()));
        let b = keychain_service_name(&StorageScope::Custom("/b".into()));
        assert_ne!(a, b, "profiles must not share one Keychain item");
    }

    #[test]
    fn trailing_slash_changes_the_hash() {
        // Documents why we always pass an explicit canonical string: the hash
        // is over the raw text, so "/a" and "/a/" are different items.
        assert_ne!(
            keychain_service_name(&StorageScope::Custom("/a".into())),
            keychain_service_name(&StorageScope::Custom("/a/".into()))
        );
    }

    #[test]
    fn account_name_is_acceptable_or_the_constant() {
        let name = keychain_account_name();
        assert!(!name.is_empty());
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'),
            "account name must satisfy Claude Code's own pattern: {name:?}"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_uses_the_hard_coded_account() {
        assert_eq!(keychain_account_name(), "claude-code-user");
    }

    #[test]
    fn default_scope_sets_no_environment() {
        assert!(env_pairs_for(&StorageScope::Default).is_empty());
    }

    #[test]
    fn custom_scope_sets_both_variables_to_the_same_string() {
        let pairs = env_pairs_for(&StorageScope::Custom("/x/y".into()));
        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().all(|(_, v)| v == "/x/y"));
        let keys: Vec<_> = pairs.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"CLAUDE_SECURESTORAGE_CONFIG_DIR"));
        assert!(keys.contains(&"CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn the_dangerous_variable_is_scrubbed() {
        // Setting this in a child's environment deletes the macOS Keychain
        // item as a side effect. It must always be removed.
        assert!(SCRUBBED_ENV.contains(&"CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(SCRUBBED_ENV.contains(&"ANTHROPIC_API_KEY"));
    }
}
