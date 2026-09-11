//! Model of the `.credentials.json` file.
//!
//! The real file on disk looks like this (verified against live installs on
//! Claude Code 2.1.236 and 2.1.260):
//!
//! ```json
//! {
//!   "claudeAiOauth": { "accessToken": "sk-ant-oat01-...", ... },
//!   "organizationUuid": "..."
//! }
//! ```
//!
//! That `organizationUuid` sibling at the top level is easy to miss -- a naive
//! `struct { claudeAiOauth }` silently discards it on rewrite. Hence
//! `#[serde(flatten)] extra` everywhere, plus a runtime comparison of every
//! write against the bytes we read (see [`assert_lossless`]).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::redact::Secret;

pub type JsonMap = Map<String, Value>;

/// The whole contents of `.credentials.json`.
///
/// CAUTION: `#[serde(flatten)]` is incompatible with
/// `#[serde(deny_unknown_fields)]` -- combining them makes every field unknown.
/// Never add `deny_unknown_fields` here. The flattened field must be declared
/// last.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    pub oauth: OAuthCredentials,

    /// Captures `organizationUuid` and anything Claude Code adds in future.
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    #[serde(rename = "accessToken")]
    pub access_token: Secret,

    #[serde(rename = "refreshToken")]
    pub refresh_token: Secret,

    /// Epoch milliseconds. `i64` rather than `u64`: `flatten` routes numbers
    /// through a buffered `Content` that can mangle values above `i64::MAX`.
    /// Epoch-ms is far below that, so this is safe.
    #[serde(rename = "expiresAt")]
    pub expires_at: i64,

    #[serde(
        rename = "refreshTokenExpiresAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub refresh_token_expires_at: Option<i64>,

    /// `Option` rather than `Vec`, so absent and empty stay distinguishable.
    ///
    /// With `skip_serializing_if = "Vec::is_empty"` an input of `"scopes": []`
    /// re-serialised to nothing at all, `assert_lossless` correctly refused
    /// the write, and every command that touches that file failed. Round-trip
    /// fidelity is the whole contract here; an empty list is a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,

    #[serde(
        rename = "subscriptionType",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub subscription_type: Option<String>,

    /// `rateLimitTier` and any other field we do not model.
    #[serde(flatten)]
    pub extra: JsonMap,
}

/// Collect key paths present in `before` but missing from `after`.
fn missing_key_paths(before: &Value, after: &Value, prefix: &str, out: &mut Vec<String>) {
    let Value::Object(before_map) = before else {
        return;
    };
    let after_map = match after {
        Value::Object(m) => m,
        _ => {
            out.push(prefix.trim_end_matches('.').to_string());
            return;
        }
    };
    for (key, before_val) in before_map {
        let path = format!("{prefix}{key}");
        match after_map.get(key) {
            None => out.push(path),
            Some(after_val) => {
                if before_val.is_object() {
                    missing_key_paths(before_val, after_val, &format!("{path}."), out);
                }
            }
        }
    }
}

