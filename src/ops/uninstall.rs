//! Removing `ccred` from a machine.
//!
//! # What it touches, and what it never does
//!
//! It removes what `ccred` put there: the refresh schedule, the installer's
//! receipt, the binary, and -- only when asked with `--purge` -- the stored
//! profiles under `~/.ccred`.
//!
//! It **never** touches `~/.claude`. That directory, and the login in it,
//! belong to Claude Code. Uninstalling a tool that manages logins must not
//! log anybody out.
//!
//! # Why the binary is not always deleted
//!
//! A package manager keeps its own record of the files it installed. Deleting
//! one of those files behind its back leaves that record lying: `scoop`
//! believes `ccred` is still installed, `dpkg` reports a package whose binary
//! is missing, and the next upgrade or removal fails in confusing ways. So
//! when a package manager owns the binary, this says which command removes it
//! and leaves the file alone.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::Ctx;
use crate::error::CcredError;

/// Who owns the running executable, and so who should remove it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Owner {
    /// The release installer put it there and left a receipt: ours to delete.
    Installer,
    /// Built or copied by hand, with nothing tracking it: ours to delete.
    Unmanaged,
    /// A package manager tracks it. Deleting the file would corrupt that.
    PackageManager { name: String, command: String },
}

/// Decide who owns an executable, from its path and a few well-known
/// bookkeeping files. Pure apart from those existence checks, and the checks
/// are passed in, so the rule is testable without any of them installed.
pub fn owner_of(exe: &Path, receipt_exists: bool, exists: impl Fn(&str) -> bool) -> Owner {
    let p = exe
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let managed = |name: &str, command: &str| Owner::PackageManager {
        name: name.to_string(),
        command: command.to_string(),
    };

    if p.contains("/scoop/apps/") || p.contains("/scoop/shims/") {
        return managed("Scoop", "scoop uninstall ccred");
    }
    if p.contains("/node_modules/") {
        return managed("npm", "npm uninstall -g ccred");
    }
    if p.contains("/cellar/") || p.contains("/homebrew/") || p.contains("/linuxbrew/") {
        return managed("Homebrew", "brew uninstall ccred");
    }
    if p.starts_with("/usr/bin/") || p.starts_with("/bin/") {
        if exists("/var/lib/dpkg/info/ccred.list") {
            return managed("apt", "sudo apt remove ccred");
        }
        if exists("/var/lib/pacman/local") {
            return managed("pacman", "sudo pacman -R ccred-bin");
        }
        // Something system-wide that we cannot identify. Deleting a file in
        // /usr/bin without knowing who put it there is not ours to do.
        return managed("the system", "your package manager");
    }
    if receipt_exists {
        return Owner::Installer;
    }
    if p.contains("/.cargo/bin/") {
        return managed("cargo", "cargo uninstall ccred");
    }
    Owner::Unmanaged
}

/// Judge the executable by the path it was started from and by where that
/// path really leads.
///
/// Homebrew links `/usr/local/bin/ccred` to a file in its Cellar. The link's
/// own path says nothing about Homebrew, so judging by it alone deleted the
/// link behind brew's back.
pub fn owner_of_paths(
    exe: &Path,
    resolved: Option<&Path>,
    receipt_exists: bool,
    exists: impl Fn(&str) -> bool,
) -> Owner {
    if let Some(real) = resolved {
        let owner = owner_of(real, false, &exists);
        if matches!(owner, Owner::PackageManager { .. }) {
            return owner;
        }
    }
    owner_of(exe, receipt_exists, exists)
}

/// What an uninstall will do, worked out before anything is touched.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub exe: PathBuf,
    pub owner: Owner,
    /// Whether a refresh schedule is registered and will be removed.
    pub schedule: bool,
    /// A registered schedule left alone, because it starts another copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_for: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<PathBuf>,
    /// The data directory: deleted with `--purge`, otherwise kept.
    pub data_dir: PathBuf,
    pub data_exists: bool,
    pub purge: bool,
    /// Why `--purge` will not delete the data directory, when it will not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purge_refused: Option<String>,
    /// Everything in the profiles directory, valid or not: a damaged
    /// profile is still somebody's credentials.
    pub profiles: Vec<String>,
    /// Entries in the backups directory. Those are credentials too, and can
    /// be the only copy of an account that was never saved.
    pub backups: usize,
    /// A directory that could not be listed might hold anything.
    pub unreadable: bool,
}

