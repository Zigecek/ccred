//! `ccred` -- manage several Claude Code accounts on one machine.
//!
//! The safety core of this library exists because of a specific incident. Its
//! bash predecessor validated tokens with `jq -e '.claudeAiOauth.refreshToken'`,
//! but `jq -e` treats an **empty string as truthy** -- only `false` and `null`
//! are falsy. A logged-out state therefore passed validation and overwrote the
//! last good copy of a profile.
//!
//! The types and validators below are built so that this class of bug is either
//! a compile error or a loud failure, never a silently discarded token.

pub mod atomic;
pub mod claude_cli;
pub mod cli;
pub mod error;
pub mod journal;
pub mod lockfile;
pub mod model;
pub mod ops;
pub mod paths;
pub mod proc;
pub mod profile;
pub mod redact;
pub mod store;
pub mod validate;

pub use error::{CcredError, Result};
