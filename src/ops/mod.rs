//! Command implementations.
//!
//! Operations return data, never printed text. That keeps them testable and
//! keeps formatting decisions -- including the rule that no output may ever
//! contain a token -- in one place.

pub mod doctor;
pub mod refresh;
pub mod schedule;
pub mod simple;
pub mod switch;

use crate::model::{AccountSnapshot, ClaudeJsonDoc};
use crate::paths::Paths;
use crate::profile::ProfileRepo;
use crate::store::file::FileStore;

/// Everything an operation needs: where things are, and the profile store.
pub struct Ctx {
    repo: ProfileRepo,
}

impl Ctx {
    pub fn from_env() -> crate::Result<Self> {
        Ok(Ctx {
            repo: ProfileRepo::new(Paths::from_env()?),
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
}

/// Days remaining until an epoch-ms deadline, floored. Negative when past.
pub fn days_until(deadline_ms: i64, now_ms: i64) -> i64 {
    (deadline_ms - now_ms) / 86_400_000
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
    }
}
