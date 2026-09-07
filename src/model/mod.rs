//! Models for files that belong to Claude Code.
//!
//! Ground rule: these files are not ours. We read them, change one thing, and
//! write them back -- and we must not drop anything we do not understand.

pub mod claude_json;
pub mod credentials;

pub use claude_json::{AccountIdentity, AccountSnapshot, ClaudeJsonDoc};
pub use credentials::{CredentialsFile, OAuthCredentials};
