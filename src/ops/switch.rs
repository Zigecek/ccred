//! Switching the active account.
//!
//! The ordering below is not arbitrary. Credentials are written before the
//! account identity in `.claude.json`, because `oauthAccount` is a cache that
//! Claude Code refetches: a window where the credentials say B but the
//! identity still says A is cosmetic and self-heals. The reverse -- showing
//! account B while acting as account A -- would be actively misleading.

use std::time::Duration;

use serde::Serialize;

use super::Ctx;
use crate::error::CcredError;
use crate::journal::{Recovery, SwitchJournal, SwitchPhase};
use crate::model::ClaudeJsonDoc;
use crate::proc::running_claude_pids;
use crate::store::{CredentialStore, now_ms};
use crate::validate::{ProfileName, validate_credentials, validate_profile_name};

/// How long to wait for the credential store lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
pub enum OutgoingSync {
    /// No profile was active, so there was nothing to preserve.
    NothingActive,
    Synced(String),
    /// Deliberately not synced. The reason is always worth showing.
    Skipped {
        profile: String,
        reason: String,
        /// Where the live credentials were copied before being overwritten.
        /// `None` means there was nothing usable to keep.
        #[serde(skip_serializing_if = "Option::is_none")]
        backup: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SwitchReport {
    pub from: Option<String>,
    pub to: String,
    pub account: String,
    pub outgoing: OutgoingSync,
    pub identity_restored: bool,
    /// Set when an interrupted earlier switch was healed first.
    pub recovered: Option<String>,
    pub warnings: Vec<String>,
    pub claude_running: Vec<u32>,
}

pub fn switch(ctx: &Ctx, target: &ProfileName, force: bool) -> crate::Result<SwitchReport> {
    let mut warnings = Vec::new();
    let recovered = recover_pending(ctx, &mut warnings)?;

    if !ctx.repo().exists(target)? {
        return Err(CcredError::ProfileNotFound(target.as_str().to_string()));
    }

    // Preflight the target BEFORE touching anything. A profile that cannot be
    // switched to must not cost us the live state on the way to finding out.
    let target_store = ctx.repo().store(target)?;
    let Some(target_loaded) = target_store.load()? else {
        return Err(CcredError::UnsafeWrite(format!(
            "profile '{target}' has no stored credentials"
        )));
    };
    let now = now_ms();
    let health = validate_credentials(&target_loaded.creds.oauth, now)?;
    if health.refresh_expired {
        return Err(CcredError::UnsafeWrite(format!(
            "profile '{target}' has an expired refresh token; run `claude auth login` \
             and then `ccred save {target}`"
        )));
    }

    let claude_running = running_claude_pids(ctx.paths().claude_config_dir());
    if !claude_running.is_empty() && !force {
        return Err(CcredError::UnsafeWrite(format!(
            "Claude Code is running (pid {claude_running:?}); it would write the old \
             account's refreshed token into the new profile's file. Quit it, or pass --force"
        )));
    }

    let live = ctx.live_store();
    let _guard = live.lock(LOCK_TIMEOUT)?;

    let from = ctx.repo().active()?;
    let journal_path = ctx.paths().switch_journal();
    let mut journal = SwitchJournal::new(
        from.as_ref().map(|n| n.as_str().to_string()),
        target.as_str().to_string(),
        now,
    );
    journal.save(&journal_path)?;

    let mut outgoing = sync_outgoing(ctx, from.as_ref())?;

    // The mirror above is skipped precisely when the live account is not the
    // one the pointer names -- which is the case where those credentials are
    // stored nowhere else. The next line destroys them, so copy them aside
    // first. This is the step whose absence made `switch` able to lose an
    // account outright.
    if let OutgoingSync::Skipped { backup, .. } = &mut outgoing {
        *backup = ctx
            .repo()
            .backup_orphaned_live(&live)?
            .map(|p| p.display().to_string());
    }
    journal.advance(&journal_path, SwitchPhase::OutgoingSynced)?;

    // A deliberate account change, so the monotonic window rule is skipped.
    live.replace(&target_loaded.creds)?;
    journal.advance(&journal_path, SwitchPhase::LiveCredsWritten)?;

    let identity_restored = restore_identity(ctx, target, &mut warnings)?;
    journal.advance(&journal_path, SwitchPhase::LiveIdentityWritten)?;

    ctx.repo().set_active(target)?;
    journal.advance(&journal_path, SwitchPhase::PointerUpdated)?;
    SwitchJournal::clear(&journal_path)?;

    let account = ctx
        .repo()
        .meta(target)?
        .map(|m| m.account.label())
        .unwrap_or_else(|| "<unknown account>".into());

    Ok(SwitchReport {
        from: from.map(|n| n.as_str().to_string()),
        to: target.as_str().to_string(),
        account,
        outgoing,
        identity_restored,
        recovered,
        warnings,
        claude_running,
    })
}

/// Mirror the live credentials into the profile we are leaving.
///
/// This is the step that preserves a freshly refreshed token: until it runs,
/// the outgoing account's newest credentials exist only in the live file, and
/// the next write would replace them.
///
/// An account mismatch here is NOT a failure. It means the pointer disagrees
/// with who is really logged in -- exactly the situation a switch is being
/// used to fix -- and syncing anyway would store the wrong account's
/// credentials into the profile.
fn sync_outgoing(ctx: &Ctx, from: Option<&ProfileName>) -> crate::Result<OutgoingSync> {
    let Some(from) = from else {
        return Ok(OutgoingSync::NothingActive);
    };
    if !ctx.repo().exists(from)? {
        return Ok(OutgoingSync::Skipped {
            profile: from.as_str().to_string(),
            reason: "the profile no longer exists".into(),
            backup: None,
        });
    }

    let account = ctx.live_account();
    match ctx.repo().save_from(from, &ctx.live_store(), &account) {
        Ok(_) => Ok(OutgoingSync::Synced(from.as_str().to_string())),
        Err(
            e @ (CcredError::AccountMismatch { .. }
            | CcredError::AccountUnverifiable { .. }
            | CcredError::InvalidCredentials(_)
            | CcredError::UnsafeWrite(_)),
        ) => Ok(OutgoingSync::Skipped {
            profile: from.as_str().to_string(),
            reason: e.to_string(),
            backup: None,
        }),
        Err(other) => Err(other),
    }
}

/// Put the target profile's `oauthAccount` blob back into `.claude.json`.
fn restore_identity(
    ctx: &Ctx,
    target: &ProfileName,
    warnings: &mut Vec<String>,
) -> crate::Result<bool> {
    let Some(blob) = ctx.repo().oauth_account(target)? else {
        warnings.push(format!(
            "profile '{target}' has no stored account details; Claude Code will \
             refetch them on next start"
        ));
        return Ok(false);
    };
    let config_path = ctx.paths().claude_config_file();
    let mut doc = match ClaudeJsonDoc::load(config_path) {
        Ok(d) => d,
        Err(e) => {
            warnings.push(format!("could not update {}: {e}", config_path.display()));
            return Ok(false);
        }
    };
    doc.set_oauth_account(blob)?;
    doc.save_atomic(config_path)?;
    Ok(true)
}

/// Heal an interrupted switch before doing anything else.
pub fn recover_pending(ctx: &Ctx, warnings: &mut Vec<String>) -> crate::Result<Option<String>> {
    let path = ctx.paths().switch_journal();
    let Some(journal) = SwitchJournal::load(&path)? else {
        return Ok(None);
    };

    let summary = match journal.recovery() {
        Recovery::RollBackTo(from) => {
            // Live was never modified, so the pointer is the only thing that
            // can be wrong.
            match from {
                Some(name) => {
                    let name = validate_profile_name(&name)?;
                    ctx.repo().set_active(&name)?;
                    format!("rolled an interrupted switch back to '{name}'")
                }
                None => "discarded an interrupted switch that had no previous profile".to_string(),
            }
        }
        Recovery::CompleteForward(to) => {
            let name = validate_profile_name(&to)?;
            let restored = restore_identity(ctx, &name, warnings)?;
            ctx.repo().set_active(&name)?;
            if restored {
                format!("completed an interrupted switch to '{name}'")
            } else {
                format!("completed an interrupted switch to '{name}' (account details missing)")
            }
        }
        Recovery::AlreadyDone => "cleared a finished switch journal".to_string(),
    };

    SwitchJournal::clear(&path)?;
    Ok(Some(summary))
}
