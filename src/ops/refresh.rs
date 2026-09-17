//! Keeping profiles alive.
//!
//! An account nobody uses for a few weeks goes stale and needs a human to log
//! in again. This is the loop that keeps its tokens rotating, and it is
//! deliberately timid.
//!
//! # What it can and cannot do
//!
//! Measured against a live account: a probe rotates both tokens and renews the
//! access token by eight hours, while `refreshTokenExpiresAt` moves by under a
//! second. That deadline is a ceiling fixed at login and inherited by
//! every rotated token, so no amount of refreshing postpones it -- when it
//! arrives, only a new login will do.
//!
//! Success is therefore an access token that expires later than it did. It
//! used to be the refresh window moving, which is a thing that does not
//! happen, so every real exchange was recorded as a failure and earned a
//! backoff.
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

/// Start saying so this many days before the refresh deadline.
///
/// The deadline cannot be moved, so the warning is the whole remedy: it has
/// to arrive with enough time for someone to notice a scheduled run and log
/// in. Two scheduled runs fit inside five days.
const EXPIRY_WARN_DAYS: i64 = 5;

/// How long to wait for the credential store lock.
///
/// Shorter than the switch timeout on purpose: this runs unattended, so
/// giving up and reporting "busy" beats holding a scheduler job open.
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// When to act, and how hard to try.
#[derive(Debug, Clone)]
pub struct RefreshPolicy {
    /// Start exchanging tokens once the refresh window drops below this.
    ///
    /// Not because an exchange extends the window -- it does not; the
    /// deadline is fixed at login -- but because a profile nobody uses stops
    /// having its tokens rotated at all, and ten days is when that begins to
    /// matter. Earlier than that it would only multiply calls.
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
    /// Whether that run hit something a retry might fix.
    ///
    /// The rate limit exists so a scheduler that double-fires costs nothing,
    /// and a transient failure should not buy two days of silence. But only
    /// *transient* trouble may bypass it: keying this on "needs attention"
    /// included a spent refresh token and an approaching deadline, neither of
    /// which a retry can help, so the limit stayed off for as long as they
    /// lasted -- permanently, on a machine with no `claude` installed.
    ///
    /// The name is kept for the on-disk shape. Old records without the field
    /// read as clean, so an upgrade does not re-run everything at once.
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
    /// The access token has not expired yet, so there is nothing to exchange.
    ///
    /// Claude Code performs a token exchange only when it needs a new access
    /// token. Spawning against a profile whose access token is still live
    /// renews nothing, and since success is judged by the access token
    /// renewing, it would be recorded as a failure and earn a backoff.
    SkipAccessLive,
    /// The refresh deadline is close and nothing can postpone it.
    ///
    /// That deadline is fixed at login and inherited by every rotated token,
    /// so there is no version of "try harder" that helps. Warning ahead of it
    /// is the only thing a schedule can usefully do about it -- which makes
    /// this the most valuable thing an unattended run reports.
    ExpiringSoon,
    /// The refresh token is gone; only a person can fix this.
    NeedsLogin,
    Broken,
    /// Something only a person can settle, and that trying again will not
    /// change: the live login is another account's, an interrupted switch
    /// could not be read, a profile holds the tokens that are live.
    ///
    /// Kept apart from `Broken` because `Broken` lifts the over-fire rate
    /// limit, which is right for trouble a retry might clear and wrong for
    /// this: a mirror refused for as long as another account stays logged in
    /// turned every scheduler firing into a full run.
    Blocked,
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
        self.profiles.iter().any(|p| {
            matches!(
                p.decision,
                Decision::NeedsLogin
                    | Decision::Broken
                    | Decision::Blocked
                    | Decision::ExpiringSoon
            )
        })
    }

    /// Did anything happen that trying again might fix?
    ///
    /// Distinct from `needs_attention`, and the distinction matters: that one
    /// includes states a retry cannot help -- a spent refresh token, a
    /// deadline five days out -- and using it to bypass the over-fire rate
    /// limit disabled the limit for as long as those states persisted, which
    /// is to say permanently. On a machine with no `claude` installed, every
    /// run was `Broken` and the idempotency this module promises was void
    /// from the first one.
    pub fn worth_retrying_sooner(&self) -> bool {
        self.profiles
            .iter()
            .any(|p| matches!(p.decision, Decision::Broken))
    }
}

/// How one profile's stored tokens stand against the clock.
///
/// Grouped rather than passed as four bare booleans: `decide(false, true,
/// false, true, ...)` is unreadable at the call site and easy to transpose.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokenState {
    /// Time left on the refresh window, when the credentials state one.
    pub window_left_ms: Option<i64>,
    /// The refresh token itself is past its stated expiry.
    pub refresh_expired: bool,
    /// The access token is past its stated expiry, so a probe would cause
    /// Claude Code to exchange tokens. Without that, a probe renews nothing.
    pub access_expired: bool,
    /// The credentials parse and pass validation at all.
    pub usable: bool,
}

/// The self-rate-limit that makes over-firing harmless.
///
/// `--force` is a person asking directly, so it outranks a limit meant for
/// schedulers, and a run that needed attention is not allowed to buy two days
/// of silence.
fn rate_limited(ctx: &Ctx, opts: &RefreshOptions, now: i64) -> bool {
    !opts.force
        && opts.if_older_than_ms.is_some_and(|window| {
            read_last_run(ctx).ok().flatten().is_some_and(|last| {
                // A run recorded in the future is a clock that was wrong, and
                // obeying it would silence the schedule until that moment
                // arrives. See `decide` for the same reasoning.
                last.finished_at_ms <= now + CLOCK_SKEW_MS
                    && now - last.finished_at_ms < window
                    && !last.needed_attention
            })
        })
}

/// What the stored credentials say about themselves. Unusable or absent
/// credentials come back as the default, which `decide` reads as `Broken`.
fn token_state(loaded: Option<&crate::store::Loaded>, now: i64) -> TokenState {
    match loaded {
        Some(l) => match validate_credentials(&l.creds.oauth, now) {
            Ok(h) => TokenState {
                window_left_ms: h.refresh_window_left_ms,
                refresh_expired: h.refresh_expired,
                access_expired: h.access_expired,
                usable: true,
            },
            Err(_) => TokenState::default(),
        },
        None => TokenState::default(),
    }
}