impl Plan {
    /// Will this delete the data directory?
    pub fn deletes_data(&self) -> bool {
        self.purge && self.data_exists && self.purge_refused.is_none()
    }

    /// Does carrying this out destroy stored credentials?
    ///
    /// Judged from what is on disk, not from what `list` accepts: a profile
    /// too damaged to list must still not be deleted without a yes.
    pub fn destroys_credentials(&self) -> bool {
        self.deletes_data() && (self.unreadable || !self.profiles.is_empty() || self.backups > 0)
    }
}

/// Whether an uninstall may go ahead without asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consent {
    Proceed,
    /// `--purge` was asked for and cannot be done safely. Stopping before
    /// anything is removed leaves the tool in place to fix it with; going
    /// ahead would remove the binary and keep exactly what was meant to go.
    Blocked(String),
    /// Ask on the terminal, and go ahead only on an explicit yes.
    Ask,
    /// Nobody is there to ask, and nobody said yes in advance.
    Refuse,
}

/// Stored profiles are the only copies of accounts that are not logged in,
/// and deleting them cannot be undone. So a plan that deletes them needs a
/// yes -- given in advance, or typed -- and when there is nobody to type it,
/// a script that forgot `--yes` must not be the thing that decides.
pub fn consent(plan: &Plan, yes: bool, can_ask: bool) -> Consent {
    if plan.purge
        && plan.data_exists
        && let Some(why) = &plan.purge_refused
    {
        return Consent::Blocked(format!(
            "`--purge` will not delete {}: {why}",
            plan.data_dir.display()
        ));
    }
    if !plan.destroys_credentials() || yes {
        Consent::Proceed
    } else if can_ask {
        Consent::Ask
    } else {
        Consent::Refuse
    }
}

/// Everything `ccred` ever creates directly inside its data directory, plus
/// the litter file managers leave behind.
const OURS: &[&str] = &[
    "profiles",
    "state",
    "backups",
    "logs",
    ".DS_Store",
    "desktop.ini",
    "Thumbs.db",
];

/// Decide whether deleting `data_dir` outright is safe.
///
/// `CCRED_HOME` can point anywhere, and a recursive delete of the wrong
/// directory is the one mistake an uninstaller cannot take back. So the
/// directory must not be, or contain, the home directory or Claude Code's,
/// and must hold nothing `ccred` did not put there.
pub fn purge_refusal(
    data_dir: &Path,
    home: &Path,
    claude_dir: &Path,
    entries: &[String],
) -> Option<String> {
    if home.starts_with(data_dir) {
        return Some("it is, or contains, your home directory".into());
    }
    if claude_dir.starts_with(data_dir) || data_dir.starts_with(claude_dir) {
        return Some("it overlaps Claude Code's own directory".into());
    }
    let foreign: Vec<&str> = entries
        .iter()
        .map(String::as_str)
        .filter(|e| !OURS.contains(e))
        .collect();
    if !foreign.is_empty() {
        return Some(format!(
            "it holds files ccred did not create ({})",
            foreign.join(", ")
        ));
    }
    None
}

/// Does this receipt describe the executable that is running?
///
/// One receipt exists per user, whatever else is on the machine. A binary
/// run from anywhere else -- a build directory, a copy -- must not delete the
/// receipt of the installed one, nor be mistaken for it.
pub fn receipt_covers(receipt: &str, exe: &Path) -> bool {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(receipt) else {
        return false;
    };
    let Some(prefix) = json.get("install_prefix").and_then(|v| v.as_str()) else {
        return false;
    };
    let Some(dir) = exe.parent() else {
        return false;
    };
    let norm = |p: &str| {
        let p = p.replace('\\', "/").trim_end_matches('/').to_string();
        if cfg!(windows) {
            p.to_ascii_lowercase()
        } else {
            p
        }
    };
    let dir = norm(&dir.to_string_lossy());
    let prefix = norm(prefix);
    // cargo-dist installs either straight into the prefix or into its `bin`.
    dir == prefix || dir == format!("{prefix}/bin")
}

