//! Safety invariants as executable code.
//!
//! This module is a direct answer to an incident where the bash predecessor
//! stored a logged-out state over the last good copy of a profile. There were
//! two bugs, and each has its counterpart here:
//!
//! 1. validation accepted an empty string -> [`validate_token`]
//! 2. an invalid state overwrote a valid one -> [`assert_safe_replacement`]

use crate::error::{CcredError, Result, TokenInvalid};
use crate::model::OAuthCredentials;
use crate::redact::Secret;

/// Observed tokens are 108 characters. 40 is a generous floor that still
/// reliably catches `""`, `"null"`, `"undefined"` and truncated values.
const MIN_TOKEN_LEN: usize = 40;

/// Check that a value looks like a Claude token.
///
/// The shape is checked by hand rather than with a regex -- it is a handful of
/// lines and saves a dependency in a tool that holds credentials.
///
/// The prefix rule is deliberately generic (`sk-ant-<part>-<part>`) rather than
/// hard-coding `oat01`/`ort01`. A hard-coded version would break this tool the
/// day Anthropic rotates the token version, while the generic shape still
/// rejects everything that matters here.
pub fn validate_token(field: &'static str, value: Option<&Secret>) -> Result<()> {
    let secret = value.ok_or(TokenInvalid::Missing { field })?;
    let raw = secret.expose();

    // Blankness is checked FIRST and explicitly. This is the line the bash
    // version was missing.
    if secret.is_blank() {
        return Err(TokenInvalid::Blank { field }.into());
    }
    if raw.len() != raw.trim().len() || raw.chars().any(char::is_control) {
        return Err(TokenInvalid::Malformed { field }.into());
    }
    if raw.len() < MIN_TOKEN_LEN {
        return Err(TokenInvalid::TooShort {
            field,
            len: raw.len(),
        }
        .into());
    }
    if !has_token_shape(raw) {
        return Err(TokenInvalid::BadShape { field }.into());
    }
    Ok(())
}

/// `sk-ant-<version>-<body>`, where version is alphanumeric and body is at
/// least 20 characters from `[A-Za-z0-9_-]`.
fn has_token_shape(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix("sk-ant-") else {
        return false;
    };
    let Some((version, body)) = rest.split_once('-') else {
        return false;
    };
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    body.len() >= 20
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// What we know about the tokens relative to the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    pub access_expired: bool,
    pub refresh_expired: bool,
    pub refresh_window_left_ms: Option<i64>,
}

/// Validate both tokens and report how they stand against the clock.
pub fn validate_credentials(creds: &OAuthCredentials, now_ms: i64) -> Result<Health> {
    validate_token("accessToken", Some(&creds.access_token))?;
    validate_token("refreshToken", Some(&creds.refresh_token))?;
    Ok(Health {
        access_expired: creds.expires_at <= now_ms,
        refresh_expired: creds.refresh_token_expires_at.is_some_and(|t| t <= now_ms),
        refresh_window_left_ms: creds.refresh_token_expires_at.map(|t| t - now_ms),
    })
}

/// The gate **every** credential write must pass.
///
/// The refresh-token window acts as a monotonic clock: a successful refresh
/// always slides it weeks forward and never backward. That makes "newer"
/// decidable without trusting the local system clock.
///
/// Note what this deliberately does NOT cover: account identity. Two different
/// accounts can both be valid and both have advancing windows, so callers must
/// check that the incoming account matches the profile separately.
pub fn assert_safe_replacement(
    existing: Option<&OAuthCredentials>,
    incoming: &OAuthCredentials,
    now_ms: i64,
) -> Result<()> {
    // 1. The incoming value must stand on its own.
    validate_credentials(incoming, now_ms)?;

    let Some(old) = existing else {
        return Ok(()); // first write
    };
    // Replacing junk with something valid is fine -- that is how a broken
    // profile gets repaired.
    if validate_credentials(old, now_ms).is_err() {
        return Ok(());
    }

    // 2. The refresh window must not shrink.
    if let (Some(old_exp), Some(new_exp)) = (
        old.refresh_token_expires_at,
        incoming.refresh_token_expires_at,
    ) && new_exp < old_exp
    {
        let hours = (old_exp - new_exp) / 3_600_000;
        return Err(CcredError::UnsafeWrite(format!(
            "refresh window would regress by {hours}h"
        )));
    }

    // 3. A different refresh token with an older access expiry is a stale write.
    if old.refresh_token != incoming.refresh_token && incoming.expires_at < old.expires_at {
        return Err(CcredError::UnsafeWrite(
            "stale credential rotation".to_string(),
        ));
    }

    // 4. Never overwrite a live account with an already-dead one.
    if incoming
        .refresh_token_expires_at
        .is_some_and(|t| t <= now_ms)
        && old.refresh_token_expires_at.is_some_and(|t| t > now_ms)
    {
        return Err(CcredError::UnsafeWrite(
            "incoming refresh token has already expired".to_string(),
        ));
    }

    Ok(())
}