/// What a refresh would do, without spawning anything or writing anything.
///
/// The decision phase and nothing else: no lock is taken, for the same
/// reason `list` takes none -- a preview must not queue behind the run it is
/// previewing. It therefore reports what is true at this moment, which is
/// what a preview is.
pub fn preview(ctx: &Ctx, opts: &RefreshOptions) -> crate::Result<RefreshReport> {
    let now = now_ms();
    // Including the gate the scheduler's own invocation meets. Without it the
    // preview of `refresh --if-older-than 48` showed a run that the real
    // command would not have made -- which is the one thing a preview must
    // not do.
    if rate_limited(ctx, opts, now) {
        return Ok(RefreshReport::skipped("last run was recent"));
    }
    let active = ctx.repo().active().unwrap_or(None);
    let mut profiles = Vec::new();

    for name in ctx.repo().list()? {
        let is_active = active.as_ref() == Some(&name);
        let loaded = ctx
            .repo()
            .store(&name)
            .ok()
            .and_then(|s| s.load().ok().flatten());
        let tokens = token_state(loaded.as_ref(), now);
        let window_before = tokens.window_left_ms;
        let state = ctx
            .repo()
            .meta(&name)
            .ok()
            .flatten()
            .map(|m| m.refresh)
            .unwrap_or_default();
        let (state, tokens, policy) = if opts.force {
            forced(state, tokens, &opts.policy)
        } else {
            (state, tokens, opts.policy.clone())
        };
        profiles.push(ProfileResult {
            name: name.as_str().to_string(),
            decision: decide(is_active, tokens, &state, now, &policy),
            detail: None,
            window_days_before: window_before.map(|ms| ms / DAY_MS),
            window_days_after: None,
        });
    }

    Ok(RefreshReport {
        status: "dry run".to_string(),
        profiles,
    })
}

/// Decide what to do with one profile. Pure, so the policy is testable.
pub fn decide(
    is_active: bool,
    tokens: TokenState,
    state: &crate::profile::RefreshState,
    now: i64,
    policy: &RefreshPolicy,
) -> Decision {
    let TokenState {
        window_left_ms,
        refresh_expired,
        access_expired,
        usable: credentials_usable,
    } = tokens;
    if is_active {
        return Decision::MirrorActive;
    }
    if !credentials_usable {
        return Decision::Broken;
    }
    if refresh_expired || state.needs_login {
        return Decision::NeedsLogin;
    }
    // The warning replaces only the *quiet* outcomes, never the work.
    //
    // Returning it unconditionally made a profile inside five days
    // unrefreshable -- the one a person is most likely to reach for `--force`
    // over -- because `Refresh` sat below it and could never be reached. The
    // deadline cannot be moved, but the access token still needs renewing,
    // and both things can be true at once: refresh if there is an exchange to
    // make, and say so either way.
    let expiring = window_left_ms.is_some_and(|left| left < EXPIRY_WARN_DAYS * DAY_MS);
    if expiring && !access_expired {
        // Nothing to exchange, so there is no work to displace.
        return Decision::ExpiringSoon;
    }
    // Timestamps from the future are discarded rather than obeyed.
    //
    // Both of these were written by an earlier run, and a machine whose clock
    // was wrong then -- a dead CMOS battery, a restored VM, a dual boot --
    // writes a moment that has not arrived yet. Obeyed literally, that parks
    // the profile in "backing off" until the date passes, which is silent and
    // can be years. The furthest either can legitimately be ahead of now is
    // one backoff, so anything beyond that is a clock, not a decision.
    let plausible = now + MAX_BACKOFF_MS + CLOCK_SKEW_MS;
    let next_after = state
        .next_attempt_after_ms
        .filter(|&after| after <= plausible);
    let last_attempt = state
        .last_attempt_ms
        .filter(|&last| last <= now + CLOCK_SKEW_MS);
    let backed_off = next_after.is_some_and(|after| now < after)
        || last_attempt.is_some_and(|last| now - last < policy.min_interval_ms);

    let decision = if backed_off {
        Decision::SkipBackoff
    } else {
        match window_left_ms {
            Some(left) if left > policy.window_below_ms => Decision::SkipFresh,
            // No stated expiry means we cannot tell how urgent it is; leave
            // it be rather than refreshing something that may not need it.
            None => Decision::SkipFresh,
            // The window is low enough to act on, but acting only works once the
            // access token has expired. An idle profile's access token is expired
            // almost all of the time -- eight hours against a schedule measured in
            // days -- so this is a short wait, not a dead end.
            Some(_) if !access_expired => Decision::SkipAccessLive,
            Some(_) => Decision::Refresh,
        }
    };

    // A backoff or a recent attempt would otherwise silence the deadline, and
    // that is the one thing about this profile worth saying.
    match decision {
        Decision::SkipBackoff | Decision::SkipFresh | Decision::SkipCap if expiring => {
            Decision::ExpiringSoon
        }
        other => other,
    }
}

/// What `--force` changes before a profile is decided.
///
/// Only the timing gates: the backoff, the minimum interval, the window
/// threshold, and the skip for a live access token -- a forced run backdates
/// that token so an exchange happens. `needs_login` stays: a person is
/// genuinely required there, and pretending otherwise would spawn a process
/// that cannot succeed.
fn forced(
    mut state: crate::profile::RefreshState,
    tokens: TokenState,
    policy: &RefreshPolicy,
) -> (crate::profile::RefreshState, TokenState, RefreshPolicy) {
    state.next_attempt_after_ms = None;
    state.last_attempt_ms = None;
    (
        state,
        TokenState {
            access_expired: true,
            ..tokens
        },
        RefreshPolicy {
            // Everything below this is "refresh now"; i64::MAX would overflow
            // the comparison, so use a century.
            window_below_ms: i64::MAX / 4,
            min_interval_ms: 0,
            ..policy.clone()
        },
    )
}