/// Should this copy remove the registered schedule?
///
/// The schedule is one per user, but it starts one particular binary. When
/// that is a different copy which still exists -- the installed one, while a
/// build is being tried out -- removing it would switch off the refreshes of
/// an installation nobody asked to touch. `registered` is `None` when nothing
/// is registered, and `Some(None)` when a job is registered but its command
/// could not be read back, which is treated as ours.
pub fn schedule_decision(
    registered: Option<Option<PathBuf>>,
    exe: &Path,
) -> (bool, Option<PathBuf>) {
    match registered {
        None => (false, None),
        Some(Some(command)) if command.exists() && !crate::paths::same_file(&command, exe) => {
            (false, Some(command))
        }
        Some(_) => (true, None),
    }
}

/// The names in a directory, sorted, and whether listing it failed. A
/// directory that does not exist is empty, not a failure.
fn entry_names(dir: &Path) -> (Vec<String>, bool) {
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            (names, false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), false),
        Err(_) => (Vec::new(), true),
    }
}

/// Where the release installer leaves its receipt.
fn receipt_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("ccred").join("ccred-receipt.json"))
}

pub fn plan(ctx: &Ctx, purge: bool) -> crate::Result<Plan> {
    let exe = std::env::current_exe().map_err(|source| CcredError::Io {
        path: PathBuf::from("<current exe>"),
        source,
    })?;
    let receipt = receipt_path()
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|text| receipt_covers(&text, &exe)));
    let resolved = std::fs::canonicalize(&exe).ok();
    let owner = owner_of_paths(&exe, resolved.as_deref(), receipt.is_some(), |p| {
        Path::new(p).exists()
    });

    let registered = match super::schedule::backend().status() {
        Ok(crate::schedule::State::Installed(h)) => Some(h.command),
        _ => None,
    };
    let (schedule, schedule_for) = schedule_decision(registered, &exe);

    let paths = ctx.paths();
    let data_dir = paths.ccred_home().to_path_buf();
    let entries: Vec<String> = std::fs::read_dir(&data_dir)
        .map(|dir| {
            dir.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let purge_refused = purge_refusal(&data_dir, paths.home(), paths.claude_config_dir(), &entries);
    let (profiles, profiles_unreadable) = entry_names(&paths.profiles_dir());
    let (backups, backups_unreadable) = entry_names(&paths.backups_dir());

    Ok(Plan {
        exe,
        owner,
        schedule,
        schedule_for,
        receipt,
        data_exists: data_dir.exists(),
        data_dir,
        purge,
        purge_refused,
        profiles,
        backups: backups.len(),
        unreadable: profiles_unreadable || backups_unreadable,
    })
}

/// What was actually done, which can differ from the plan when a step fails.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub schedule_removed: bool,
    pub data_removed: bool,
    pub receipt_removed: bool,
    /// `true` once the binary is gone or scheduled to go.
    pub binary_removed: bool,
    /// Steps that did not work, in words. Reported, never fatal: a partial
    /// uninstall that says what is left beats one that stops at the first
    /// problem and leaves the rest unknown.
    pub problems: Vec<String>,
}