/// Verify that a rewrite dropped no key we do not understand.
///
/// This is a runtime backstop on top of `#[serde(flatten)]`: if a future
/// Claude Code version adds a field our model fails to carry for any reason,
/// the write is **refused** rather than silently losing it. Only key names are
/// reported, never values.
pub fn assert_lossless(original: &[u8], reserialized: &Value) -> crate::Result<()> {
    let before: Value =
        serde_json::from_slice(original).map_err(|source| crate::CcredError::Json {
            path: std::path::PathBuf::from("<buffer>"),
            source,
        })?;
    let mut dropped = Vec::new();
    missing_key_paths(&before, reserialized, "", &mut dropped);
    if dropped.is_empty() {
        Ok(())
    } else {
        Err(crate::CcredError::LossyRewrite { dropped })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape from a live install, sibling `organizationUuid` included.
    const REAL_SHAPE: &str = r#"{
      "claudeAiOauth": {
        "accessToken": "sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "refreshToken": "sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        "expiresAt": 1788531142673,
        "refreshTokenExpiresAt": 1789660808000,
        "scopes": ["user:inference", "user:profile"],
        "subscriptionType": "max",
        "rateLimitTier": "default_claude_max_5x"
      },
      "organizationUuid": "b5c1c992-726a-4dab-9d8e-d225ba8ee6d4"
    }"#;

    #[test]
    fn roundtrip_keeps_sibling_organization_uuid() {
        let parsed: CredentialsFile = serde_json::from_str(REAL_SHAPE).unwrap();
        assert!(
            parsed.extra.contains_key("organizationUuid"),
            "organizationUuid was not captured into extra"
        );

        let back = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            back.get("organizationUuid").and_then(|v| v.as_str()),
            Some("b5c1c992-726a-4dab-9d8e-d225ba8ee6d4")
        );
    }

    #[test]
    fn roundtrip_keeps_unknown_inner_fields() {
        let parsed: CredentialsFile = serde_json::from_str(REAL_SHAPE).unwrap();
        let back = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            back["claudeAiOauth"]["rateLimitTier"].as_str(),
            Some("default_claude_max_5x")
        );
    }

    #[test]
    fn roundtrip_survives_a_field_we_have_never_seen() {
        let with_future: String = REAL_SHAPE.replace(
            r#""organizationUuid""#,
            r#""futureUnknownField": {"nested": [1,2,3]}, "organizationUuid""#,
        );
        let parsed: CredentialsFile = serde_json::from_str(&with_future).unwrap();
        let back = serde_json::to_value(&parsed).unwrap();
        assert_eq!(back["futureUnknownField"]["nested"][2].as_i64(), Some(3));
        assert_lossless(with_future.as_bytes(), &back).expect("round-trip must be lossless");
    }

    /// An empty `scopes` array is a value, not an absence.
    ///
    /// It used to re-serialise to nothing, which made `assert_lossless` refuse
    /// the write and turned every command that touched such a file into a
    /// hard failure. Absent and empty must stay distinguishable in both
    /// directions.
    #[test]
    fn an_empty_scopes_array_survives_the_round_trip() {
        let raw = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r",
                      "expiresAt":1,"scopes":[]}}"#;
        let parsed: CredentialsFile = serde_json::from_str(raw).unwrap();
        let back = serde_json::to_value(&parsed).unwrap();
        assert_eq!(back["claudeAiOauth"]["scopes"], serde_json::json!([]));
        assert_lossless(raw.as_bytes(), &back).expect("an empty array must not read as dropped");
    }

    #[test]
    fn an_absent_scopes_key_is_not_invented() {
        let raw = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":1}}"#;
        let parsed: CredentialsFile = serde_json::from_str(raw).unwrap();
        let back = serde_json::to_value(&parsed).unwrap();
        assert!(
            back["claudeAiOauth"].get("scopes").is_none(),
            "a key that was not there must not appear: {back}"
        );
    }

    #[test]
    fn assert_lossless_catches_a_dropped_key() {
        let parsed: CredentialsFile = serde_json::from_str(REAL_SHAPE).unwrap();
        let mut back = serde_json::to_value(&parsed).unwrap();
        back.as_object_mut().unwrap().remove("organizationUuid");

        let err = assert_lossless(REAL_SHAPE.as_bytes(), &back).unwrap_err();
        match err {
            crate::CcredError::LossyRewrite { dropped } => {
                assert_eq!(dropped, vec!["organizationUuid".to_string()]);
            }
            other => panic!("expected LossyRewrite, got: {other}"),
        }
    }

    #[test]
    fn assert_lossless_catches_a_dropped_nested_key() {
        let parsed: CredentialsFile = serde_json::from_str(REAL_SHAPE).unwrap();
        let mut back = serde_json::to_value(&parsed).unwrap();
        back["claudeAiOauth"]
            .as_object_mut()
            .unwrap()
            .remove("rateLimitTier");

        let err = assert_lossless(REAL_SHAPE.as_bytes(), &back).unwrap_err();
        match err {
            crate::CcredError::LossyRewrite { dropped } => {
                assert_eq!(dropped, vec!["claudeAiOauth.rateLimitTier".to_string()]);
            }
            other => panic!("expected LossyRewrite, got: {other}"),
        }
    }

    #[test]
    fn debug_of_the_whole_file_leaks_nothing() {
        let parsed: CredentialsFile = serde_json::from_str(REAL_SHAPE).unwrap();
        let rendered = format!("{parsed:?}");
        assert!(
            !rendered.contains("AAAAAAAA"),
            "access token leaked: {rendered}"
        );
        assert!(
            !rendered.contains("BBBBBBBB"),
            "refresh token leaked: {rendered}"
        );
    }
}
