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