/// Carry out a plan.
///
/// The order matters. The schedule goes first: removing the binary before it
/// would leave a registered job pointing at nothing, failing quietly twice a
/// week for ever. The binary goes last, because it is the thing running.
pub fn execute(plan: &Plan) -> Outcome {
    let mut out = Outcome {
        schedule_removed: false,
        data_removed: false,
        receipt_removed: false,
        binary_removed: false,
        problems: Vec::new(),
    };

    if plan.schedule {
        match crate::schedule::uninstall_checked(super::schedule::backend().as_ref()) {
            Ok(()) => out.schedule_removed = true,
            Err(e) => {
                // Stop here. Removing the binary would leave the job firing
                // at nothing, and deleting the data is not what someone who
                // has to rerun this after fixing the scheduler expects to
                // have already happened.
                out.problems
                    .push(format!("schedule: {e}; nothing else was removed"));
                return out;
            }
        }
    }

    if plan.purge
        && plan.data_exists
        && let Some(why) = &plan.purge_refused
    {
        out.problems.push(format!(
            "{} was not deleted: {why}",
            plan.data_dir.display()
        ));
    }
    if plan.deletes_data() {
        match std::fs::remove_dir_all(&plan.data_dir) {
            Ok(()) => out.data_removed = true,
            Err(e) => out
                .problems
                .push(format!("{}: {e}", plan.data_dir.display())),
        }
    }

    let ours = matches!(plan.owner, Owner::Installer | Owner::Unmanaged);

    if ours && let Some(receipt) = &plan.receipt {
        match std::fs::remove_file(receipt) {
            Ok(()) => {
                out.receipt_removed = true;
                // The receipt sits alone in a directory the installer made.
                if let Some(dir) = receipt.parent() {
                    let _ = std::fs::remove_dir(dir);
                }
            }
            Err(e) => out.problems.push(format!("{}: {e}", receipt.display())),
        }
    }

    if ours {
        match remove_self(&plan.exe) {
            Ok(()) => out.binary_removed = true,
            Err(e) => out.problems.push(format!("{}: {e}", plan.exe.display())),
        }
    }

    out
}

/// Delete the running executable.
///
/// On unix a running file can simply be unlinked. Windows refuses to delete
/// an executable while it runs, so a detached `cmd` waits for this process
/// to exit and removes the file then.
#[cfg(not(windows))]
fn remove_self(exe: &Path) -> std::io::Result<()> {
    std::fs::remove_file(exe)
}

