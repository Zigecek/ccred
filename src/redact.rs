//! A string type that cannot be printed by accident.

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// A value that must never reach output or logs.
///
/// `Display` is **deliberately not implemented**, which makes
/// `println!("{}", secret)` a compile error rather than a runtime leak.
/// `Debug` prints only a length and a fingerprint. The single route to the raw
/// value is [`Secret::expose`] -- one greppable name whose call sites are
/// checked by a test.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Empty or whitespace-only -- exactly the state `jq -e` let through.
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }

    /// Stable, non-reversible fingerprint for logs and diffs.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.0.as_bytes());
        format!("sha256:{}", &hex::encode(digest)[..12])
    }

    /// The only accessor for the raw value. Call it from `store` and
    /// `validate` only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            write!(f, "Secret(<empty>)")
        } else {
            write!(
                f,
                "Secret(<redacted {}B {}>)",
                self.0.len(),
                self.fingerprint()
            )
        }
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL: &str = "sk-ant-oat01-SENTINELvalue0000000000000000000000000000";

    #[test]
    fn debug_never_reveals_the_value() {
        let s = Secret::new(SENTINEL);
        let rendered = format!("{s:?}");
        assert!(
            !rendered.contains(SENTINEL),
            "Debug leaked the token: {rendered}"
        );
        assert!(
            !rendered.contains("SENTINEL"),
            "Debug leaked the token: {rendered}"
        );
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn blank_detection_covers_whitespace() {
        assert!(Secret::new("").is_blank());
        assert!(Secret::new("   ").is_blank());
        assert!(Secret::new("\n\t").is_blank());
        assert!(!Secret::new("x").is_blank());
    }

    #[test]
    fn fingerprint_is_stable_and_not_the_value() {
        let a = Secret::new(SENTINEL);
        let b = Secret::new(SENTINEL);
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.fingerprint(), Secret::new("other").fingerprint());
        assert!(!a.fingerprint().contains("SENTINEL"));
    }

    #[test]
    fn serde_roundtrip_is_transparent() {
        let s = Secret::new("abc");
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"abc\"");
        let back: Secret = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expose(), "abc");
    }
}
