//! Command implementations.
//!
//! Operations return data, never printed text. That keeps them testable and
//! keeps formatting decisions -- including the rule that no output may ever
//! contain a token -- in one place.

pub mod desktop;
pub mod doctor;
pub mod refresh;
pub mod schedule;
pub mod simple;
pub mod switch;
pub mod uninstall;

use crate::lockfile::DirLock;
use crate::model::{AccountSnapshot, ClaudeJsonDoc};
use crate::paths::{Locations, Paths};
use crate::profile::ProfileRepo;
use crate::store::file::FileStore;

/// Everything an operation needs: where things are, and the profile store.
pub struct Ctx {
    repo: ProfileRepo,
}

impl Ctx {
    pub fn from_env() -> crate::Result<Self> {
        Self::resolve(Locations::default())
    }

    /// Locations given on the command line, falling back to the environment.
    pub fn resolve(explicit: Locations) -> crate::Result<Self> {
        Ok(Ctx {
            repo: ProfileRepo::new(Paths::resolve(explicit)?),
        })
    }

    pub fn with_paths(paths: Paths) -> Self {
        Ctx {
            repo: ProfileRepo::new(paths),
        }
    }

    pub fn repo(&self) -> &ProfileRepo {
        &self.repo
    }

    pub fn paths(&self) -> &Paths {
        self.repo.paths()
    }

    /// The store a plain `claude` reads from.
    pub fn live_store(&self) -> FileStore {
        FileStore::new(self.paths().claude_config_dir().to_path_buf())
    }

    /// Exclusive use of the profiles, between `ccred` processes.
    ///
    /// A refresh runs `claude` against one profile's own store for up to two
    /// minutes. A switch that made that profile live meanwhile copied a
    /// token the probe then rotated away, signing the live session out; and
    /// the refresh, having read the active profile once at the start, never
    /// knew. So every command that changes which profile is live, or writes
    /// a profile's credentials, holds this, and `refresh` holds it for one
    /// profile at a time and reads the active profile under it.
    ///
    /// Always taken before the live store's lock, never while holding it, so
    /// the two cannot deadlock.
    pub fn lock_profiles(&self, timeout: std::time::Duration) -> crate::Result<DirLock> {
        crate::lockfile::acquire(&self.paths().state_dir().join(".profiles"), timeout).map_err(
            |e| match e {
                crate::error::CcredError::Busy(_) => crate::error::CcredError::Busy(
                    concat!(
                        "another ccred command is working on the profiles -- a scheduled ",
                        "refresh can take a minute or two; try again shortly"
                    )
                    .into(),
                ),
                other => other,
            },
        )
    }

    /// Which account the live configuration currently names.
    ///
    /// An absent or unreadable `.claude.json` yields an unknown snapshot
    /// rather than an error: callers must already treat "unknown" as a reason
    /// to refuse a write, so it needs no separate failure path.
    pub fn live_account(&self) -> AccountSnapshot {
        match ClaudeJsonDoc::load(self.paths().claude_config_file()) {
            Ok(doc) => AccountSnapshot::from_config(&doc),
            Err(_) => AccountSnapshot::default(),
        }
    }

    /// Why the account could not be read, when it could not.
    ///
    /// The snapshot above collapses every failure into "unknown", which is
    /// the right answer for a report and the wrong one for a refusal: "is
    /// Claude Code set up in this home?" is unhelpful when the truth is that
    /// the file is UTF-16, or has been edited into something that will not
    /// parse. Only the error's own sentence, which names a path and a kind
    /// and never file contents.
    pub fn live_account_problem(&self) -> Option<String> {
        match ClaudeJsonDoc::load(self.paths().claude_config_file()) {
            Ok(_) => None,
            // Absent is not a problem to explain: it means Claude Code has
            // not run here yet, which the message already says.
            Err(crate::CcredError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                None
            }
            Err(e) => Some(e.to_string()),
        }
    }
}

/// Days remaining until an epoch-ms deadline, floored. Negative when past.
///
/// Floored, not truncated: plain division rounds toward zero, so a deadline
/// an hour in the past came out as `0` and the list read `0d` -- "expires
/// today" rather than "expired".
pub fn days_until(deadline_ms: i64, now_ms: i64) -> i64 {
    deadline_ms.saturating_sub(now_ms).div_euclid(86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_until_counts_down_and_goes_negative() {
        let now = 1_788_000_000_000;
        assert_eq!(days_until(now + 86_400_000 * 3, now), 3);
        assert_eq!(days_until(now, now), 0);
        assert_eq!(days_until(now - 86_400_000 * 2, now), -2);
        // An hour past the deadline is expired, not "0 days left".
        assert_eq!(days_until(now - 3_600_000, now), -1);
        // And an hour before it is still today.
        assert_eq!(days_until(now + 3_600_000, now), 0);
    }
}