/// Did a probe renew the access token?
///
/// Judged by the access token's expiry, never by the refresh window: the
/// window is a ceiling fixed at login, so it does not move on a successful
/// exchange. Both readings must exist -- `Some(0) > None` is true, so a store
/// that vanished and came back holding a logged-out blob would otherwise
/// count as a renewal.
fn renewed(before: Option<i64>, after: Option<i64>) -> bool {
    matches!((before, after), (Some(b), Some(a)) if a > b)
}

/// How long to leave a profile alone after it failed this many times in a
/// row: a day, then two, then four.
///
/// Measured in days because `min_interval_ms` already spaces attempts a day
/// apart. The earlier schedule started at half an hour and topped out at one
/// day, so it never once delayed anything the interval had not -- a profile
/// that failed every time was retried as often as a healthy one. The cap
/// keeps a persistent failure from going quiet for good: with a twice-weekly
/// schedule it is still tried about once a week.
/// The longest `backoff_ms` can return, so the longest a `next_attempt_after`
/// written by this program can sit ahead of the run that wrote it.
const MAX_BACKOFF_MS: i64 = 4 * DAY_MS;

/// Slack for clocks that disagree slightly -- NTP stepping mid-run, a file
/// system timestamp rounded up. Small enough that a wrong year is still
/// caught, large enough that ordinary skew is not called a wrong year.
const CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;

