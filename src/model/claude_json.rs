//! Patching `.claude.json`.
//!
//! This file is not ours and it is large: a live install has ~68 top-level
//! keys, including per-project history, MCP configuration and several hundred
//! cached feature flags. We change exactly one key, `oauthAccount`, and leave
//! everything else byte-identical.
//!
//! It is deliberately **not** modelled as a struct. Modelling foreign state
//! that changes between releases is a liability; patching a raw `Value` is not.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::atomic::write_atomic;
use crate::error::CcredError;

/// A loaded `.claude.json`, kept as an opaque tree.
#[derive(Debug, Clone)]
pub struct ClaudeJsonDoc {
    root: Value,
    /// Top-level key count when loaded, used as a corruption guard on write.
    loaded_key_count: usize,
}

impl ClaudeJsonDoc {
    pub fn load(path: &Path) -> crate::Result<Self> {
        let raw = std::fs::read(path).map_err(|source| CcredError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let root: Value = serde_json::from_slice(&raw).map_err(|source| CcredError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        let loaded_key_count = root.as_object().map(|o| o.len()).unwrap_or(0);
        Ok(ClaudeJsonDoc {
            root,
            loaded_key_count,
        })
    }

    /// The raw `oauthAccount` object, uninterpreted.
    pub fn oauth_account(&self) -> Option<&Value> {
        self.root.get("oauthAccount").filter(|v| !v.is_null())
    }

    pub fn set_oauth_account(&mut self, value: Value) -> crate::Result<()> {
        let obj = self
            .root
            .as_object_mut()
            .ok_or_else(|| CcredError::UnsafeWrite("`.claude.json` is not an object".into()))?;
        obj.insert("oauthAccount".to_string(), value);
        Ok(())
    }

    /// Mark a directory as trusted so a non-interactive `claude` run in it does
    /// not stop on the trust prompt.
    pub fn trust_dir(&mut self, dir: &Path) -> crate::Result<()> {
        let key = dir.to_string_lossy().to_string();
        let obj = self
            .root
            .as_object_mut()
            .ok_or_else(|| CcredError::UnsafeWrite("`.claude.json` is not an object".into()))?;
        let projects = obj
            .entry("projects")
            .or_insert_with(|| Value::Object(Default::default()));
        let projects = projects.as_object_mut().ok_or_else(|| {
            CcredError::UnsafeWrite("`projects` in `.claude.json` is not an object".into())
        })?;
        let entry = projects
            .entry(key)
            .or_insert_with(|| Value::Object(Default::default()));
        if let Some(map) = entry.as_object_mut() {
            map.insert("hasTrustDialogAccepted".to_string(), Value::Bool(true));
        }
        Ok(())
    }

    /// Write back atomically, refusing an implausible shrink.
    ///
    /// If a bug ever reduced this document to a handful of keys, writing it
    /// would destroy the user's project history and MCP setup. Cheap guard,
    /// catastrophic failure prevented.
    pub fn save_atomic(&self, path: &Path) -> crate::Result<()> {
        let now = self.root.as_object().map(|o| o.len()).unwrap_or(0);
        if now < self.loaded_key_count {
            return Err(CcredError::UnsafeWrite(format!(
                "refusing to write {}: top-level keys dropped from {} to {}",
                path.display(),
                self.loaded_key_count,
                now
            )));
        }
        let bytes = serde_json::to_vec_pretty(&self.root).map_err(|source| CcredError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        write_atomic(path, &bytes, true)
    }

    pub fn top_level_key_count(&self) -> usize {
        self.root.as_object().map(|o| o.len()).unwrap_or(0)
    }
}

/// A read-only projection of `oauthAccount`, for display and for comparison.
///
/// Never used on the write path -- when switching profiles we move the whole
/// opaque `oauthAccount` value, because it holds twenty fields we do not model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_tier: Option<String>,
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_string)
}

impl AccountIdentity {
    pub fn from_oauth_account(v: &Value) -> Self {
        AccountIdentity {
            email: str_field(v, "emailAddress"),
            account_uuid: str_field(v, "accountUuid"),
            organization_uuid: str_field(v, "organizationUuid"),
            organization_name: str_field(v, "organizationName"),
            subscription_type: str_field(v, "organizationType"),
            rate_limit_tier: str_field(v, "organizationRateLimitTier"),
        }
    }

