//! Command-line surface.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ccred",
    version,
    about = "Save, list and switch between named sets of local Claude Code credentials.",
    long_about = None,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Print machine-readable JSON instead of a human summary.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show which account is logged in and which profile is active.
    Current,

    /// List saved profiles.
    #[command(alias = "ls")]
    List,

    /// Save the account that is currently logged in as a named profile.
    Save {
        /// Profile name (letters, digits, dot, underscore, hyphen).
        name: String,
    },

    /// Make a saved profile the active account.
    Switch {
        name: String,
        /// Switch even though Claude Code is running. It may then write the
        /// old account's refreshed token into the new profile's file.
        #[arg(long, short)]
        force: bool,
    },

    /// Delete a saved profile.
    #[command(alias = "remove")]
    Rm { name: String },

    /// Refresh stored profiles so idle accounts do not expire.
    ///
    /// Safe to run more often than needed: it checks when it last ran and
    /// exits successfully without doing anything if that was recent.
    Refresh {
        /// Do nothing if the last run was more recent than this many hours.
        #[arg(long, value_name = "HOURS")]
        if_older_than: Option<u32>,
        /// Accepted for symmetry with schedulers; all profiles are the default.
        #[arg(long)]
        all: bool,
        /// Path to the claude binary, when it is not on PATH.
        #[arg(long, value_name = "PATH")]
        claude_path: Option<std::path::PathBuf>,
    },

    /// Check for anything that is quietly wrong.
    Doctor,

    /// Anything else: almost always someone typing `ccred <profile>`.
    ///
    /// The bare form is deliberately not a switch alias. That ambiguity -- a
    /// profile named `list` or `save` -- is exactly what this command layout
    /// was reorganised to remove, so the answer is a hint, not a guess.
    #[command(external_subcommand)]
    Unknown(Vec<String>),
}