fn backoff_ms(consecutive_failures: u32) -> i64 {
    DAY_MS << consecutive_failures.saturating_sub(1).min(2)
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

    if rate_limited(ctx, opts, now) {
        return Ok(skipped_and_logged(ctx, opts, now, "last run was recent"));
    }

    // Settle a switch that died part-way before deciding which profile is
    // active. Left alone, the profile it was moving to counts as idle -- and
    // refreshing its stored copy, which holds the very token that is now
    // live, rotates that token away from under the live session.
    if !matches!(
        crate::journal::SwitchJournal::load(&ctx.paths().switch_journal()),
        Ok(None)
    ) {
        let profiles = ctx.lock_profiles(PROFILES_LOCK_TIMEOUT);
        let live = ctx.live_store();
        match (profiles, live.lock(LOCK_TIMEOUT)) {
            (Ok(_profiles), Ok(_live)) => {
                // An unreadable journal is set aside and marked unsettled;
                // the mirror below then refuses, and says why. Stopping the
                // run here instead left no log entry and no last-run record
                // -- nothing a person could find later.
                let _ = super::switch::recover_locked(ctx, &mut Vec::new());
            }
            (Err(CcredError::Busy(_)), _) | (_, Err(CcredError::Busy(_))) => {
                return Ok(skipped_and_logged(
                    ctx,
                    opts,
                    now,
                    "a switch is in progress",
                ));
            }
            (Err(e), _) | (_, Err(e)) => return Err(e),
        }
    }

    let mut results = Vec::new();
    let mut spawned = 0usize;
    // Set when `claude` could not be found, so the run can exit with
    // "this machine is not set up" rather than "your credentials are unsafe".
    let mut missing_claude = false;

    // Resolved lazily: a machine with nothing to refresh should not fail just
    // because `claude` is not installed.
    let mut cli: Option<ClaudeCli> = None;

    for name in ctx.repo().list()? {
        // One profile at a time under the profiles lock, with the active
        // profile read under it. Read once for the whole run, it went stale
        // whenever someone switched during a probe, and the profile they had
        // just made live was then refreshed through its own store -- rotating
        // away the token the live session held.
        let _profiles = match ctx.lock_profiles(PROFILES_LOCK_TIMEOUT) {
            Ok(guard) => guard,
            Err(e) => {
                results.push(ProfileResult {
                    name: name.as_str().to_string(),
                    decision: Decision::SkipBackoff,
                    detail: Some(format!("left for the next run: {e}")),
                    window_days_before: None,
                    window_days_after: None,
                });
                continue;
            }
        };
        let active = ctx.repo().active()?;
        let is_active = active.as_ref() == Some(&name);
        let store = ctx.repo().store(&name)?;
        let loaded = store.load().ok().flatten();

        let tokens = token_state(loaded.as_ref(), now);
        let window_before = tokens.window_left_ms;

        let meta = ctx.repo().meta(&name)?;
        let state = meta.map(|m| m.refresh).unwrap_or_default();
        let (state, tokens, effective_policy) = if opts.force {
            forced(state, tokens, &opts.policy)
        } else {
            (state, tokens, opts.policy.clone())
        };
        let mut decision = decide(is_active, tokens, &state, now, &effective_policy);

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
                    // A switch that died part-way leaves the live tokens and
                    // the account `.claude.json` names disagreeing, and the
                    // pointer naming the profile it was leaving. Mirroring
                    // then copies one account's tokens into the other's
                    // profile. Settle it first, and copy nothing this run:
                    // what counts as "active" was decided before.
                    Ok(_guard) => match super::switch::recover_locked(ctx, &mut Vec::new()) {
                        Ok(Some(settled)) => {
                            decision = Decision::SkipBackoff;
                            detail = Some(format!("{settled}; copied on the next run"));
                        }
                        Err(e) => {
                            decision = Decision::Blocked;
                            detail = Some(format!("not mirrored: {e}"));
                        }
                        Ok(None)
                            if crate::journal::SwitchJournal::unsettled(
                                &ctx.paths().unsettled_switch(),
                            )
                            .is_some() =>
                        {
                            decision = Decision::Blocked;
                            detail = Some(
                                concat!(
                                    "not mirrored: an interrupted switch could not be read. ",
                                    "Check `ccred current`, then `ccred save` the account it shows"
                                )
                                .into(),
                            );
                        }
                        Ok(None) => {
                            let account = ctx.live_account();
                            match ctx.repo().save_from(&name, &live, &account) {
                                Ok(outcome) => detail = Some(format!("{outcome:?}").to_lowercase()),
                                // Reported as "mirrored" with the reason in small
                                // print, and exit 0, this hid exactly the failure
                                // that loses tokens: the live pair renewed, the
                                // profile still holding the rotated-away one.
                                Err(e) => {
                                    decision = Decision::Blocked;
                                    detail = Some(format!("not mirrored: {e}"));
                                }
                            }
                        }
                    },
                    // Claude Code writing at this moment: the next run
                    // copies it, so nobody needs to act.
                    Err(e) => {
                        decision = Decision::SkipBackoff;
                        detail = Some(format!("not mirrored, store is busy: {e}"));
                    }
                }
            }
            // Whatever the pointer says, a profile holding the very tokens
            // that are live is the live login's copy: exchanging its token
            // retires the one the running session uses.
            Decision::Refresh if holds_the_live_tokens(ctx, loaded.as_ref()) => {
                decision = Decision::Blocked;
                detail = Some(
                    concat!(
                        "not refreshed: it holds the tokens that are live right now, ",
                        "and refreshing this copy would sign the live session out. ",
                        "`ccred current` says which profile is active"
                    )
                    .into(),
                );
            }
            Decision::Refresh => {
                spawned += 1;
                // A missing `claude` is reported per profile, not thrown out
                // of the whole run. Only the profiles that need a spawn are
                // affected; mirroring the active one does not use the binary
                // at all, and losing that work as well would be gratuitous.
                if cli.is_none() {
                    match ClaudeCli::discover(opts.claude_path.as_deref()) {
                        Ok(found) => {
                            cli =
                                Some(found.with_config_dir(
                                    ctx.paths().overrides().claude_config_dir.clone(),
                                ))
                        }
                        Err(e) => {
                            missing_claude = true;
                            // Same reasoning as above: without an attempt
                            // recorded, a machine with no `claude` retries
                            // every profile on every firing for ever.
                            let failures = state.consecutive_failures.saturating_add(1);
                            let next = now + backoff_ms(failures);
                            let _ = ctx.repo().update_meta(&name, |m| {
                                m.refresh.last_attempt_ms = Some(now);
                                m.refresh.consecutive_failures = failures;
                                m.refresh.next_attempt_after_ms = Some(next);
                            });
                            results.push(ProfileResult {
                                name: name.as_str().to_string(),
                                decision: Decision::Broken,
                                detail: Some(e.to_string()),
                                window_days_before: window_before.map(|ms| ms / DAY_MS),
                                window_days_after: None,
                            });
                            continue;
                        }
                    }
                }
                // The branch above either set this or moved on, so the
                // else is unreachable -- written as a contained failure
                // rather than a panic because this runs unattended, where a
                // panic is a scheduler slot that reports nothing.
                let Some(cli) = cli.as_ref() else {
                    results.push(ProfileResult {
                        name: name.as_str().to_string(),
                        decision: Decision::Broken,
                        detail: Some("no `claude` was resolved for this run".into()),
                        window_days_before: window_before.map(|ms| ms / DAY_MS),
                        window_days_after: None,
                    });
                    continue;
                };
                // Contained to this profile. A store that will not load, a
                // spawn that will not start, a metadata write that fails --
                // any of them used to end the whole run, taking the other
                // profiles' work with it. An unattended job that gives up on
                // everything because one account is broken is worse than one
                // that reports the broken account.
                match refresh_one(ctx, &name, cli, &state, &effective_policy, now, opts.force) {
                    Ok((d, note)) => {
                        decision = d;
                        detail = note;
                    }
                    Err(e) => {
                        // Record the attempt, or there is no backoff: a
                        // machine where the spawn always fails would retry
                        // `max_spawns` profiles at the full timeout on every
                        // firing, for ever.
                        let failures = state.consecutive_failures.saturating_add(1);
                        let next = now + backoff_ms(failures);
                        let _ = ctx.repo().update_meta(&name, |m| {
                            m.refresh.last_attempt_ms = Some(now);
                            m.refresh.consecutive_failures = failures;
                            m.refresh.next_attempt_after_ms = Some(next);
                        });
                        decision = Decision::Broken;
                        detail = Some(e.to_string());
                    }
                }
                window_after = store
                    .load()
                    .ok()
                    .flatten()
                    .and_then(|l| l.creds.oauth.refresh_token_expires_at)
                    .map(|t| t.saturating_sub(now) / DAY_MS);
            }
            Decision::SkipAccessLive => {
                detail =
                    Some("the access token has not expired yet; nothing would be exchanged".into());
            }
            Decision::ExpiringSoon => {
                let days = window_before.map(|ms| ms / DAY_MS).unwrap_or(0);
                detail = Some(format!(
                    concat!(
                        "log in again within {} days -- this deadline is fixed at ",
                        "login and refreshing cannot move it"
                    ),
                    days
                ));
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
        // The status is what `doctor` and a scheduler read back, so say which
        // kind of "ran" this was.
        status: if missing_claude {
            "ran, but claude was not found".to_string()
        } else {
            "ran".to_string()
        },
        profiles: results,
    };
    // The finish, not the `now` the run started with: a run that spawned
    // four probes can take minutes, and the record says when it ended.
    write_last_run(ctx, crate::store::now_ms(), &report)?;

    // A scheduled run is otherwise invisible on Windows, where Task Scheduler
    // discards stdout. Failing to write the record must never fail the run it
    // was recording.
    // Only the registered job passes `--if-older-than`; a person who types
    // `ccred refresh` asked for the work and gets it unconditionally.
    let scheduled = opts.if_older_than_ms.is_some();
    let _ = crate::logbook::append(&ctx.paths().log_dir(), &log_entry(now, &report, scheduled));

    Ok(report)
}

/// How long a refresh waits for another `ccred` command to finish with the
/// profiles before leaving a profile for the next run. Short: a switch or a
/// save holds them for a moment, and an unattended run has no reason to wait
/// longer than that.
const PROFILES_LOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// Does this stored copy hold the refresh token that is live?
fn holds_the_live_tokens(ctx: &Ctx, stored: Option<&crate::store::Loaded>) -> bool {
    let Some(stored) = stored else {
        return false;
    };
    matches!(
        crate::store::load_unlocked(&ctx.live_store()),
        Ok(Some(live)) if live.creds.oauth.refresh_token == stored.creds.oauth.refresh_token
    )
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
    // An error is NOT "the store was cleared". `FileStore::load` is explicit
    // that a read failure must never be reported as absence, because a caller
    // that believes a file is gone will overwrite it -- and `replace` skips
    // the monotonic window check, so this would put the pre-probe credentials
    // over freshly refreshed ones with nothing to stop it.
    //
    // A Windows sharing violation while Claude Code writes, or a parse that
    // fails the lossless check, both land here. Neither means the account was
    // destroyed, and both are read again on the next run.
    match store.load() {
        Err(_) => None,
        Ok(None) => Some(store.replace(&good.creds).is_ok()),
        Ok(Some(now_stored)) => {
            if validate_credentials(&now_stored.creds.oauth, now).is_ok() {
                None
            } else {
                Some(store.replace(&good.creds).is_ok())
            }
        }
    }
}