    /// Does this identity refer to the same account as `other`?
    ///
    /// Compares the account UUID when both sides have one, because an e-mail
    /// address can change while the account stays the same. Falls back to a
    /// case-insensitive e-mail comparison.
    ///
    /// Returns `None` when there is not enough information to decide -- callers
    /// must treat that as "unknown", never as "matches".
    pub fn same_account_as(&self, other: &AccountIdentity) -> Option<bool> {
        if let (Some(a), Some(b)) = (&self.account_uuid, &other.account_uuid) {
            return Some(a == b);
        }
        if let (Some(a), Some(b)) = (&self.email, &other.email) {
            return Some(a.eq_ignore_ascii_case(b));
        }
        None
    }

    /// Short label for messages. Never contains a token.
    pub fn label(&self) -> String {
        self.email
            .clone()
            .or_else(|| self.account_uuid.clone())
            .unwrap_or_else(|| "<unknown account>".to_string())
    }
}

/// An account identity plus the opaque blob it came from.
///
/// The projection in [`AccountIdentity`] is for comparing and displaying. When
/// a switch puts an account back, it must restore the *whole* `oauthAccount`
/// value -- a live install has twenty fields there and we model six, so
/// rebuilding it from the projection would quietly discard the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountSnapshot {
    pub identity: AccountIdentity,
    pub raw: Option<Value>,
}

impl AccountSnapshot {
    pub fn from_oauth_account(v: &Value) -> Self {
        AccountSnapshot {
            identity: AccountIdentity::from_oauth_account(v),
            raw: Some(v.clone()),
        }
    }

    /// Read the account out of a loaded `.claude.json`, if it has one.
    pub fn from_config(doc: &ClaudeJsonDoc) -> Self {
        match doc.oauth_account() {
            Some(v) => Self::from_oauth_account(v),
            None => AccountSnapshot::default(),
        }
    }

    pub fn label(&self) -> String {
        self.identity.label()
    }

    pub fn is_known(&self) -> bool {
        self.identity != AccountIdentity::default()
    }
}

