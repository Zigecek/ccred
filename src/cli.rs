//! Command-line surface.

use clap::{Args, Parser, Subcommand};

/// clap's own help and errors, in the same palette as everything else.
///
/// Without this the help screen is the one plain thing left on the display,
/// which reads as a different program.
const STYLES: clap::builder::Styles = clap::builder::Styles::styled()
    .header(anstyle::Style::new().bold())
    .usage(anstyle::Style::new().bold())
    .literal(
        anstyle::Style::new()
            .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Cyan)))
            .bold(),
    )
    .placeholder(anstyle::Style::new().dimmed())
    .error(
        anstyle::Style::new()
            .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Red)))
            .bold(),
    )
    .valid(anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Green))))
    .invalid(
        anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow))),
    );

const HELP_TEMPLATE: &str = "{before-help}{name} {version}
{about}

{usage-heading} {usage}

{all-args}{after-help}";

const EXAMPLES: &str = "Examples:
  ccred                      who is logged in right now
  ccred save work            store that account under a name
  ccred switch personal      make another saved profile the active one
  ccred list                 every profile, and how much window each has left
  ccred schedule install     keep idle profiles alive without being asked

Every command takes --json for machine-readable output.
A bare `ccred <name>` is not a switch alias, so a profile may safely be
called `list` or `save`.";

#[derive(Debug, Parser)]
#[command(
    name = "ccred",
    bin_name = "ccred",
    version,
    about = "Save, list and switch between named sets of local Claude Code credentials.",
    long_about = None,
    disable_help_subcommand = true,
    styles = STYLES,
    help_template = HELP_TEMPLATE,
    after_help = EXAMPLES,
    max_term_width = 100
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
    #[command(display_order = 1)]
    Current,

    /// List saved profiles, with the refresh window each has left.
    #[command(alias = "ls", display_order = 2)]
    List,

    /// Save the account that is currently logged in as a named profile.
    #[command(display_order = 3)]
    Save {
        /// Profile name (letters, digits, dot, underscore, hyphen).
        name: String,
    },

    /// Make a saved profile the active account.
    #[command(display_order = 4)]
    Switch {
        name: String,
        /// Switch even though Claude Code is running. It may then write the
        /// old account's refreshed token into the new profile's file.
        #[arg(long, short)]
        force: bool,
    },

    /// Delete a saved profile.
    #[command(alias = "remove", display_order = 5)]
    Rm { name: String },

    /// Refresh stored profiles so idle accounts do not expire.
    ///
    /// Safe to run more often than needed: it checks when it last ran and
    /// exits successfully without doing anything if that was recent.
    #[command(display_order = 6)]
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

    /// Install, remove or inspect the background refresh schedule.
    #[command(display_order = 7)]
    Schedule(ScheduleArgs),

    /// Check for anything that is quietly wrong.
    #[command(display_order = 8)]
    Doctor,

    /// Anything else: almost always someone typing `ccred <profile>`.
    ///
    /// The bare form is deliberately not a switch alias. That ambiguity -- a
    /// profile named `list` or `save` -- is exactly what this command layout
    /// was reorganised to remove, so the answer is a hint, not a guess.
    #[command(external_subcommand)]
    Unknown(Vec<String>),
}

#[derive(Debug, Args)]
pub struct ScheduleArgs {
    #[command(subcommand)]
    pub action: ScheduleAction,
}

#[derive(Debug, Subcommand)]
pub enum ScheduleAction {
    /// Register the periodic refresh with this platform's scheduler.
    Install {
        /// Print what would be registered, without touching anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove it again.
    Uninstall,
    /// Report whether it is registered and when it will next run.
    Status,
}
