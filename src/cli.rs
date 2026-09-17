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
  ccred save work            store that account under a name (Desktop as work-desktop)
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

    /// Keep profiles and state in DIR [default: ~/.ccred]
    ///
    /// The same as $CCRED_HOME, which it overrides. A schedule installed with
    /// either is registered with this flag, because the job does not inherit
    /// the shell that set the variable.
    #[arg(long, global = true, value_name = "DIR")]
    pub ccred_home: Option<std::path::PathBuf>,

    /// Claude Code's configuration directory [default: ~/.claude]
    ///
    /// The same as $CLAUDE_CONFIG_DIR, which it overrides. A spawned `claude`
    /// is given this directory too, and a schedule carries it like
    /// --ccred-home.
    #[arg(long, global = true, value_name = "DIR")]
    pub claude_config_dir: Option<std::path::PathBuf>,

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

    /// Save what is logged in, under a name (Claude Desktop as <name>-desktop).
    ///
    /// Claude Code's login becomes the profile, Claude Desktop's its
    /// `-desktop` namesake. Each half is saved when it is there to save, and
    /// the output says which. `-desktop` is reserved: `ccred save work`
    /// records `work` and `work-desktop`, and the two are switched
    /// separately from then on.
    #[command(display_order = 3)]
    Save {
        /// Profile name (letters, digits, dot, underscore, hyphen).
        name: String,
        /// Save only Claude Code's login.
        #[arg(long, conflicts_with = "only_desktop")]
        only_code: bool,
        /// Save only Claude Desktop's login.
        #[arg(long)]
        only_desktop: bool,
    },

    /// Make a saved profile the active account.
    ///
    /// `ccred switch work` moves Claude Code -- the terminal, the VS Code
    /// extension -- and `ccred switch work-desktop` moves Claude Desktop,
    /// which has to be closed for it. The two log in separately and can be
    /// on different accounts.
    #[command(display_order = 4)]
    Switch {
        name: String,
        /// Switch even though Claude Code is running. It may then write the
        /// old account's refreshed token into the new profile's file.
        /// (Claude Desktop sessions never do, and do not need this.)
        #[arg(long, short)]
        force: bool,
    },

    /// Delete a saved profile: `work`, or its Desktop login `work-desktop`.
    #[command(alias = "remove", display_order = 5)]
    Rm { name: String },

    /// Put a profile's last-known-good credentials back.
    ///
    /// Every accepted save also writes a copy beside the profile. This puts
    /// that copy back, for when the current credentials have been damaged --
    /// a spawned Claude Code signing itself out is the case this exists for.
    /// `ccred doctor` says when a profile has a copy worth restoring.
    #[command(display_order = 6)]
    Restore { name: String },

    /// Refresh stored profiles so idle accounts do not expire.
    ///
    /// Safe to run more often than needed: it checks when it last ran and
    /// exits successfully without doing anything if that was recent.
    #[command(display_order = 7)]
    Refresh {
        /// Do nothing if the last run was more recent than this many hours.
        #[arg(long, value_name = "HOURS")]
        if_older_than: Option<u32>,
        /// Try every profile now, ignoring the backoff and the window
        /// threshold.
        ///
        /// The schedule deliberately leaves a profile alone until its window
        /// runs low, and backs off after a failure. Both are right for a
        /// timer and wrong for someone who has just fixed whatever was broken
        /// and wants to see it work.
        #[arg(long)]
        force: bool,
        /// Every profile. This is the default, and the flag exists so that
        /// someone who types it gets what they expect rather than an error.
        ///
        /// Nothing passes it -- not the scheduler, which sends only
        /// `--if-older-than`. An earlier comment claimed it was there for
        /// symmetry with schedulers, which was not true of any of them.
        #[arg(long)]
        all: bool,
        /// Path to the claude binary, when it is not on PATH.
        #[arg(long, value_name = "PATH")]
        claude_path: Option<std::path::PathBuf>,
    },

    /// Install, remove or inspect the background refresh schedule.
    #[command(display_order = 8)]
    Schedule(ScheduleArgs),

    /// Check for anything that is quietly wrong.
    #[command(display_order = 10)]
    Doctor,

    /// Remove ccred from this machine.
    ///
    /// Removes the refresh schedule, the installer's receipt and the binary.
    /// Stored profiles are kept unless `--purge` is given. The Claude Code
    /// login in ~/.claude is never touched.
    ///
    /// A binary installed by Scoop, npm, Homebrew, apt or cargo is left for
    /// that tool to remove, and the command to do it is printed: deleting a
    /// file a package manager tracks corrupts its record of what is installed.
    #[command(display_order = 11)]
    Uninstall {
        /// Also delete every stored profile, backup and log under ~/.ccred.
        /// Those are the only copies of accounts that are not logged in.
        #[arg(long)]
        purge: bool,
        /// Do not ask before deleting stored credentials.
        #[arg(long, short)]
        yes: bool,
        /// Show what would be removed, and remove nothing.
        #[arg(long)]
        dry_run: bool,
    },

    /// Show what past refreshes did, scheduled or not.
    ///
    /// A scheduled run is otherwise invisible on Windows, where Task
    /// Scheduler discards its output entirely. Decisions and numbers only --
    /// never an error message, which could echo a token.
    #[command(display_order = 12)]
    Log {
        /// How many runs to show.
        #[arg(long, short = 'n', value_name = "COUNT", default_value_t = 20)]
        count: usize,
    },

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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap validates its own configuration only in debug builds, and only
    /// when asked. Without this a duplicated short flag or a bad argument
    /// relationship is a panic the first time a user runs that command.
    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// The bare form is not a switch alias, which is what lets a profile be
    /// called `list` or `save`. An external subcommand is how that stays
    /// true; losing it would turn a hint into a wrong guess.
    #[test]
    fn an_unknown_word_is_captured_rather_than_rejected() {
        let cli = Cli::try_parse_from(["ccred", "some-profile"]).expect("must parse");
        assert!(
            matches!(cli.command, Some(Command::Unknown(_))),
            "{:?}",
            cli.command
        );
    }

    #[test]
    fn json_is_accepted_on_every_command_not_just_the_root() {
        for args in [
            vec!["ccred", "list", "--json"],
            vec!["ccred", "--json", "list"],
            vec!["ccred", "doctor", "--json"],
            vec!["ccred", "schedule", "status", "--json"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(cli.json, "{args:?}");
        }
    }

    /// `refresh --force` and `switch --force` mean different things, and both
    /// take the short form. A collision here would be found by a user.
    #[test]
    fn both_force_flags_parse() {
        let cli = Cli::try_parse_from(["ccred", "switch", "work", "--force"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Switch { force: true, .. })
        ));
        let cli = Cli::try_parse_from(["ccred", "refresh", "--force"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Refresh { force: true, .. })
        ));
    }
}