/// Where `.claude.json` lives for a given config directory, for error messages.
pub fn describe_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn doc_with(root: Value) -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
        (dir, path)
    }

    /// Stand-in for the real file: many unrelated keys plus oauthAccount.
    fn realistic() -> Value {
        json!({
            "numStartups": 412,
            "installMethod": "native",
            "projects": { "/home/user/code": { "hasTrustDialogAccepted": true } },
            "mcpServers": { "example": { "command": "x" } },
            "cachedGrowthBookFeatures": { "flag_a": true, "flag_b": false },
            "tipsHistory": { "tip": 3 },
            "oauthAccount": {
                "accountUuid": "aaaaaaaa-0000-0000-0000-000000000000",
                "emailAddress": "person@example.com",
                "organizationUuid": "bbbbbbbb-0000-0000-0000-000000000000",
                "organizationName": "Example Org",
                "organizationType": "claude_max",
                "organizationRateLimitTier": "default_claude_max_5x",
                "profileFetchedAt": 1788000000000_i64
            }
        })
    }

    #[test]
    fn patching_oauth_account_preserves_every_other_key() {
        let (_d, path) = doc_with(realistic());
        let mut doc = ClaudeJsonDoc::load(&path).unwrap();
        let before = doc.top_level_key_count();

        doc.set_oauth_account(json!({"emailAddress": "other@example.com"}))
            .unwrap();
        doc.save_atomic(&path).unwrap();

        let reloaded = ClaudeJsonDoc::load(&path).unwrap();
        assert_eq!(reloaded.top_level_key_count(), before);

        let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["numStartups"], json!(412));
        assert_eq!(raw["mcpServers"]["example"]["command"], json!("x"));
        assert_eq!(raw["cachedGrowthBookFeatures"]["flag_a"], json!(true));
        assert_eq!(
            raw["projects"]["/home/user/code"]["hasTrustDialogAccepted"],
            json!(true)
        );
        assert_eq!(
            raw["oauthAccount"]["emailAddress"],
            json!("other@example.com")
        );
    }

    #[test]
    fn refuses_to_write_a_document_that_lost_keys() {
        let (_d, path) = doc_with(realistic());
        let mut doc = ClaudeJsonDoc::load(&path).unwrap();

        // Simulate a bug that gutted the document.
        doc.root = json!({ "oauthAccount": {} });

        let err = doc.save_atomic(&path).unwrap_err();
        assert!(matches!(err, CcredError::UnsafeWrite(_)), "{err}");

        // The file on disk must be untouched.
        let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["numStartups"], json!(412));
    }

    #[test]
    fn trust_dir_adds_the_flag_without_disturbing_others() {
        let (_d, path) = doc_with(realistic());
        let mut doc = ClaudeJsonDoc::load(&path).unwrap();
        doc.trust_dir(Path::new("/tmp/profile")).unwrap();
        doc.save_atomic(&path).unwrap();

        let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            raw["projects"]["/tmp/profile"]["hasTrustDialogAccepted"],
            json!(true)
        );
        assert_eq!(
            raw["projects"]["/home/user/code"]["hasTrustDialogAccepted"],
            json!(true)
        );
    }

    #[test]
    fn identity_is_read_out_of_the_account_object() {
        let root = realistic();
        let id = AccountIdentity::from_oauth_account(&root["oauthAccount"]);
        assert_eq!(id.email.as_deref(), Some("person@example.com"));
        assert_eq!(id.organization_name.as_deref(), Some("Example Org"));
        assert_eq!(id.label(), "person@example.com");
    }

    #[test]
    fn uuid_wins_over_email_when_comparing() {
        let a = AccountIdentity {
            account_uuid: Some("same".into()),
            email: Some("old@example.com".into()),
            ..Default::default()
        };
        let b = AccountIdentity {
            account_uuid: Some("same".into()),
            email: Some("new@example.com".into()),
            ..Default::default()
        };
        // An address can change while the account does not.
        assert_eq!(a.same_account_as(&b), Some(true));
    }

    #[test]
    fn email_comparison_ignores_case() {
        let a = AccountIdentity {
            email: Some("Person@Example.com".into()),
            ..Default::default()
        };
        let b = AccountIdentity {
            email: Some("person@example.com".into()),
            ..Default::default()
        };
        assert_eq!(a.same_account_as(&b), Some(true));
    }

    #[test]
    fn different_accounts_are_reported_as_different() {
        let a = AccountIdentity {
            account_uuid: Some("one".into()),
            ..Default::default()
        };
        let b = AccountIdentity {
            account_uuid: Some("two".into()),
            ..Default::default()
        };
        assert_eq!(a.same_account_as(&b), Some(false));
    }

    #[test]
    fn a_snapshot_keeps_every_field_not_just_the_projected_ones() {
        let root = realistic();
        let snap = AccountSnapshot::from_oauth_account(&root["oauthAccount"]);
        assert_eq!(snap.identity.email.as_deref(), Some("person@example.com"));
        // profileFetchedAt is not in the projection, but must survive.
        assert_eq!(
            snap.raw.as_ref().unwrap()["profileFetchedAt"],
            json!(1788000000000_i64)
        );
        assert!(snap.is_known());
    }

    #[test]
    fn a_config_without_an_account_yields_an_unknown_snapshot() {
        let (_d, path) = doc_with(json!({ "numStartups": 1 }));
        let doc = ClaudeJsonDoc::load(&path).unwrap();
        let snap = AccountSnapshot::from_config(&doc);
        assert!(!snap.is_known());
        assert_eq!(snap.label(), "<unknown account>");
    }

    #[test]
    fn insufficient_information_is_unknown_not_a_match() {
        // The caller must not read this as "same account".
        let empty = AccountIdentity::default();
        assert_eq!(empty.same_account_as(&AccountIdentity::default()), None);
    }
}