/// Put the real access-token expiry back after a forced run that renewed
/// nothing.
///
/// Only when the store still holds exactly the backdated value. If anything
/// else is there, Claude Code wrote it -- a renewal, or something the
/// cleared-profile guard has already dealt with -- and it is not ours to
/// overwrite.
fn undo_backdate(
    store: &dyn CredentialStore,
    pre: Option<&crate::store::Loaded>,
    backdated_to: Option<i64>,
) {
    let (Some(stale), Some(good)) = (backdated_to, pre) else {
        return;
    };
    let still_ours = store
        .load()
        .ok()
        .flatten()
        .is_some_and(|l| l.creds.oauth.expires_at == stale);
    if still_ours {
        let _ = store.replace(&good.creds);
    }
}

/// Refresh one idle profile by running Claude Code against its own store.
///
/// Success is judged by an observed change in the credential store, never by
/// an exit code. Exit codes lie; a refresh window that moved forward does not.
fn refresh_one(
    ctx: &Ctx,
    name: &ProfileName,
    cli: &ClaudeCli,
    state: &crate::profile::RefreshState,
    policy: &RefreshPolicy,
    now: i64,
    force: bool,
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
    // Success is an access token that expires later than it did.
    //
    // It used to be the refresh window moving, and that window does not move.
    // Measured against a live account: a probe rotates both tokens -- their
    // hashes change -- and renews `expiresAt` by eight hours, while
    // `refreshTokenExpiresAt` moves by under a second, either way. The refresh
    // deadline is a fixed ceiling set at login, and each rotated token
    // inherits it. Judging by the window meant every successful exchange was
    // recorded as a failure and earned a backoff.
    let mut access_before = pre.as_ref().map(|l| l.creds.oauth.expires_at);

    let mut last_note = String::new();

    // `--force` means "make it happen now", and the one thing that stops it
    // happening is an access token that has not expired: Claude Code only
    // exchanges tokens when it needs a new access token. Backdating the stored
    // expiry is what turns a forced run into an actual exchange.
    //
    // This goes through `replace`, which checks validity and nothing else --
    // it does not run the monotonic window gate. That is acceptable only
    // because the write is undone on every path that does not end in a
    // renewal: see `undo_backdate`. Left in place, a failed forced run would
    // leave the profile's only record of its access token lying about it.
    let mut backdated_to: Option<i64> = None;
    if force
        && let Some(good) = &pre
        && !validate_credentials(&good.creds.oauth, now)
            .map(|h| h.access_expired)
            .unwrap_or(false)
    {
        let stale = now - 60_000;
        let mut backdated = good.creds.clone();
        backdated.oauth.expires_at = stale;
        match store.replace(&backdated) {
            // The baseline has to be what the probe will actually find, not
            // what was there a moment ago. Comparing against the original
            // would call a real exchange a failure whenever the token it
            // replaced happened to live longer than the new one.
            Ok(()) => {
                access_before = Some(stale);
                backdated_to = Some(stale);
            }
            // Not fatal. Everything else on this path degrades per profile,
            // and a forced run that cannot backdate is merely a forced run
            // that will find nothing to do.
            Err(e) => last_note = format!("could not force an exchange: {e}"),
        }
    }

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

    // Whether the store was written at all, as opposed to written usefully.
    // Without this a failure reads "the window did not move", which covers
    // two very different situations: the probe was the wrong rung and did
    // nothing, or it did exchange tokens and the server refused. Only the
    // first is worth trying a harder rung for.
    let revision_before = store.revision().ok().flatten();

    for probe in ladder {
        let outcome = match cli.run(&scope, probe, policy.spawn_timeout) {
            Ok(o) => o,
            Err(e) => {
                // The caller contains this error to the profile, but the
                // backdated expiry would outlive it.
                undo_backdate(&store, pre.as_ref(), backdated_to);
                return Err(e);
            }
        };

        // Did the probe leave the profile worse than it found it?
        if let Some(restored) = restore_if_cleared(&store, pre.as_ref(), pre_was_usable, now) {
            ctx.repo().update_meta(name, |m| {
                m.refresh.needs_login = true;
                m.refresh.last_attempt_ms = Some(now);
            })?;
            // NeedsLogin, not Broken. The credentials are back and readable;
            // what is gone is the server's willingness to refresh them, and
            // the only thing that fixes that is a person logging in.
            return Ok((
                Decision::NeedsLogin,
                Some(format!(
                    concat!(
                        "claude cleared this profile's credentials; {}. ",
                        "Run `claude auth login` and `ccred save {}`"
                    ),
                    if restored {
                        "the previous copy was put back"
                    } else {
                        "restoring the previous copy FAILED, see the backups directory"
                    },
                    name
                )),
            ));
        }

        if outcome.needs_login() {
            // Not a renewal, so the forced expiry goes back like on every
            // other such path. The message can be transient, and a profile
            // left believing its token expired a minute ago would be
            // misreported until someone refreshed it for real.
            undo_backdate(&store, pre.as_ref(), backdated_to);
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

        // There is deliberately no separate "did the environment take effect"
        // oracle here. Two were tried and both were wrong.
        //
        // `projectsDirectory` follows `CLAUDE_CONFIG_DIR`, which we do not
        // set, so it reports the shared configuration either way. The account
        // in `auth status` comes from the cached `oauthAccount` in that same
        // shared `.claude.json`, so it names the live account no matter which
        // credential store was read -- that one marked every idle profile
        // broken.
        //
        // Measured against claude 2.1.236: pointing
        // `CLAUDE_SECURESTORAGE_CONFIG_DIR` at empty credentials makes it
        // report `loggedIn: false`, while without the variable it is logged
        // in. The variable is honoured, so the renewal test below is the whole
        // proof: had another store been used, this profile's file could not
        // have changed.
        // A failed read is "not renewed", not a reason to leave: leaving here
        // would skip the undo below and strand a backdated expiry.
        let reloaded = store.load().ok().flatten();
        let access_after = reloaded.as_ref().map(|l| l.creds.oauth.expires_at);

        if renewed(access_before, access_after) {
            ctx.repo().update_meta(name, |m| {
                m.refresh.last_attempt_ms = Some(now);
                m.refresh.last_success_ms = Some(now);
                m.refresh.consecutive_failures = 0;
                m.refresh.next_attempt_after_ms = None;
                m.refresh.working_probe = Some(format!("{probe:?}").to_lowercase());
            })?;
            return Ok((Decision::Refresh, Some(format!("renewed via {probe:?}"))));
        }

        last_note = if outcome.timed_out {
            format!("{probe:?} timed out")
        } else if store.revision().ok().flatten() != revision_before {
            format!("{probe:?} rewrote the store without renewing the access token")
        } else {
            format!("{probe:?} left the store untouched")
        };
    }

    // Nothing on the ladder worked, so a forced run leaves nothing behind.
    undo_backdate(&store, pre.as_ref(), backdated_to);

    // Back off rather than hammering: an unrecognised refresher that keeps
    // retrying is how accounts get blocked.
    let failures = state.consecutive_failures.saturating_add(1);
    let next = now + backoff_ms(failures);
    ctx.repo().update_meta(name, |m| {
        m.refresh.last_attempt_ms = Some(now);
        m.refresh.consecutive_failures = failures;
        m.refresh.next_attempt_after_ms = Some(next);
    })?;
    Ok((Decision::SkipBackoff, Some(last_note)))
}

/// A run that decided to do nothing, written down anyway.
///
/// Without this, a firing that found nothing to do left no trace at all, and
/// `ccred log` could not tell it from a timer that never fired -- which is
/// the one question the log exists to answer, on a platform where Task
/// Scheduler discards everything a job prints.
///
/// Deliberately does NOT record it as the last run: the rate limit measures
/// from the last run that did something, and moving that forward on every
/// skip would suppress the real ones for as long as the timer kept firing.
fn skipped_and_logged(ctx: &Ctx, opts: &RefreshOptions, now: i64, reason: &str) -> RefreshReport {
    let report = RefreshReport::skipped(reason);
    let _ = crate::logbook::append(
        &ctx.paths().log_dir(),
        &log_entry(now, &report, opts.if_older_than_ms.is_some()),
    );
    report
}

/// Turn a report into a log line.
///
/// Decisions and numbers only. `detail` is deliberately left out: it is built
/// from rendered error messages, and the project rule is to persist error
/// kinds, because a message can echo its input and that input can be a token.
fn log_entry(now: i64, report: &RefreshReport, scheduled: bool) -> crate::logbook::Entry {
    crate::logbook::Entry {
        at_ms: now,
        command: "refresh".to_string(),
        status: report.status.clone(),
        scheduled: Some(scheduled),
        profiles: report
            .profiles
            .iter()
            .map(|p| crate::logbook::ProfileLine {
                name: p.name.clone(),
                // The serde spelling, which is snake_case; `{:?}` squashes
                // MirrorActive into "mirroractive".
                decision: serde_json::to_value(p.decision)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unknown".into()),
                window_days_before: p.window_days_before,
                window_days_after: p.window_days_after,
            })
            .collect(),
    }
}

/// The record of the last run, for anyone who wants to know whether the
/// schedule is actually working. `doctor` asks.
pub fn read_last_run(ctx: &Ctx) -> crate::Result<Option<LastRun>> {
    let path = ctx.paths().last_run();
    match std::fs::read(&path) {
        Ok(raw) => Ok(serde_json::from_slice(&raw).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CcredError::Io { path, source }),
    }
}

fn write_last_run(ctx: &Ctx, finished_at_ms: i64, report: &RefreshReport) -> crate::Result<()> {
    let record = LastRun {
        finished_at_ms,
        status: report.status.clone(),
        needed_attention: report.worth_retrying_sooner(),
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
            TokenState {
                window_left_ms: Some(2 * DAY_MS),
                refresh_expired: false,
                access_expired: true,
                usable: true,
            },
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
            TokenState {
                window_left_ms: Some(20 * DAY_MS),
                refresh_expired: false,
                access_expired: true,
                usable: true,
            },
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
            TokenState {
                window_left_ms: Some(7 * DAY_MS),
                refresh_expired: false,
                access_expired: true,
                usable: true,
            },
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
            TokenState {
                window_left_ms: Some(-DAY_MS),
                refresh_expired: true,
                access_expired: true,
                usable: true,
            },
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
        let d = decide(
            false,
            TokenState {
                window_left_ms: Some(7 * DAY_MS),
                refresh_expired: false,
                access_expired: true,
                usable: true,
            },
            &state,
            NOW,
            &policy(),
        );
        assert_eq!(d, Decision::NeedsLogin);
    }

    #[test]
    fn backoff_is_respected() {
        let state = RefreshState {
            next_attempt_after_ms: Some(NOW + 3_600_000),
            ..Default::default()
        };
        let d = decide(
            false,
            TokenState {
                window_left_ms: Some(8 * DAY_MS),
                refresh_expired: false,
                access_expired: true,
                usable: true,
            },
            &state,
            NOW,
            &policy(),
        );
        assert_eq!(d, Decision::SkipBackoff);
    }

    #[test]
    fn a_recent_attempt_is_not_repeated() {
        let state = RefreshState {
            last_attempt_ms: Some(NOW - 3_600_000),
            ..Default::default()
        };
        let d = decide(
            false,
            TokenState {
                window_left_ms: Some(8 * DAY_MS),
                refresh_expired: false,
                access_expired: true,
                usable: true,
            },
            &state,
            NOW,
            &policy(),
        );
        assert_eq!(d, Decision::SkipBackoff);
    }

    #[test]
    fn unusable_credentials_are_reported_not_refreshed() {
        let d = decide(
            false,
            TokenState {
                window_left_ms: None,
                refresh_expired: false,
                access_expired: true,
                usable: false,
            },
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
            TokenState {
                window_left_ms: None,
                refresh_expired: false,
                access_expired: true,
                usable: true,
            },
            &RefreshState::default(),
            NOW,
            &policy(),
        );
        assert_eq!(d, Decision::SkipFresh);
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

    /// A clock that was wrong when a run recorded its timestamps must not
    /// park the profile for as long as the mistake lasts. Both of these come
    /// off disk, and a dead CMOS battery or a restored VM writes moments that
    /// have not arrived; obeyed literally, that is silent and can be years.
    #[test]
    fn a_timestamp_from_the_future_is_a_wrong_clock_not_a_backoff() {
        let due = TokenState {
            window_left_ms: Some(6 * DAY_MS),
            refresh_expired: false,
            access_expired: true,
            usable: true,
        };
        let year = 365 * DAY_MS;

        let stamped_next_year = RefreshState {
            next_attempt_after_ms: Some(NOW + year),
            last_attempt_ms: Some(NOW + year),
            consecutive_failures: 1,
            ..Default::default()
        };
        assert_eq!(
            decide(false, due, &stamped_next_year, NOW, &policy()),
            Decision::Refresh,
            "a wrong clock must not outrank the work"
        );

        // A real backoff, which is at most one of those, is still obeyed.
        let genuinely_backed_off = RefreshState {
            next_attempt_after_ms: Some(NOW + DAY_MS),
            ..Default::default()
        };
        assert_eq!(
            decide(false, due, &genuinely_backed_off, NOW, &policy()),
            Decision::SkipBackoff
        );

        // And so is one a minute of clock skew away from now.
        let skewed = RefreshState {
            last_attempt_ms: Some(NOW + 60_000),
            ..Default::default()
        };
        assert_eq!(
            decide(false, due, &skewed, NOW, &policy()),
            Decision::SkipBackoff,
            "ordinary skew is not a wrong year"
        );
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
                TokenState {
                    window_left_ms: Some(20 * DAY_MS),
                    refresh_expired: false,
                    access_expired: true,
                    usable: true,
                },
                &backed_off,
                NOW,
                &policy(),
            ),
            Decision::SkipBackoff
        );
        // With force, through the same function `refresh` uses. The access
        // token is live here too: a forced run backdates it.
        let live = TokenState {
            window_left_ms: Some(20 * DAY_MS),
            refresh_expired: false,
            access_expired: false,
            usable: true,
        };
        let (cleared, tokens, wide) = forced(backed_off.clone(), live, &policy());
        assert_eq!(
            decide(false, tokens, &cleared, NOW, &wide),
            Decision::Refresh
        );

        // But a latched needs_login survives `forced`: spawning there would
        // start a process that cannot possibly succeed.
        let logged_out = RefreshState {
            needs_login: true,
            ..backed_off
        };
        let (state, tokens, wide) = forced(logged_out, live, &policy());
        assert_eq!(
            decide(false, tokens, &state, NOW, &wide),
            Decision::NeedsLogin
        );
    }

    /// Measured on a live machine: a profile whose access token still had six
    /// hours left ran all three probes, moved nothing, and was recorded as a
    /// failure with a backoff. Claude Code had no reason to exchange
    /// anything, so the refresh window could not have moved -- the run was
    /// judging a correct no-op by a yardstick that did not apply.
    #[test]
    fn a_profile_whose_access_token_is_still_live_is_not_spawned_for() {
        let low_window = Some(7 * DAY_MS);
        assert_eq!(
            decide(
                false,
                TokenState {
                    window_left_ms: low_window,
                    refresh_expired: false,
                    access_expired: false,
                    usable: // access token still valid
                true,
                },
                &RefreshState::default(),
                NOW,
                &policy(),
            ),
            Decision::SkipAccessLive
        );
        // Once it has expired there is something to exchange, so go.
        assert_eq!(
            decide(
                false,
                TokenState {
                    window_left_ms: low_window,
                    refresh_expired: false,
                    access_expired: true,
                    usable: true,
                },
                &RefreshState::default(),
                NOW,
                &policy(),
            ),
            Decision::Refresh
        );
    }

    /// The wait is short by construction: an access token lives about eight
    /// hours and the schedule is measured in days, so an idle profile is
    /// almost always past it by the time a run comes round.
    #[test]
    fn a_live_access_token_never_outranks_a_dead_refresh_token() {
        assert_eq!(
            decide(
                false,
                TokenState {
                    window_left_ms: Some(-1),
                    refresh_expired: true,
                    access_expired: // refresh window gone
                false,
                    usable: // access token still valid
                true,
                },
                &RefreshState::default(),
                NOW,
                &policy(),
            ),
            Decision::NeedsLogin,
            "a human is needed regardless of the access token"
        );
    }

    /// Measured against a live account, twice: a probe rotates both tokens
    /// and renews `expiresAt` by eight hours, while `refreshTokenExpiresAt`
    /// moves by under a second. The refresh deadline is a ceiling fixed at
    /// login and inherited by every rotated token -- so judging success by the
    /// window recorded every real exchange as a failure.
    ///
    /// This pins the shape of that measurement so the yardstick cannot drift
    /// back.
    #[test]
    fn a_renewed_access_token_is_success_even_though_the_window_stands_still() {
        // Before and after one real exchange, measured.
        let (before_access, after_access) = (1_789_152_740_824i64, 1_789_188_962_015i64);
        let (before_window, after_window) = (1_790_964_987_824i64, 1_790_964_987_015i64);

        assert!(
            renewed(Some(before_access), Some(after_access)),
            "the access token is what moves, and it moved"
        );
        assert!(
            !renewed(Some(before_window), Some(after_window)),
            "judging by the window calls this real exchange a failure"
        );
    }

    #[test]
    fn repeated_failures_wait_longer_than_the_interval_does() {
        let interval = policy().min_interval_ms;
        assert_eq!(backoff_ms(1), interval, "one failure: the usual spacing");
        assert!(backoff_ms(2) > interval, "the backoff must add something");
        assert!(backoff_ms(3) > backoff_ms(2));
        // Capped, so a profile that keeps failing is still tried weekly.
        assert_eq!(backoff_ms(3), backoff_ms(50));
        assert!(backoff_ms(u32::MAX) <= 7 * DAY_MS);
        assert_eq!(backoff_ms(0), interval);
    }

    #[test]
    fn a_store_that_vanished_is_not_a_renewal() {
        assert!(!renewed(None, Some(0)));
        assert!(!renewed(Some(5), None));
        assert!(!renewed(None, None));
        assert!(!renewed(Some(5), Some(5)), "unchanged is not renewed");
    }

    /// The deadline cannot be postponed, so saying so in time is the only
    /// remedy there is -- and it has to outrank every gate that would
    /// otherwise stay quiet. A profile four days from being locked out must
    /// not be reported as "backing off" or "up to date".
    #[test]
    fn an_approaching_deadline_outranks_every_reason_to_stay_quiet() {
        let nearly_gone = TokenState {
            window_left_ms: Some(4 * DAY_MS),
            refresh_expired: false,
            access_expired: false,
            usable: true,
        };
        let backed_off = RefreshState {
            next_attempt_after_ms: Some(NOW + DAY_MS),
            last_attempt_ms: Some(NOW),
            ..Default::default()
        };
        for state in [RefreshState::default(), backed_off] {
            assert_eq!(
                decide(false, nearly_gone, &state, NOW, &policy()),
                Decision::ExpiringSoon
            );
        }
    }

    /// It must not shout about the active profile, which Claude Code is
    /// refreshing anyway, nor about one that is already past the deadline --
    /// that is NeedsLogin, and the advice differs.
    #[test]
    fn the_warning_does_not_displace_the_two_states_that_outrank_it() {
        let nearly_gone = TokenState {
            window_left_ms: Some(4 * DAY_MS),
            refresh_expired: false,
            access_expired: false,
            usable: true,
        };
        assert_eq!(
            decide(true, nearly_gone, &RefreshState::default(), NOW, &policy()),
            Decision::MirrorActive
        );
        let gone = TokenState {
            window_left_ms: Some(-1),
            refresh_expired: true,
            ..nearly_gone
        };
        assert_eq!(
            decide(false, gone, &RefreshState::default(), NOW, &policy()),
            Decision::NeedsLogin
        );
    }

    /// The warning must displace only the quiet outcomes, never the work.
    ///
    /// Returned unconditionally, it made a profile inside five days
    /// unrefreshable -- the very profile someone is most likely to reach for
    /// `--force` over -- because `Refresh` sat below it and could not be
    /// reached. The deadline cannot be moved, but the access token still
    /// needs renewing, and both are true at once.
    #[test]
    fn an_approaching_deadline_does_not_cancel_the_refresh_it_cannot_replace() {
        let due = TokenState {
            window_left_ms: Some(4 * DAY_MS),
            refresh_expired: false,
            access_expired: true, // there IS an exchange to make
            usable: true,
        };
        assert_eq!(
            decide(false, due, &RefreshState::default(), NOW, &policy()),
            Decision::Refresh,
            "a renewable profile must still be renewed while it is warned about"
        );

        // With nothing to exchange, the warning is all there is to say.
        let nothing_to_do = TokenState {
            access_expired: false,
            ..due
        };
        assert_eq!(
            decide(
                false,
                nothing_to_do,
                &RefreshState::default(),
                NOW,
                &policy()
            ),
            Decision::ExpiringSoon
        );
    }

    /// Only trouble a retry might fix may bypass the over-fire rate limit.
    ///
    /// Keyed on "needs attention" it included a spent refresh token and an
    /// approaching deadline, neither of which retrying helps -- so the limit
    /// stayed off for as long as those lasted. On a machine with no `claude`
    /// installed that is permanent, and the idempotency this module promises
    /// was void from the first run.
    #[test]
    fn only_transient_trouble_lifts_the_rate_limit() {
        let line = |d: Decision| ProfileResult {
            name: "p".into(),
            decision: d,
            detail: None,
            window_days_before: None,
            window_days_after: None,
        };
        let report = |d: Decision| RefreshReport {
            status: "ran".into(),
            profiles: vec![line(d)],
        };

        for persistent in [Decision::NeedsLogin, Decision::ExpiringSoon] {
            let r = report(persistent);
            assert!(
                r.needs_attention(),
                "{persistent:?} still concerns a person"
            );
            assert!(
                !r.worth_retrying_sooner(),
                "{persistent:?} cannot be fixed by running again"
            );
        }

        let r = report(Decision::Broken);
        assert!(r.worth_retrying_sooner(), "a broken run is worth retrying");

        let quiet = report(Decision::SkipFresh);
        assert!(!quiet.needs_attention() && !quiet.worth_retrying_sooner());
    }

    /// A forced run that renews nothing must leave the profile as it found
    /// it. `--force` backdates the stored access-token expiry so that Claude
    /// Code exchanges tokens; if nothing is exchanged, that false expiry was
    /// staying behind as the profile's only record of its access token.
    #[test]
    fn a_forced_run_that_renews_nothing_leaves_the_real_expiry() {
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

        let stale = NOW - 60_000;
        let mut backdated = good.clone();
        backdated.oauth.expires_at = stale;
        store.replace(&backdated).unwrap();

        undo_backdate(&store, pre.as_ref(), Some(stale));
        assert_eq!(
            store.load().unwrap().unwrap().creds.oauth.expires_at,
            1_788_003_600_000,
            "the real expiry must be back"
        );
    }

    /// If Claude Code wrote anything, it is not ours to overwrite -- that is
    /// a renewal, or damage the cleared-profile guard has already handled.
    #[test]
    fn undoing_a_backdate_never_overwrites_what_claude_wrote() {
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

        // A renewal landed: a new expiry, not the backdated one.
        let mut renewed = good.clone();
        renewed.oauth.expires_at = NOW + 8 * 3_600_000;
        store.replace(&renewed).unwrap();

        undo_backdate(&store, pre.as_ref(), Some(NOW - 60_000));
        assert_eq!(
            store.load().unwrap().unwrap().creds.oauth.expires_at,
            NOW + 8 * 3_600_000,
            "a renewal must survive"
        );
    }
}