/// Names reserved on Windows -- with or without an extension, any case.
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A profile name proven safe to place into a path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProfileName(String);

impl ProfileName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProfileName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validate a profile name. This is a security control, not cosmetics --
/// `ccred switch ../../.ssh/id_rsa` must fail.
pub fn validate_profile_name(name: &str) -> Result<ProfileName> {
    let bad = |reason: &'static str| CcredError::InvalidProfileName {
        name: name.to_string(),
        reason,
    };

    if name.is_empty() {
        return Err(bad("empty"));
    }
    if name.len() > 64 {
        return Err(bad("longer than 64 characters"));
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err(bad("must start with a letter or digit"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(bad("allowed characters are A-Z a-z 0-9 . _ -"));
    }
    if name.contains("..") {
        return Err(bad("path traversal"));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(bad("trailing dot or space (invalid on Windows)"));
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    if WINDOWS_RESERVED.contains(&stem.as_str()) {
        return Err(bad("reserved device name on Windows"));
    }
    Ok(ProfileName(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    const NOW: i64 = 1_788_000_000_000;
    const DAY: i64 = 86_400_000;

    fn tok(prefix: &str) -> Secret {
        Secret::new(format!("sk-ant-{prefix}-{}", "A".repeat(90)))
    }

    fn creds(expires_at: i64, refresh_exp: Option<i64>, refresh: &str) -> OAuthCredentials {
        OAuthCredentials {
            access_token: tok("oat01"),
            refresh_token: Secret::new(format!("sk-ant-ort01-{}", refresh.repeat(30))),
            expires_at,
            refresh_token_expires_at: refresh_exp,
            scopes: None,
            subscription_type: None,
            extra: Map::new(),
        }
    }

    // ------------------------------------------------------- v1 regression

    /// This test is the whole reason the module exists. The bash version tested
    /// tokens with `jq -e`, which succeeds for `""`. Every value below must be
    /// rejected.
    #[test]
    fn v1_regression_blank_and_bogus_tokens_are_rejected() {
        let cases: &[(&str, &str)] = &[
            ("empty string", ""),
            ("spaces only", "   "),
            ("newline", "\n"),
            ("tab", "\t"),
            ("literal null", "null"),
            ("literal undefined", "undefined"),
            ("truncated prefix", "sk-ant-ort01-"),
            ("prefix only", "sk-ant-"),
            (
                "foreign shape",
                "Bearer abcdefghijklmnopqrstuvwxyz0123456789",
            ),
            ("long enough but no prefix", &"x".repeat(120)),
        ];
        for (label, value) in cases {
            let secret = Secret::new(*value);
            assert!(
                validate_token("refreshToken", Some(&secret)).is_err(),
                "'{label}' should have been rejected but passed"
            );
        }
    }

    #[test]
    fn missing_token_is_rejected() {
        assert!(validate_token("refreshToken", None).is_err());
    }

    #[test]
    fn a_real_looking_token_is_accepted() {
        assert!(validate_token("accessToken", Some(&tok("oat01"))).is_ok());
        assert!(validate_token("refreshToken", Some(&tok("ort01"))).is_ok());
    }

    #[test]
    fn a_future_token_version_still_validates() {
        // When Anthropic rotates the version, this tool must not break.
        assert!(validate_token("accessToken", Some(&tok("oat99"))).is_ok());
        assert!(validate_token("accessToken", Some(&tok("xyz1"))).is_ok());
    }

    #[test]
    fn embedded_whitespace_is_rejected() {
        let s = Secret::new(format!(
            "sk-ant-oat01-{} {}",
            "A".repeat(40),
            "B".repeat(40)
        ));
        assert!(validate_token("accessToken", Some(&s)).is_err());
    }

    // ------------------------------------- never replace valid with worse

    #[test]
    fn first_write_is_always_allowed() {
        let incoming = creds(NOW + DAY, Some(NOW + 30 * DAY), "C");
        assert!(assert_safe_replacement(None, &incoming, NOW).is_ok());
    }

    #[test]
    fn refresh_window_may_not_regress() {
        let existing = creds(NOW + DAY, Some(NOW + 30 * DAY), "C");
        let incoming = creds(NOW + DAY, Some(NOW + 20 * DAY), "D");
        let err = assert_safe_replacement(Some(&existing), &incoming, NOW).unwrap_err();
        assert!(matches!(err, CcredError::UnsafeWrite(_)), "{err}");
    }

    #[test]
    fn refresh_window_moving_forward_is_fine() {
        let existing = creds(NOW + DAY, Some(NOW + 20 * DAY), "C");
        let incoming = creds(NOW + 2 * DAY, Some(NOW + 30 * DAY), "D");
        assert!(assert_safe_replacement(Some(&existing), &incoming, NOW).is_ok());
    }

    #[test]
    fn a_dead_incoming_may_not_replace_a_live_one() {
        let existing = creds(NOW + DAY, Some(NOW + 20 * DAY), "C");
        let incoming = creds(NOW + DAY, Some(NOW - DAY), "D");
        assert!(assert_safe_replacement(Some(&existing), &incoming, NOW).is_err());
    }

    #[test]
    fn repairing_a_broken_profile_is_allowed() {
        // The stored profile has empty tokens (exactly the incident state).
        // A valid incoming write must be able to repair it.
        let mut broken = creds(0, None, "C");
        broken.access_token = Secret::new("");
        broken.refresh_token = Secret::new("");
        let incoming = creds(NOW + DAY, Some(NOW + 30 * DAY), "D");
        assert!(assert_safe_replacement(Some(&broken), &incoming, NOW).is_ok());
    }

    #[test]
    fn an_invalid_incoming_is_always_refused() {
        let existing = creds(NOW + DAY, Some(NOW + 20 * DAY), "C");
        let mut incoming = creds(NOW + 2 * DAY, Some(NOW + 30 * DAY), "D");
        incoming.refresh_token = Secret::new("");
        assert!(assert_safe_replacement(Some(&existing), &incoming, NOW).is_err());
    }

    /// Documents a real gap: two different accounts can both be valid with
    /// advancing windows, so this gate alone cannot stop a cross-account write.
    /// A live near-miss happened exactly this way -- the wrong account's
    /// credentials were about to be stored into another account's profile
    /// because its window was *longer*. Identity must be checked by the caller.
    #[test]
    fn window_gate_alone_does_not_catch_a_different_account() {
        let existing = creds(NOW + DAY, Some(NOW + 13 * DAY), "C");
        let other_account = creds(NOW + DAY, Some(NOW + 28 * DAY), "D");
        assert!(
            assert_safe_replacement(Some(&existing), &other_account, NOW).is_ok(),
            "the window gate is not an identity check; callers must add one"
        );
    }

    // --------------------------------------------------------- profile names

    #[test]
    fn traversal_and_reserved_names_are_rejected() {
        let bad = [
            "..",
            ".",
            "../x",
            "..\\x",
            "/etc/passwd",
            "C:\\Windows",
            "a/b",
            "a\\b",
            "con",
            "CON",
            "con.txt",
            "nul",
            "lpt1",
            "-leading",
            ".hidden",
            "trailing.",
            "trailing ",
            "with space",
            "",
            &"x".repeat(65),
        ];
        for name in bad {
            assert!(
                validate_profile_name(name).is_err(),
                "'{name}' should have been rejected"
            );
        }
    }

    #[test]
    fn ordinary_names_are_accepted() {
        for name in ["work", "personal", "work-2", "a.b_c", "X1"] {
            assert!(
                validate_profile_name(name).is_ok(),
                "'{name}' should have passed"
            );
        }
    }

    #[test]
    fn health_reports_expiry() {
        let c = creds(NOW - 1, Some(NOW + 10 * DAY), "C");
        let h = validate_credentials(&c, NOW).unwrap();
        assert!(h.access_expired);
        assert!(!h.refresh_expired);
        assert_eq!(h.refresh_window_left_ms, Some(10 * DAY));
    }
}
