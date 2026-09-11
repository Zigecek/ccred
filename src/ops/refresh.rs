//! Keeping profiles alive.
//!
//! An account nobody uses for a few weeks has an expired refresh token and
//! needs a human to log in again. This is the loop that prevents that, and it
//! is deliberately timid.
//!
//! # Why it runs the real binary
//!
//! Refreshing is done by running `claude` against each profile's own
//! credential store. We never call the token endpoint ourselves: a refresh
//! token is single-use, replaying one is treated as theft, and unrecognised
//! refresh traffic from a server address has been reported to end in a block
//! that only a manual login clears.
//!
//! # Why it is idempotent
//!
//! Every scheduler has a way of firing more often than you asked. systemd
//! catches up after downtime, launchd coalesces missed calendar events, and a
//! Windows task with both a boot trigger and a schedule can do both at once.
//! Rather than trying to make three schedulers agree, `refresh` is safe to
//! over-fire: it checks when it last ran and exits successfully with
//! `"skipped"` if that was recent. The schedule becomes a hint, and the real
//! rate limit lives here.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::Ctx;
use crate::atomic::write_atomic;
use crate::claude_cli::{ClaudeCli, Probe};
use crate::error::CcredError;
use crate::store::resolve::{StorageScope, scope_for};
use crate::store::{CredentialStore, now_ms};
use crate::validate::{ProfileName, validate_credentials};

const DAY_MS: i64 = 86_400_000;

/// How long to wait for the credential store lock.
///
/// Shorter than the switch timeout on purpose: this runs unattended, so
/// giving up and reporting "busy" beats holding a scheduler job open.
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// When to act, and how hard to try.
#[derive(Debug, Clone)]
pub struct RefreshPolicy {
    /// Refresh once the remaining refresh window drops below this.
    ///
    /// A successful refresh resets the whole window, so acting with ten days
    /// to spare leaves at least three scheduled runs of margin before anything
    /// could actually die -- while making far fewer calls than refreshing on
    /// the access token's eight-hour cadence.
    pub window_below_ms: i64,
    /// Never attempt the same profile more often than this.
    pub min_interval_ms: i64,
    /// Give up on one profile after this long.
    pub spawn_timeout: Duration,
    /// Cap on how many profiles one run will spawn `claude` for.
    pub max_spawns: usize,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        RefreshPolicy {
            window_below_ms: 10 * DAY_MS,
            min_interval_ms: DAY_MS,
            spawn_timeout: Duration::from_secs(120),
            max_spawns: 4,
        }
    }
}