#[cfg(windows)]
fn remove_self(exe: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let path = exe.to_string_lossy();
    // The path goes into a cmd.exe command line, where these characters
    // change meaning. A user profile path containing them is vanishingly
    // rare, and refusing beats handing cmd something it would reinterpret.
    if path.contains(['"', '%', '&', '|', '<', '>', '^']) {
        return Err(std::io::Error::other(
            "the path contains characters cmd.exe would reinterpret; delete it by hand",
        ));
    }
    // `ping` rather than `timeout`, which refuses to run without a console.
    // Three pings is roughly two seconds: ample for this process to exit.
    let script = format!("/c ping -n 3 127.0.0.1 >NUL & del /f /q \"{path}\"");
    Command::new("cmd")
        .raw_arg(script)
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nothing(_: &str) -> bool {
        false
    }

    /// Deleting a file a package manager installed corrupts its record: the
    /// manager still believes the package is there, and the next upgrade or
    /// removal fails. Every one of these must be left to its owner.
    #[test]
    fn a_binary_a_package_manager_owns_is_never_ours_to_delete() {
        let cases = [
            (
                r"C:\Users\x\scoop\apps\ccred\0.2.14\ccred.exe",
                "scoop uninstall ccred",
            ),
            (
                "/home/x/.npm-global/lib/node_modules/@ccred/linux-x64/bin/ccred",
                "npm uninstall -g ccred",
            ),
            (
                "/opt/homebrew/Cellar/ccred/0.2.14/bin/ccred",
                "brew uninstall ccred",
            ),
        ];
        for (path, command) in cases {
            match owner_of(Path::new(path), false, nothing) {
                Owner::PackageManager { command: c, .. } => assert_eq!(c, command, "{path}"),
                other => panic!("{path} was judged {other:?}"),
            }
        }
    }

    #[test]
    fn a_system_binary_names_the_manager_that_installed_it() {
        let apt = owner_of(Path::new("/usr/bin/ccred"), false, |p| {
            p == "/var/lib/dpkg/info/ccred.list"
        });
        assert!(
            matches!(&apt, Owner::PackageManager { command, .. } if command == "sudo apt remove ccred"),
            "{apt:?}"
        );

        let pacman = owner_of(Path::new("/usr/bin/ccred"), false, |p| {
            p == "/var/lib/pacman/local"
        });
        assert!(
            matches!(&pacman, Owner::PackageManager { command, .. } if command.contains("pacman")),
            "{pacman:?}"
        );

        // Unknown owner in /usr/bin: still not ours to delete.
        assert!(matches!(
            owner_of(Path::new("/usr/bin/ccred"), true, nothing),
            Owner::PackageManager { .. }
        ));
    }

    /// The release installer and `cargo install` both use ~/.cargo/bin. The
    /// receipt is what tells them apart, and only one of them is ours.
    #[test]
    fn the_receipt_decides_between_the_installer_and_cargo() {
        let exe = Path::new("/home/x/.cargo/bin/ccred");
        assert_eq!(owner_of(exe, true, nothing), Owner::Installer);
        assert!(matches!(
            owner_of(exe, false, nothing),
            Owner::PackageManager { command, .. } if command == "cargo uninstall ccred"
        ));
    }

    #[test]
    fn a_link_into_a_package_managers_tree_belongs_to_it() {
        let owner = owner_of_paths(
            Path::new("/usr/local/bin/ccred"),
            Some(Path::new("/usr/local/Cellar/ccred/0.2.16/bin/ccred")),
            false,
            nothing,
        );
        assert!(
            matches!(&owner, Owner::PackageManager { command, .. } if command == "brew uninstall ccred"),
            "{owner:?}"
        );
        // A link to an ordinary file changes nothing.
        assert_eq!(
            owner_of_paths(
                Path::new("/home/x/bin/ccred"),
                Some(Path::new("/home/x/tools/ccred")),
                false,
                nothing
            ),
            Owner::Unmanaged
        );
    }

    #[test]
    fn a_hand_placed_binary_is_ours() {
        assert_eq!(
            owner_of(Path::new("/home/x/bin/ccred"), false, nothing),
            Owner::Unmanaged
        );
    }

    #[test]
    fn only_a_purge_with_profiles_destroys_credentials() {
        let base = Plan {
            exe: "/x/ccred".into(),
            owner: Owner::Unmanaged,
            schedule: false,
            schedule_for: None,
            receipt: None,
            data_dir: "/x/.ccred".into(),
            data_exists: true,
            purge: false,
            purge_refused: None,
            profiles: vec!["work".into()],
            backups: 0,
            unreadable: false,
        };
        assert!(!base.destroys_credentials(), "no purge, nothing destroyed");
        let purge = Plan {
            purge: true,
            ..base.clone()
        };
        assert!(purge.destroys_credentials());
        let refused = Plan {
            purge_refused: Some("no".into()),
            ..purge.clone()
        };
        assert!(!refused.deletes_data(), "a refused purge deletes nothing");
        let empty = Plan {
            profiles: vec![],
            ..purge.clone()
        };
        assert!(!empty.destroys_credentials());

        // Found by running it: a backup, or a directory that could not be
        // listed, is credentials too -- a purge of either needs a yes.
        let backups_only = Plan {
            backups: 1,
            ..empty.clone()
        };
        assert!(backups_only.destroys_credentials());
        let unreadable = Plan {
            unreadable: true,
            ..empty
        };
        assert!(unreadable.destroys_credentials());
    }

    #[test]
    fn a_profile_too_damaged_to_list_still_counts() {
        let dir = tempfile::TempDir::new().unwrap();
        // No metadata at all: `ProfileRepo::list` would skip this.
        std::fs::create_dir(dir.path().join("broken")).unwrap();
        assert_eq!(entry_names(dir.path()), (vec!["broken".to_string()], false));
        assert_eq!(entry_names(&dir.path().join("absent")), (vec![], false));
    }

    #[test]
    fn deleting_profiles_needs_a_yes_and_nothing_else_does() {
        let keep = Plan {
            exe: "/x/ccred".into(),
            owner: Owner::Unmanaged,
            schedule: true,
            schedule_for: None,
            receipt: None,
            data_dir: "/x/.ccred".into(),
            data_exists: true,
            purge: false,
            purge_refused: None,
            profiles: vec!["work".into()],
            backups: 0,
            unreadable: false,
        };
        let purge = Plan {
            purge: true,
            ..keep.clone()
        };

        // Keeping the profiles is harmless: no question, even unattended.
        assert_eq!(consent(&keep, false, false), Consent::Proceed);
        assert_eq!(consent(&keep, false, true), Consent::Proceed);

        // Deleting them: a yes given in advance goes through...
        assert_eq!(consent(&purge, true, false), Consent::Proceed);
        // ...otherwise ask when someone is there...
        assert_eq!(consent(&purge, false, true), Consent::Ask);
        // ...and refuse when nobody is.
        assert_eq!(consent(&purge, false, false), Consent::Refuse);

        // A purge that cannot be done safely stops everything, yes or not.
        let blocked = Plan {
            purge_refused: Some("it holds photos".into()),
            ..purge
        };
        assert!(matches!(
            consent(&blocked, true, false),
            Consent::Blocked(why) if why.contains("photos")
        ));
    }

    #[test]
    fn a_receipt_only_covers_the_binary_it_installed() {
        let receipt = if cfg!(windows) {
            r#"{"install_prefix":"C:\\Users\\x\\.cargo","install_layout":"cargo-home"}"#
        } else {
            r#"{"install_prefix":"/home/x/.cargo","install_layout":"cargo-home"}"#
        };
        let (installed, build) = if cfg!(windows) {
            (
                r"c:\users\X\.cargo\bin\ccred.exe",
                r"C:\src\ccred\target\debug\ccred.exe",
            )
        } else {
            (
                "/home/x/.cargo/bin/ccred",
                "/home/x/src/ccred/target/debug/ccred",
            )
        };
        assert!(receipt_covers(receipt, Path::new(installed)));
        assert!(
            !receipt_covers(receipt, Path::new(build)),
            "a build would have deleted the installed copy's receipt"
        );
        assert!(!receipt_covers("not json", Path::new(installed)));
        assert!(!receipt_covers("{}", Path::new(installed)));
    }

    #[test]
    fn a_schedule_that_starts_another_copy_is_left_alone() {
        let here = std::env::current_exe().unwrap();
        let other = here.parent().unwrap().to_path_buf(); // exists, and is not us

        assert_eq!(schedule_decision(None, &here), (false, None));
        assert_eq!(
            schedule_decision(Some(Some(here.clone())), &here),
            (true, None)
        );
        assert_eq!(
            schedule_decision(Some(Some(other.clone())), &here),
            (false, Some(other))
        );
        // A job whose binary is gone serves nobody: remove it.
        assert_eq!(
            schedule_decision(Some(Some("/nonexistent/ccred".into())), &here),
            (true, None)
        );
        // Registered, command unreadable: ours to remove.
        assert_eq!(schedule_decision(Some(None), &here), (true, None));
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_normal_data_directory_may_be_purged() {
        let refusal = purge_refusal(
            Path::new("/home/x/.ccred"),
            Path::new("/home/x"),
            Path::new("/home/x/.claude"),
            &names(&["profiles", "state", "backups", "logs"]),
        );
        assert_eq!(refusal, None);
    }

    /// `CCRED_HOME` can point anywhere. A recursive delete aimed at the home
    /// directory, or at Claude Code's, must be impossible however it is set.
    #[test]
    fn a_purge_never_reaches_the_home_or_claude_directory() {
        let home = Path::new("/home/x");
        let claude = Path::new("/home/x/.claude");
        for data in [
            "/home/x",
            "/home",
            "/",
            "/home/x/.claude",
            "/home/x/.claude/sub",
        ] {
            assert!(
                purge_refusal(Path::new(data), home, claude, &names(&["profiles"])).is_some(),
                "{data} would have been deleted"
            );
        }
    }

    #[test]
    fn a_directory_holding_anything_else_is_not_purged() {
        let refusal = purge_refusal(
            Path::new("/srv/data"),
            Path::new("/home/x"),
            Path::new("/home/x/.claude"),
            &names(&["profiles", "photos"]),
        );
        assert!(refusal.is_some_and(|why| why.contains("photos")));
    }
}
