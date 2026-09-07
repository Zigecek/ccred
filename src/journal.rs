//! Crash safety for `switch`.
//!
//! Switching touches four things in order: the outgoing profile, the live
//! credentials, the live account identity, and the active pointer. A process
//! killed between any two of them would leave the machine in a state nobody
//! can reason about afterwards.
//!
//! The journal records how far we got. Every command replays it before doing
//! anything else, so an interrupted switch heals on the next invocation rather
//! than needing a human. Because credentials are *copied* and both a backup
//! and a last-known-good copy exist, no phase can lose a token -- the worst
//! case is a divergence that `doctor` reconciles.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::atomic::write_atomic;
use crate::error::CcredError;

/// How far a switch got. Ordering matters: later phases imply earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchPhase {
    /// Journal written. Nothing else touched yet.
    Started,
    /// The outgoing profile has the live credentials mirrored into it.
    OutgoingSynced,
    /// The target's credentials are now the live ones.
    LiveCredsWritten,
    /// `.claude.json` now names the target account.
    LiveIdentityWritten,
    /// The active pointer names the target.
    PointerUpdated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchJournal {
    pub from: Option<String>,
    pub to: String,
    pub phase: SwitchPhase,
    pub started_at_ms: i64,
    pub pid: u32,
}

impl SwitchJournal {
    pub fn new(from: Option<String>, to: String, now_ms: i64) -> Self {
        SwitchJournal {
            from,
            to,
            phase: SwitchPhase::Started,
            started_at_ms: now_ms,
            pid: std::process::id(),
        }
    }

    pub fn load(path: &Path) -> crate::Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(raw) => {
                let journal = serde_json::from_slice(&raw).map_err(|source| CcredError::Json {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok(Some(journal))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CcredError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn save(&self, path: &Path) -> crate::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|source| CcredError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        write_atomic(path, &bytes, true)
    }

    pub fn advance(&mut self, path: &Path, phase: SwitchPhase) -> crate::Result<()> {
        self.phase = phase;
        self.save(path)
    }

    pub fn clear(path: &Path) -> crate::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CcredError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// What an interrupted switch should do to become consistent again.
    ///
    /// The rule is simple: before the live credentials were replaced, roll
    /// back; after, roll forward. Rolling back once the live file already
    /// holds the target's credentials would leave the pointer disagreeing
    /// with reality, which is worse than finishing the job.
    pub fn recovery(&self) -> Recovery {
        match self.phase {
            SwitchPhase::Started | SwitchPhase::OutgoingSynced => {
                Recovery::RollBackTo(self.from.clone())
            }
            SwitchPhase::LiveCredsWritten | SwitchPhase::LiveIdentityWritten => {
                Recovery::CompleteForward(self.to.clone())
            }
            SwitchPhase::PointerUpdated => Recovery::AlreadyDone,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// Live was never modified; restore the pointer and stop.
    RollBackTo(Option<String>),
    /// Live already holds the target; finish the remaining steps.
    CompleteForward(String),
    AlreadyDone,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn journal(phase: SwitchPhase) -> SwitchJournal {
        SwitchJournal {
            from: Some("work".into()),
            to: "personal".into(),
            phase,
            started_at_ms: 1_788_000_000_000,
            pid: 1234,
        }
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("switch.journal");
        assert!(SwitchJournal::load(&path).unwrap().is_none());

        let j = journal(SwitchPhase::Started);
        j.save(&path).unwrap();

        let back = SwitchJournal::load(&path).unwrap().unwrap();
        assert_eq!(back.to, "personal");
        assert_eq!(back.phase, SwitchPhase::Started);

        SwitchJournal::clear(&path).unwrap();
        assert!(SwitchJournal::load(&path).unwrap().is_none());
    }

    #[test]
    fn clear_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("switch.journal");
        SwitchJournal::clear(&path).unwrap();
        SwitchJournal::clear(&path).unwrap();
    }

    #[test]
    fn phases_before_the_live_write_roll_back() {
        for phase in [SwitchPhase::Started, SwitchPhase::OutgoingSynced] {
            assert_eq!(
                journal(phase).recovery(),
                Recovery::RollBackTo(Some("work".into())),
                "{phase:?} must roll back"
            );
        }
    }

    #[test]
    fn phases_after_the_live_write_roll_forward() {
        for phase in [
            SwitchPhase::LiveCredsWritten,
            SwitchPhase::LiveIdentityWritten,
        ] {
            assert_eq!(
                journal(phase).recovery(),
                Recovery::CompleteForward("personal".into()),
                "{phase:?} must complete forward"
            );
        }
    }

    #[test]
    fn the_final_phase_needs_nothing() {
        assert_eq!(
            journal(SwitchPhase::PointerUpdated).recovery(),
            Recovery::AlreadyDone
        );
    }

    #[test]
    fn phase_ordering_is_monotonic() {
        // Recovery reasoning relies on later phases implying earlier ones.
        assert!(SwitchPhase::Started < SwitchPhase::OutgoingSynced);
        assert!(SwitchPhase::OutgoingSynced < SwitchPhase::LiveCredsWritten);
        assert!(SwitchPhase::LiveCredsWritten < SwitchPhase::LiveIdentityWritten);
        assert!(SwitchPhase::LiveIdentityWritten < SwitchPhase::PointerUpdated);
    }
}