/// What one run recorded about itself.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LastRun {
    pub finished_at_ms: i64,
    pub status: String,
    /// Whether that run left anything a person has to deal with.
    ///
    /// The rate limit exists so a scheduler that double-fires costs nothing.
    /// Arming it from a run where every profile came back broken turns a
    /// transient failure into two days of silence, which is the opposite of
    /// what it is for. Old records without the field read as clean, so an
    /// upgrade does not suddenly re-run everything.
    #[serde(default)]
    pub needed_attention: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// The active profile is mirrored from the live store instead.
    MirrorActive,
    SkipFresh,
    SkipBackoff,
    SkipCap,
    Refresh,
    /// The refresh token is gone; only a person can fix this.
    NeedsLogin,
    Broken,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileResult {
    pub name: String,
    pub decision: Decision,
    pub detail: Option<String>,
    /// Days of refresh window before and after, when both are known.
    pub window_days_before: Option<i64>,
    pub window_days_after: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefreshReport {
    pub status: String,
    pub profiles: Vec<ProfileResult>,
}

impl RefreshReport {
    fn skipped(reason: &str) -> Self {
        RefreshReport {
            status: format!("skipped: {reason}"),
            profiles: Vec::new(),
        }
    }

    /// Did anything happen that a person needs to act on?
    pub fn needs_attention(&self) -> bool {
        self.profiles
            .iter()
            .any(|p| matches!(p.decision, Decision::NeedsLogin | Decision::Broken))
    }
}

/// Decide what to do with one profile. Pure, so the policy is testable.
pub fn decide(
    is_active: bool,
    window_left_ms: Option<i64>,
    refresh_expired: bool,
    credentials_usable: bool,
    state: &crate::profile::RefreshState,
    now: i64,
    policy: &RefreshPolicy,
) -> Decision {
    if is_active {
        return Decision::MirrorActive;
    }
    if !credentials_usable {
        return Decision::Broken;
    }
    if refresh_expired || state.needs_login {
        return Decision::NeedsLogin;
    }
    if let Some(after) = state.next_attempt_after_ms
        && now < after
    {
        return Decision::SkipBackoff;
    }
    if let Some(last) = state.last_attempt_ms
        && now - last < policy.min_interval_ms
    {
        return Decision::SkipBackoff;
    }
    match window_left_ms {
        Some(left) if left > policy.window_below_ms => Decision::SkipFresh,
        // No stated expiry means we cannot tell how urgent it is; leave it be
        // rather than refreshing something that may not need it.
        None => Decision::SkipFresh,
        Some(_) => Decision::Refresh,
    }
}

/// Exponential backoff with a hard ceiling.
fn backoff_ms(consecutive_failures: u32) -> i64 {
    let base = 30 * 60 * 1000i64; // 30 minutes
    let capped = consecutive_failures.min(6);
    (base << capped).min(DAY_MS)
}

#[derive(Debug, Clone, Default)]
pub struct RefreshOptions {
    pub policy: RefreshPolicy,
    /// Do nothing if the last successful run was more recent than this.
    pub if_older_than_ms: Option<i64>,
    pub claude_path: Option<std::path::PathBuf>,
    /// Ignore the backoff and the window threshold and try everything now.
    ///
    /// Never set by the scheduler. It exists for the person who has just
    /// fixed whatever was wrong and wants to watch it work, rather than
    /// waiting out a backoff that is measured in hours.
    pub force: bool,
}

pub fn refresh(ctx: &Ctx, opts: &RefreshOptions) -> crate::Result<RefreshReport> {
    let now = now_ms();

    // The self-rate-limit that makes over-firing harmless. `--force` is a
    // person asking directly, so it outranks the limit meant for schedulers.
    if !opts.force
        && let Some(window) = opts.if_older_than_ms
        && let Some(last) = read_last_run(ctx)?
        && now - last.finished_at_ms < window
        && !last.needed_attention
    {
        return Ok(RefreshReport::skipped("last run was recent"));
    }

    let active = ctx.repo().active()?;
    let mut results = Vec::new();
    let mut spawned = 0usize;

    // Resolved lazily: a machine with nothing to refresh should not fail just
    // because `claude` is not installed.
    let mut cli: Option<ClaudeCli> = None;

    for name in ctx.repo().list()? {
        let is_active = active.as_ref() == Some(&name);
        let store = ctx.repo().store(&name)?;
        let loaded = store.load().ok().flatten();

        let (usable, window_before, refresh_expired) = match &loaded {
            Some(l) => match validate_credentials(&l.creds.oauth, now) {
                Ok(h) => (true, h.refresh_window_left_ms, h.refresh_expired),
                Err(_) => (false, None, false),
            },
            None => (false, None, false),
        };

        let meta = ctx.repo().meta(&name)?;
        let mut state = meta.map(|m| m.refresh).unwrap_or_default();
        if opts.force {
            // Clear only the timing gates. `needs_login` stays: a person is
            // genuinely required there, and pretending otherwise would spawn
            // a process that cannot succeed.
            state.next_attempt_after_ms = None;
            state.last_attempt_ms = None;
        }
        let effective_policy = if opts.force {
            RefreshPolicy {
                // Everything below this is "refresh now"; i64::MAX would
                // overflow the comparison, so use a century.
                window_below_ms: i64::MAX / 4,
                min_interval_ms: 0,
                ..opts.policy.clone()
            }
        } else {
            opts.policy.clone()
        };
        let mut decision = decide(
            is_active,
            window_before,
            refresh_expired,
            usable,
            &state,
            now,
            &effective_policy,
        );

        if decision == Decision::Refresh && spawned >= opts.policy.max_spawns {
            decision = Decision::SkipCap;
        }

        let mut detail = None;
        let mut window_after = None;

        match decision {
            Decision::MirrorActive => {
                // Claude Code refreshes the live store on its own; our job is
                // only to copy the result into the profile so a later switch
                // does not lose it.
                //
                // Read it under the same lock Claude Code takes, or a refresh
                // landing mid-read copies half of one token pair and half of
                // the next. This runs from a timer, so it collides with a live
                // session more often than anything a person types.
                let live = ctx.live_store();
                match live.lock(LOCK_TIMEOUT) {
                    Ok(_guard) => {
                        let account = ctx.live_account();
                        match ctx.repo().save_from(&name, &live, &account) {
                            Ok(outcome) => detail = Some(format!("{outcome:?}").to_lowercase()),
                            Err(e) => detail = Some(format!("not mirrored: {e}")),
                        }
                    }
                    Err(e) => detail = Some(format!("not mirrored, store is busy: {e}")),
                }
            }
            Decision::Refresh => {
                spawned += 1;
                let cli = match &cli {
                    Some(c) => c,
                    None => {
                        cli = Some(ClaudeCli::discover(opts.claude_path.as_deref())?);
                        cli.as_ref().unwrap()
                    }
                };
                let outcome = refresh_one(ctx, &name, cli, &state, &effective_policy, now)?;
                decision = outcome.0;
                detail = outcome.1;
                window_after = store
                    .load()
                    .ok()
                    .flatten()
                    .and_then(|l| l.creds.oauth.refresh_token_expires_at)
                    .map(|t| (t - now) / DAY_MS);
            }
            Decision::NeedsLogin => {
                detail = Some(format!(
                    "run `ccred switch {name}`, then `claude auth login`, then `ccred save {name}`"
                ));
            }
            Decision::Broken => detail = Some("no usable credentials stored".into()),
            _ => {}
        }

        results.push(ProfileResult {
            name: name.as_str().to_string(),
            decision,
            detail,
            window_days_before: window_before.map(|ms| ms / DAY_MS),
            window_days_after: window_after,
        });
    }

    let report = RefreshReport {
        status: "ran".to_string(),
        profiles: results,
    };
    write_last_run(ctx, now, &report)?;
    Ok(report)
}

/// Refresh one idle profile by running Claude Code against its own store.
///
/// Success is judged by an observed change in the credential store, never by
/// an exit code. Exit codes lie; a refresh window that moved forward does not.
/// The address this profile is recorded as belonging to, when one is known.
///
/// `None` means the profile predates identity capture or the account was never
/// resolved. An unknown identity must never be treated as a mismatch -- the
/// same rule the switch gate follows, and for the same reason.
fn expected_email(ctx: &Ctx, name: &ProfileName) -> crate::Result<Option<String>> {
    Ok(ctx.repo().meta(name)?.and_then(|m| m.account.email))
}

/// Put a profile back if the spawned `claude` emptied it.
///
/// Returns `None` when there is nothing to do, and `Some(restored)` when the
/// store went from usable to unusable -- `restored` saying whether writing the
/// good copy back worked.
///
/// Restoring a token that later turns out to be dead costs nothing, because
/// `validate` will say so on the next run. Not restoring costs the account.
fn restore_if_cleared(
    store: &dyn CredentialStore,
    pre: Option<&crate::store::Loaded>,
    pre_was_usable: bool,
    now: i64,
) -> Option<bool> {
    if !pre_was_usable {
        return None;
    }
    let good = pre?;
    let still_usable = store
        .load()
        .ok()
        .flatten()
        .is_some_and(|l| validate_credentials(&l.creds.oauth, now).is_ok());
    if still_usable {
        return None;
    }
    Some(store.replace(&good.creds).is_ok())
}

fn refresh_one(
    ctx: &Ctx,
    name: &ProfileName,
    cli: &ClaudeCli,
    state: &crate::profile::RefreshState,
    policy: &RefreshPolicy,
    now: i64,
) -> crate::Result<(Decision, Option<String>)> {
    let dir = ctx.paths().profile_dir(name)?;
    let scope = scope_for(&dir, false);
    let store = ctx.repo().store(name)?;

    // Keep the credentials as they stand before anything is spawned.
    //
    // Everything else in this tool guards what *we* write. This is the one
    // place a third party writes into a profile: `claude` is pointed at the
    // profile's own store and may do whatever it likes there, including
    // clearing it. That is not hypothetical -- a real run on a real machine
    // emptied a profile's tokens outright, and only a last-known-good copy
    // made it recoverable.
    let pre = store.load()?;
    let pre_was_usable = pre
        .as_ref()
        .is_some_and(|l| validate_credentials(&l.creds.oauth, now).is_ok());
    let before = pre
        .as_ref()
        .and_then(|l| l.creds.oauth.refresh_token_expires_at);

    // Try the rung that worked last time first, then the rest of the ladder.
    let mut ladder: Vec<Probe> = Vec::new();
    if let Some(remembered) = state.working_probe.as_deref()
        && let Some(p) = Probe::LADDER
            .iter()
            .find(|p| format!("{p:?}").to_lowercase() == remembered)
    {
        ladder.push(*p);
    }
    for p in Probe::LADDER {
        if !ladder.contains(&p) {
            ladder.push(p);
        }
    }

    let mut last_note = String::new();
    for probe in ladder {
        let outcome = cli.run(&scope, probe, policy.spawn_timeout)?;

        // Did the probe leave the profile worse than it found it?
        if let Some(restored) = restore_if_cleared(&store, pre.as_ref(), pre_was_usable, now) {
            ctx.repo().update_meta(name, |m| {
                m.refresh.needs_login = true;
                m.refresh.last_attempt_ms = Some(now);
            })?;
            return Ok((
                Decision::Broken,
                Some(format!(
                    "claude cleared this profile's credentials; {}. Run                      `claude auth login` and `ccred save {name}`",
                    if restored {
                        "the previous copy was put back"
                    } else {
                        "restoring the previous copy FAILED, see the backups directory"
                    }
                )),
            ));
        }

        if outcome.needs_login() {
            ctx.repo().update_meta(name, |m| {
                m.refresh.needs_login = true;
                m.refresh.last_attempt_ms = Some(now);
                m.refresh.next_attempt_after_ms = None;
            })?;
            return Ok((
                Decision::NeedsLogin,
                Some("Claude Code reports this account is signed out".into()),
            ));
        }

        // Did the environment actually take effect?
        //
        // The identity Claude Code reports is the direct evidence: if our
        // credential directory was honoured, the account it authenticated as
        // is this profile's. `projectsDirectory` cannot answer this -- it
        // follows `CLAUDE_CONFIG_DIR`, which we deliberately do not set, so it
        // reports the shared configuration either way.
        //
        // Without this check a build that ignored the variable would refresh
        // the default account, and we would have no way to tell that from a
        // profile that simply had nothing to do.
        if probe == Probe::AuthStatus
            && outcome.succeeded()
            && let Ok(status) =
                serde_json::from_str::<crate::claude_cli::AuthStatus>(outcome.stdout.trim())
            && let Some(reported) = status.email.as_deref()
            && let Some(expected) = expected_email(ctx, name)?
            && !reported.eq_ignore_ascii_case(&expected)
        {
            return Ok((
                Decision::Broken,
                Some(format!(
                    "claude ignored the credential directory: it authenticated as \
                     {reported}, not this profile's account"
                )),
            ));
        }

        let after = store
            .load()?
            .and_then(|l| l.creds.oauth.refresh_token_expires_at);

        if after > before {
            ctx.repo().update_meta(name, |m| {
                m.refresh.last_attempt_ms = Some(now);
                m.refresh.last_success_ms = Some(now);
                m.refresh.consecutive_failures = 0;
                m.refresh.next_attempt_after_ms = None;
                m.refresh.working_probe = Some(format!("{probe:?}").to_lowercase());
            })?;
            return Ok((Decision::Refresh, Some(format!("refreshed via {probe:?}"))));
        }

        last_note = if outcome.timed_out {
            format!("{probe:?} timed out")
        } else {
            format!("{probe:?} did not move the refresh window")
        };
    }

    // Nothing on the ladder worked. Back off rather than hammering: an
    // unrecognised refresher that keeps retrying is how accounts get blocked.
    let failures = state.consecutive_failures.saturating_add(1);
    let next = now + backoff_ms(failures);
    ctx.repo().update_meta(name, |m| {
        m.refresh.last_attempt_ms = Some(now);
        m.refresh.consecutive_failures = failures;
        m.refresh.next_attempt_after_ms = Some(next);
    })?;
    Ok((Decision::SkipBackoff, Some(last_note)))
}

fn read_last_run(ctx: &Ctx) -> crate::Result<Option<LastRun>> {
    let path = ctx.paths().last_run();
    match std::fs::read(&path) {
        Ok(raw) => Ok(serde_json::from_slice(&raw).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CcredError::Io { path, source }),
    }
}

fn write_last_run(ctx: &Ctx, now: i64, report: &RefreshReport) -> crate::Result<()> {
    let record = LastRun {
        finished_at_ms: now,
        status: report.status.clone(),
        needed_attention: report.needs_attention(),
    };
    let bytes = serde_json::to_vec_pretty(&record).map_err(|source| CcredError::Json {
        path: ctx.paths().last_run(),
        source,
    })?;
    write_atomic(&ctx.paths().last_run(), &bytes, true)
}

/// Exposed so `doctor` can describe the scope a profile would be refreshed in.
pub fn scope_of(ctx: &Ctx, name: &ProfileName) -> crate::Result<StorageScope> {
    Ok(scope_for(&ctx.paths().profile_dir(name)?, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::RefreshState;

    const NOW: i64 = 1_788_000_000_000;

    fn policy() -> RefreshPolicy {
        RefreshPolicy::default()
    }

    #[test]
    fn the_active_profile_is_mirrored_not_spawned() {
        // Claude Code already refreshes the live store; spawning for it would
        // be pure waste.
        let d = decide(
            true,
            Some(2 * DAY_MS),
            false,
            true,
            &RefreshState::default(),
            NOW,
            &policy(),
        );
        assert_eq!(d, Decision::MirrorActive);
    }

    #[test]
    fn a_healthy_idle_profile_is_left_alone() {
        let d = decide(
            false,
            Some(20 * DAY_MS),
            false,
            true,
            &RefreshState::default(),
            NOW,
            &policy(),
        );
        assert_eq!(d, Decision::SkipFresh);
    }

    #[test]
    fn a_profile_nearing_expiry_is_refreshed() {
        let d = decide(
            false,
            Some(3 * DAY_MS),
            false,
            true,
            &RefreshState::default(),
            NOW,
            &policy(),
        );
        assert_eq!(d, Decision::Refresh);
    }

    #[test]
    fn an_expired_profile_asks_for_a_human() {
        // Spawning cannot help once the refresh token is gone.
        let d = decide(
            false,
            Some(-DAY_MS),
            true,
            true,
            &RefreshState::default(),
            NOW,
            &policy(),
        );
        assert_eq!(d, Decision::NeedsLogin);
    }

    #[test]
    fn the_needs_login_latch_stops_repeated_attempts() {
        let state = RefreshState {
            needs_login: true,
            ..Default::default()
        };
        let d = decide(false, Some(3 * DAY_MS), false, true, &state, NOW, &policy());
        assert_eq!(d, Decision::NeedsLogin);
    }

    #[test]
    fn backoff_is_respected() {
        let state = RefreshState {
            next_attempt_after_ms: Some(NOW + 3_600_000),
            ..Default::default()
        };
        let d = decide(false, Some(DAY_MS), false, true, &state, NOW, &policy());
        assert_eq!(d, Decision::SkipBackoff);
    }

    #[test]
    fn a_recent_attempt_is_not_repeated() {
        let state = RefreshState {
            last_attempt_ms: Some(NOW - 3_600_000),
            ..Default::default()
        };
        let d = decide(false, Some(DAY_MS), false, true, &state, NOW, &policy());
        assert_eq!(d, Decision::SkipBackoff);
    }

    #[test]
    fn unusable_credentials_are_reported_not_refreshed() {
        let d = decide(
            false,
            None,
            false,
            false,
            &RefreshState::default(),
            NOW,
            &policy(),
        );
        assert_eq!(d, Decision::Broken);
    }

    #[test]
    fn an_unknown_expiry_is_left_alone() {
        let d = decide(
            false,
            None,
            false,
            true,
            &RefreshState::default(),
            NOW,
            &policy(),
        );
        assert_eq!(d, Decision::SkipFresh);
    }

    #[test]
    fn backoff_grows_then_stops_growing() {
        assert!(backoff_ms(1) < backoff_ms(2));
        assert!(backoff_ms(2) < backoff_ms(3));
        assert_eq!(backoff_ms(20), DAY_MS, "must not grow without bound");
        assert!(backoff_ms(1) >= 30 * 60 * 1000);
    }

    #[test]
    fn a_report_flags_what_needs_a_person() {
        let report = RefreshReport {
            status: "ran".into(),
            profiles: vec![ProfileResult {
                name: "work".into(),
                decision: Decision::NeedsLogin,
                detail: None,
                window_days_before: None,
                window_days_after: None,
            }],
        };
        assert!(report.needs_attention());

        let quiet = RefreshReport {
            status: "ran".into(),
            profiles: vec![ProfileResult {
                name: "work".into(),
                decision: Decision::SkipFresh,
                detail: None,
                window_days_before: Some(20),
                window_days_after: None,
            }],
        };
        assert!(!quiet.needs_attention());
    }

    /// This happened on a real machine, not in theory.
    ///
    /// A scheduled refresh spawned `claude`, which decided it was signed out
    /// and wrote an empty credential blob over the profile's own store. Every
    /// safety gate in this tool guards what *we* write; that write was not
    /// ours, so nothing stopped it. The profile survived only because a
    /// last-known-good copy happened to exist.
    #[test]
    fn a_probe_that_empties_the_profile_has_its_damage_undone() {
        use crate::store::CredentialStore;
        use crate::store::file::FileStore;

        let dir = tempfile::TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

        let good: crate::model::CredentialsFile = serde_json::from_str(
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
               "refreshToken":"sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
               "expiresAt":1788003600000,"refreshTokenExpiresAt":1790000000000,
               "scopes":["user:inference"]}}"#,
        )
        .unwrap();
        store.replace(&good).unwrap();
        let pre = store.load().unwrap();
        assert!(pre.is_some());

        // What the spawned binary did: a logged-out blob, straight over the top.
        std::fs::write(
            dir.path().join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,
               "refreshTokenExpiresAt":0,"scopes":["user:inference"]}}"#,
        )
        .unwrap();

        let restored = restore_if_cleared(&store, pre.as_ref(), true, NOW);
        assert_eq!(restored, Some(true), "the wipe must be detected and undone");

        let after = store.load().unwrap().expect("credentials must be back");
        assert!(
            validate_credentials(&after.creds.oauth, NOW).is_ok(),
            "the restored profile must be usable again"
        );
    }

    #[test]
    fn a_probe_that_changes_nothing_is_left_alone() {
        use crate::store::CredentialStore;
        use crate::store::file::FileStore;

        let dir = tempfile::TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());
        let good: crate::model::CredentialsFile = serde_json::from_str(
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
               "refreshToken":"sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
               "expiresAt":1788003600000,"refreshTokenExpiresAt":1790000000000,
               "scopes":["user:inference"]}}"#,
        )
        .unwrap();
        store.replace(&good).unwrap();
        let pre = store.load().unwrap();

        assert_eq!(restore_if_cleared(&store, pre.as_ref(), true, NOW), None);
    }

    /// A profile that was already unusable cannot be made worse, and writing
    /// a dead blob back over whatever is there now would be pure noise.
    #[test]
    fn an_already_broken_profile_is_not_restored_over() {
        use crate::store::file::FileStore;
        let dir = tempfile::TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());
        assert_eq!(restore_if_cleared(&store, None, false, NOW), None);
    }

    /// `--force` exists for the person who has just fixed whatever was broken
    /// and wants to watch it work, rather than waiting out a backoff measured
    /// in hours or a window threshold measured in days.
    #[test]
    fn force_clears_the_timing_gates_but_not_the_need_for_a_human() {
        let backed_off = RefreshState {
            next_attempt_after_ms: Some(NOW + DAY_MS),
            last_attempt_ms: Some(NOW),
            consecutive_failures: 3,
            ..Default::default()
        };
        // Without force: the backoff wins.
        assert_eq!(
            decide(
                false,
                Some(20 * DAY_MS),
                false,
                true,
                &backed_off,
                NOW,
                &policy()
            ),
            Decision::SkipBackoff
        );
        // With force, the caller has already cleared the timing state and
        // widened the window, which is what `refresh` does for --force.
        let cleared = RefreshState {
            next_attempt_after_ms: None,
            last_attempt_ms: None,
            ..backed_off.clone()
        };
        let wide = RefreshPolicy {
            window_below_ms: i64::MAX / 4,
            min_interval_ms: 0,
            ..policy()
        };
        assert_eq!(
            decide(false, Some(20 * DAY_MS), false, true, &cleared, NOW, &wide),
            Decision::Refresh
        );

        // But a latched needs_login still wins: forcing a spawn there would
        // start a process that cannot possibly succeed.
        let logged_out = RefreshState {
            needs_login: true,
            ..cleared
        };
        assert_eq!(
            decide(
                false,
                Some(20 * DAY_MS),
                false,
                true,
                &logged_out,
                NOW,
                &wide
            ),
            Decision::NeedsLogin
        );
    }
}
