//! Error types and their mapping onto process exit codes.
//!
//! The codes are part of the interface: a scheduled job uses them to tell
//! "retry soon" from "retry later" from "stop and get a human".

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, CcredError>;

/// Why a token was rejected. Carries the field name only, never the value.
#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum TokenInvalid {
    #[error("{field} is missing or null")]
    Missing { field: &'static str },
    #[error("{field} is empty or whitespace only")]
    Blank { field: &'static str },
    #[error("{field} is implausibly short ({len} bytes)")]
    TooShort { field: &'static str, len: usize },
    #[error("{field} contains whitespace or control characters")]
    Malformed { field: &'static str },
    #[error("{field} does not have the shape of a Claude token")]
    BadShape { field: &'static str },
}

#[derive(Debug, thiserror::Error)]
pub enum CcredError {
    #[error("invalid credentials: {0}")]
    InvalidCredentials(#[from] TokenInvalid),

    #[error("refusing unsafe write: {0}")]
    UnsafeWrite(String),

    #[error("refusing lossy rewrite, it would drop: {dropped:?}")]
    LossyRewrite { dropped: Vec<String> },

    #[error("refusing to write through a symlink: {0}")]
    RefusedSymlink(PathBuf),

    #[error("invalid profile name '{name}': {reason}")]
    InvalidProfileName { name: String, reason: &'static str },

    #[error("profile name '{name}' escapes the profiles directory")]
    PathEscape { name: String },

    #[error("profile '{0}' not found")]
    ProfileNotFound(String),

    /// The incoming credentials belong to a different account than the profile.
    ///
    /// This is the gap that the refresh-window check cannot see: two accounts
    /// can both be valid with advancing windows. A live near-miss stored one
    /// account's credentials into another account's profile for exactly this
    /// reason.
    #[error("profile '{profile}' belongs to {stored}, not {incoming}")]
    AccountMismatch {
        profile: String,
        stored: String,
        incoming: String,
    },

    /// We could not establish which account the incoming credentials are for,
    /// and the profile already has a known owner. Refusing beats guessing.
    #[error(
        "cannot confirm these credentials belong to {stored}, the owner of profile '{profile}'"
    )]
    AccountUnverifiable { profile: String, stored: String },

    #[error("scheduler: {0}")]
    Schedule(String),

    /// The `claude` binary could not be found or is not where it was said to
    /// be. A configuration problem, not an unsafe state, and the exit code
    /// says so -- a scheduler needs to tell "set this machine up" apart from
    /// "something is wrong with the credentials".
    #[error("{0}")]
    ClaudeMissing(String),

    #[error("I/O error at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("malformed JSON in {path}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Exit codes. A scheduler consumes these, so they are stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Ok = 0,
    Internal = 1,
    Usage = 2,
    NotFound = 3,
    /// Needs a manual login. Do **not** retry -- this needs a human.
    NeedsLogin = 4,
    /// Transient failure, retry according to the backoff.
    Transient = 5,
    /// Locked, or `claude` is running. Retry soon.
    Busy = 6,
    /// Unsafe or corrupt state, write refused. Stop and report loudly.
    Unsafe = 7,
    /// Misconfiguration (`claude` missing, environment ignored).
    Misconfigured = 8,
}

impl CcredError {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            CcredError::InvalidCredentials(_) => ExitCode::Unsafe,
            CcredError::UnsafeWrite(_) => ExitCode::Unsafe,
            CcredError::LossyRewrite { .. } => ExitCode::Unsafe,
            CcredError::RefusedSymlink(_) => ExitCode::Unsafe,
            CcredError::InvalidProfileName { .. } => ExitCode::NotFound,
            CcredError::PathEscape { .. } => ExitCode::Unsafe,
            CcredError::ProfileNotFound(_) => ExitCode::NotFound,
            CcredError::AccountMismatch { .. } => ExitCode::Unsafe,
            CcredError::AccountUnverifiable { .. } => ExitCode::Unsafe,
            CcredError::Schedule(_) => ExitCode::Misconfigured,
            CcredError::ClaudeMissing(_) => ExitCode::Misconfigured,
            CcredError::Io { .. } => ExitCode::Internal,
            CcredError::Json { .. } => ExitCode::Unsafe,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scheduler reads nothing but the exit code, so the numbers are a
    /// published contract. Changing one silently changes what systemd,
    /// launchd and Task Scheduler do about a failure.
    #[test]
    fn the_exit_code_numbers_are_a_contract() {
        assert_eq!(ExitCode::Ok as i32, 0);
        assert_eq!(ExitCode::Internal as i32, 1);
        assert_eq!(ExitCode::Usage as i32, 2);
        assert_eq!(ExitCode::NotFound as i32, 3);
        assert_eq!(ExitCode::NeedsLogin as i32, 4);
        assert_eq!(ExitCode::Transient as i32, 5);
        assert_eq!(ExitCode::Busy as i32, 6);
        assert_eq!(ExitCode::Unsafe as i32, 7);
        assert_eq!(ExitCode::Misconfigured as i32, 8);
    }

    /// The distinction that matters most: "this machine is not set up" must
    /// not arrive looking like "your credentials are in danger". They call for
    /// opposite reactions -- install something, versus stop and look.
    #[test]
    fn a_missing_binary_is_not_reported_as_an_unsafe_state() {
        assert_eq!(
            CcredError::ClaudeMissing("no claude".into()).exit_code(),
            ExitCode::Misconfigured
        );
        assert_eq!(
            CcredError::Schedule("systemd said no".into()).exit_code(),
            ExitCode::Misconfigured
        );
    }

    /// Everything that means "a write was refused because the state is not
    /// safe" has to land on the same code, or a scheduler cannot act on it.
    #[test]
    fn every_refused_write_exits_unsafe() {
        let refusals = [
            CcredError::UnsafeWrite("x".into()),
            CcredError::LossyRewrite {
                dropped: vec!["k".into()],
            },
            CcredError::RefusedSymlink("/tmp/x".into()),
            CcredError::PathEscape { name: "..".into() },
            CcredError::AccountMismatch {
                profile: "p".into(),
                stored: "a".into(),
                incoming: "b".into(),
            },
            CcredError::AccountUnverifiable {
                profile: "p".into(),
                stored: "a".into(),
            },
        ];
        for e in refusals {
            assert_eq!(e.exit_code(), ExitCode::Unsafe, "{e}");
        }
    }

    /// A name the user mistyped is a lookup failure, not a danger.
    #[test]
    fn a_bad_name_is_a_lookup_failure() {
        assert_eq!(
            CcredError::ProfileNotFound("nope".into()).exit_code(),
            ExitCode::NotFound
        );
        assert_eq!(
            CcredError::InvalidProfileName {
                name: "!".into(),
                reason: "bad"
            }
            .exit_code(),
            ExitCode::NotFound
        );
    }

    /// No error may render as an empty string: the message is the whole
    /// report for anything reading stderr.
    #[test]
    fn every_error_says_something() {
        let all = [
            CcredError::UnsafeWrite("x".into()),
            CcredError::ClaudeMissing("y".into()),
            CcredError::Schedule("z".into()),
            CcredError::ProfileNotFound("p".into()),
            CcredError::LossyRewrite {
                dropped: vec!["k".into()],
            },
            CcredError::PathEscape { name: "..".into() },
        ];
        for e in all {
            assert!(!e.to_string().trim().is_empty(), "{e:?} renders as nothing");
        }
    }
}
