//! Where everything lives, on all three platforms.
//!
//! Written by hand rather than using the `directories` crate: we need only a
//! handful of paths, none of those crates gets both macOS `~/Library/Logs` and
//! the XDG state dir right, and in a tool that holds tokens, forty lines you
//! can read in one screen beat a transitive dependency.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::CcredError;
use crate::validate::ProfileName;

/// Derived paths. Built from the environment; injectable in tests.
#[derive(Debug, Clone)]
pub struct Paths {
    home: PathBuf,
    ccred_home: PathBuf,
    claude_config_dir: PathBuf,
    claude_config_file: PathBuf,
    desktop_dir: PathBuf,
    overrides: Locations,
}

/// Locations chosen instead of the defaults, by flag or by environment.
///
/// Kept apart from the resolved paths because some consumers need to know
/// what was *chosen*, not just where things ended up: a scheduled job does not
/// inherit the shell that set `CCRED_HOME`, so the choice has to be written
/// into the job, and a spawned `claude` has to be told the same configuration
/// directory this program is using.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Locations {
    pub ccred_home: Option<PathBuf>,
    pub claude_config_dir: Option<PathBuf>,
}

fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var_os(key) {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => None,
    }
}

/// Do two paths name the same file? Links and case are resolved where the
/// file system can say; otherwise the paths are compared as written.
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// A relative location would be read against whatever directory the process
/// starts in -- the user's shell today, the scheduler's choice tomorrow.
fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

fn home_dir() -> Option<PathBuf> {
    env_path("HOME").or_else(|| env_path("USERPROFILE"))
}

impl Paths {
    pub fn from_env() -> crate::Result<Self> {
        Self::resolve(Locations::default())
    }

    /// Explicit locations win over the environment, which wins over the
    /// defaults.
    pub fn resolve(explicit: Locations) -> crate::Result<Self> {
        Self::resolve_from(home_dir().map(absolute), explicit)
    }

    /// The part that has no environment in it, so a test can say "this
    /// machine has no home" without touching the process it runs in.
    fn resolve_from(home: Option<PathBuf>, explicit: Locations) -> crate::Result<Self> {
        let chosen = Locations {
            ccred_home: explicit.ccred_home.or_else(|| env_path("CCRED_HOME")),
            claude_config_dir: explicit
                .claude_config_dir
                .or_else(|| env_path("CLAUDE_CONFIG_DIR")),
        };
        // A home is only needed for the directories nobody named. With no
        // HOME and no USERPROFILE -- a container with a scrubbed environment,
        // a service unit that sets neither -- naming both was already the
        // answer, and ccred refused anyway while advising exactly that.
        let home = match home {
            Some(home) => home,
            None => {
                let (Some(ccred_home), Some(_)) = (&chosen.ccred_home, &chosen.claude_config_dir)
                else {
                    return Err(CcredError::Io {
                        path: PathBuf::from("$HOME"),
                        source: std::io::Error::other(concat!(
                            "cannot determine the home directory: neither HOME nor ",
                            "USERPROFILE is set. Set one, or name both directories ",
                            "with --ccred-home and --claude-config-dir"
                        )),
                    });
                };
                // Recorded as "home" only for what is left that reads it: a
                // scheduled job's working directory, the Desktop's default
                // location, and the path `--purge` refuses to delete.
                let ccred_home = absolute(ccred_home.clone());
                ccred_home
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or(ccred_home)
            }
        };
        let mut paths = Self::with_overrides(
            home,
            chosen.ccred_home.map(absolute),
            chosen.claude_config_dir.map(absolute),
        );
        // The Desktop's directory follows the platform's config-dir variable
        // when one is set, and those are read here, once, for the reason
        // `log_dir` gives: a `Paths` built for a sandbox must not quietly
        // point at the real user's directory through the process environment.
        let base = if cfg!(target_os = "windows") {
            env_path("APPDATA")
        } else if cfg!(target_os = "macos") {
            None
        } else {
            env_path("XDG_CONFIG_HOME")
        };
        if let Some(base) = base {
            paths.desktop_dir = absolute(base).join(DESKTOP_DIR_NAME);
        }
        if cfg!(target_os = "windows") {
            paths.desktop_dir =
                windows_desktop_dir(&paths.desktop_dir, packaged_desktop_dirs(), |dir| {
                    std::fs::metadata(dir.join(DESKTOP_CONFIG_FILE))
                        .and_then(|m| m.modified())
                        .ok()
                });
        }
        Ok(paths)
    }

    /// Constructor for tests and for explicit directory choices.
    pub fn with_overrides(
        home: PathBuf,
        ccred_home: Option<PathBuf>,
        claude_config_dir: Option<PathBuf>,
    ) -> Self {
        let overrides = Locations {
            ccred_home: ccred_home.clone(),
            claude_config_dir: claude_config_dir.clone(),
        };
        let ccred_home = ccred_home.unwrap_or_else(|| home.join(".ccred"));

        // Mind this detail: without CLAUDE_CONFIG_DIR, `.claude.json` sits
        // NEXT TO the `.claude` directory, not inside it. Setting the variable
        // moves it inside. This exact distinction was the bug in the
        // predecessor's systemd unit, whose ReadWritePaths did not cover
        // `~/.claude.json`.
        let (claude_config_dir, claude_config_file) = match claude_config_dir {
            Some(dir) => {
                let file = dir.join(".claude.json");
                (dir, file)
            }
            None => (home.join(".claude"), home.join(".claude.json")),
        };

        let desktop_dir = default_desktop_dir(&home);

        Paths {
            home,
            ccred_home,
            claude_config_dir,
            claude_config_file,
            desktop_dir,
            overrides,
        }
    }

    /// What was chosen instead of the defaults, if anything.
    pub fn overrides(&self) -> &Locations {
        &self.overrides
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn ccred_home(&self) -> &Path {
        &self.ccred_home
    }

    /// The live directory a plain `claude` reads from.
    pub fn claude_config_dir(&self) -> &Path {
        &self.claude_config_dir
    }

    /// `.claude.json` -- 60 kB of foreign state. Patch it, never regenerate it.
    pub fn claude_config_file(&self) -> &Path {
        &self.claude_config_file
    }

    pub fn live_credentials(&self) -> PathBuf {
        credentials_in(&self.claude_config_dir)
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.ccred_home.join("profiles")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.ccred_home.join("state")
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.ccred_home.join("backups")
    }

    pub fn active_pointer(&self) -> PathBuf {
        self.state_dir().join("current")
    }

    pub fn switch_journal(&self) -> PathBuf {
        self.state_dir().join("switch.journal")
    }

    /// Present while an interrupted switch could not be settled; see
    /// [`crate::journal::SwitchJournal::mark_unsettled`].
    pub fn unsettled_switch(&self) -> PathBuf {
        self.state_dir().join("switch.unsettled")
    }

    pub fn last_run(&self) -> PathBuf {
        self.state_dir().join("last-run.json")
    }

    /// One profile's directory. The name must already be validated; the join
    /// result is checked again for escapes (belt and braces).
    pub fn profile_dir(&self, name: &ProfileName) -> crate::Result<PathBuf> {
        confine(&self.profiles_dir(), name)
    }

    pub fn profile_credentials(&self, name: &ProfileName) -> crate::Result<PathBuf> {
        Ok(credentials_in(&self.profile_dir(name)?))
    }

    /// The profile's stored `oauthAccount` blob, restored verbatim on switch.
    pub fn profile_oauth_account(&self, name: &ProfileName) -> crate::Result<PathBuf> {
        Ok(self.profile_dir(name)?.join("oauth-account.json"))
    }

    /// Last known-good copy. Only advanced after successful validation, so
    /// unlike the rotating backups it cannot all be rotten at once.
    pub fn profile_lkg(&self, name: &ProfileName) -> crate::Result<PathBuf> {
        Ok(self.profile_dir(name)?.join(".credentials.json.lkg"))
    }

    /// Where the record of unattended runs lives.
    ///
    /// Under `ccred_home`, with everything else this tool owns, rather than
    /// in the platform's log convention. The conventional directories are
    /// found through `LOCALAPPDATA` and `XDG_STATE_HOME`, which are read from
    /// the process environment -- so a `Paths` built for a sandbox, or
    /// relocated with `CCRED_HOME`, wrote its log to the real user's
    /// directory anyway. The test suite was quietly appending to it.
    pub fn log_dir(&self) -> PathBuf {
        self.ccred_home.join("logs")
    }

    /// Claude Desktop's own data directory -- Electron's `userData`, where
    /// the app keeps its login, its cookies and its index of Code sessions.
    ///
    /// `~/.config/Claude` on Linux, `~/Library/Application Support/Claude`
    /// on macOS, `%APPDATA%\Claude` on Windows. Nothing inside is ours to
    /// read beyond one plaintext key; see `desktop`.
    pub fn desktop_dir(&self) -> &Path {
        &self.desktop_dir
    }

    /// Where the Desktop directories of profiles that are not active wait.
    pub fn desktop_store_dir(&self) -> PathBuf {
        self.ccred_home.join("desktop")
    }

    /// One profile's parked Desktop directory.
    pub fn desktop_profile_dir(&self, name: &ProfileName) -> crate::Result<PathBuf> {
        confine(&self.desktop_store_dir(), name)
    }
}

/// The last path component of the Desktop's data directory on every platform.
const DESKTOP_DIR_NAME: &str = "Claude";

/// The file every Claude Desktop data directory has, and the one this tool
/// reads an account out of. Its presence is what tells a real directory from
/// a path that merely could be one.
const DESKTOP_CONFIG_FILE: &str = "config.json";

/// Which of the two places a Windows Claude Desktop keeps its data.
///
/// There are two installs, and they are not a Store/not-Store distinction.
/// The classic installer writes to `%APPDATA%\Claude`. The MSIX package --
/// which arrives from the Store *or* from a downloaded installer, and reports
/// `SignatureKind: Developer` in the second case -- runs with filesystem
/// redirection: it writes what it thinks is `%APPDATA%\Claude`, and Windows
/// puts it under `%LOCALAPPDATA%\Packages\Claude_<publisher>\LocalCache/// Roaming\Claude`. A process outside the package, which is what ccred is,
/// sees only the redirected path.
///
/// Measured on a machine running Claude 2.110.1.0 installed by hand: the
/// classic directory did not exist at all, and the packaged one held the
/// `config.json`, the `lockfile` and everything else. Looking only where the
/// contributor's Linux machine suggested would have reported "no Claude
/// Desktop here" on a machine that plainly has one.
///
/// So: whichever holds a `config.json`, and the most recently written one
/// when both do -- a machine that has been through both installs keeps the
/// older directory, and the app is whichever wrote last. With neither, the
/// classic path stands, so a message names the conventional place.
fn windows_desktop_dir(
    classic: &Path,
    packaged: Vec<PathBuf>,
    changed_at: impl Fn(&Path) -> Option<SystemTime>,
) -> PathBuf {
    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    for dir in std::iter::once(classic.to_path_buf()).chain(packaged) {
        if let Some(at) = changed_at(&dir) {
            candidates.push((at, dir));
        }
    }
    candidates.sort_by_key(|(at, _)| *at);
    candidates
        .pop()
        .map(|(_, dir)| dir)
        .unwrap_or_else(|| classic.to_path_buf())
}

/// Every packaged Claude the local account has installed.
///
/// The directory under `Packages` is a package family name: the app's name,
/// an underscore, and a hash of the publisher. The hash is stable for
/// Anthropic but is not something to hard-code, so the family is matched by
/// its `Claude_` prefix.
fn packaged_desktop_dirs() -> Vec<PathBuf> {
    let Some(local) = env_path("LOCALAPPDATA") else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(absolute(local).join("Packages")) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(concat!("Claude", "_"))
        })
        .map(|e| {
            e.path()
                .join("LocalCache")
                .join("Roaming")
                .join(DESKTOP_DIR_NAME)
        })
        .collect();
    found.sort();
    found
}

/// The Desktop's directory when no environment variable relocates it.
fn default_desktop_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        home.join("AppData").join("Roaming").join(DESKTOP_DIR_NAME)
    } else if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join(DESKTOP_DIR_NAME)
    } else {
        home.join(".config").join(DESKTOP_DIR_NAME)
    }
}

/// `.credentials.json` inside a given config directory.
pub fn credentials_in(config_dir: &Path) -> PathBuf {
    config_dir.join(".credentials.json")
}

/// The lock target Claude Code also takes before every credential change.
pub fn storage_write_lock_target(config_dir: &Path) -> PathBuf {
    config_dir.join(".storage-write")
}

/// Join a root with a profile name and verify the result stays inside.
///
/// [`crate::validate::validate_profile_name`] already rejects suspicious names;
/// this is a second layer in case that validation is ever bypassed.
pub fn confine(root: &Path, name: &ProfileName) -> crate::Result<PathBuf> {
    let joined = root.join(name.as_str());

    // Do not rely on canonicalize -- the directory may not exist yet. It is
    // enough to verify the path grew by exactly one normal component.
    let extra: Vec<_> = joined
        .strip_prefix(root)
        .map_err(|_| CcredError::PathEscape {
            name: name.as_str().to_string(),
        })?
        .components()
        .collect();

    let ok = extra.len() == 1 && matches!(extra[0], std::path::Component::Normal(_));
    if !ok {
        return Err(CcredError::PathEscape {
            name: name.as_str().to_string(),
        });
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::validate_profile_name;

    fn paths() -> Paths {
        Paths::with_overrides(PathBuf::from("/home/user"), None, None)
    }

    /// A container run with a scrubbed environment has no HOME and no
    /// USERPROFILE. Naming both directories is the whole answer, and ccred
    /// refused anyway -- while advising exactly that.
    #[test]
    fn no_home_is_only_a_problem_for_a_directory_nobody_named() {
        let both = Locations {
            ccred_home: Some(PathBuf::from("/data/ccred")),
            claude_config_dir: Some(PathBuf::from("/data/claude")),
        };
        let p = Paths::resolve_from(None, both).expect("both were named");
        // Compared through `absolute`, which on Windows puts a drive letter
        // on a path that starts with a separator.
        assert_eq!(p.ccred_home(), absolute(PathBuf::from("/data/ccred")));
        assert_eq!(
            p.claude_config_dir(),
            absolute(PathBuf::from("/data/claude"))
        );
        // What is left calling itself "home" is the data directory's parent:
        // a working directory for a scheduled job, and the path `--purge`
        // refuses to delete. Never the data directory itself.
        assert_eq!(p.home(), absolute(PathBuf::from("/data")));

        let only_one = Locations {
            ccred_home: Some(PathBuf::from("/data/ccred")),
            claude_config_dir: None,
        };
        let err = Paths::resolve_from(None, only_one).unwrap_err();
        let said = format!("{err} {}", std::error::Error::source(&err).unwrap());
        assert!(said.contains("USERPROFILE"), "{said}");
        assert!(said.contains("--claude-config-dir"), "{said}");
    }

    #[test]
    fn claude_json_sits_next_to_the_dir_by_default() {
        let p = paths();
        assert_eq!(p.claude_config_dir(), Path::new("/home/user/.claude"));
        assert_eq!(p.claude_config_file(), Path::new("/home/user/.claude.json"));
    }

    #[test]
    fn claude_config_dir_override_moves_the_json_inside() {
        let p = Paths::with_overrides(
            PathBuf::from("/home/user"),
            None,
            Some(PathBuf::from("/tmp/alt")),
        );
        assert_eq!(p.claude_config_dir(), Path::new("/tmp/alt"));
        assert_eq!(p.claude_config_file(), Path::new("/tmp/alt/.claude.json"));
        assert_eq!(
            p.live_credentials(),
            Path::new("/tmp/alt/.credentials.json")
        );
    }

    /// Windows keeps a packaged Claude Desktop somewhere else entirely, and
    /// a machine that has one usually has nothing at the classic path. Found
    /// on a real install of 2.110.1.0: `%APPDATA%` held no Claude directory
    /// at all, and everything was under
    /// `%LOCALAPPDATA%\Packages\Claude_<publisher>\LocalCache\Roaming`.
    /// Looking only at the classic path reported "no Claude Desktop here" on
    /// a machine that plainly had one.
    #[test]
    fn a_packaged_desktop_is_found_where_windows_redirects_it() {
        let classic = PathBuf::from("/appdata/Claude");
        let packaged = PathBuf::from("/local/Packages/Claude_abc/LocalCache/Roaming/Claude");
        let at = |secs: u64| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        let only = |real: PathBuf, when: u64| move |dir: &Path| (dir == real).then(|| at(when));

        // The case that was broken: nothing classic, everything packaged.
        assert_eq!(
            windows_desktop_dir(&classic, vec![packaged.clone()], only(packaged.clone(), 10)),
            packaged
        );

        // A classic install on its own still wins.
        assert_eq!(
            windows_desktop_dir(&classic, vec![packaged.clone()], only(classic.clone(), 10)),
            classic
        );

        // Both real: the one written last, since a machine that has been
        // through both installs keeps the older directory around.
        let both_newer_packaged = windows_desktop_dir(&classic, vec![packaged.clone()], |dir| {
            Some(if dir == classic { at(10) } else { at(20) })
        });
        assert_eq!(both_newer_packaged, packaged);
        let both_newer_classic = windows_desktop_dir(&classic, vec![packaged.clone()], |dir| {
            Some(if dir == classic { at(30) } else { at(20) })
        });
        assert_eq!(both_newer_classic, classic);

        // Neither: the conventional path stands, so a message names it.
        assert_eq!(
            windows_desktop_dir(&classic, vec![packaged], |_| None),
            classic
        );
    }

    #[test]
    fn the_desktop_directory_is_the_platforms_and_its_parking_is_ours() {
        let p = paths();
        let expected = if cfg!(target_os = "windows") {
            "/home/user/AppData/Roaming/Claude"
        } else if cfg!(target_os = "macos") {
            "/home/user/Library/Application Support/Claude"
        } else {
            "/home/user/.config/Claude"
        };
        assert_eq!(p.desktop_dir(), Path::new(expected));
        let name = validate_profile_name("work").unwrap();
        assert_eq!(
            p.desktop_profile_dir(&name).unwrap(),
            Path::new("/home/user/.ccred/desktop/work")
        );
    }

    #[test]
    fn profile_paths_land_under_the_profiles_dir() {
        let p = paths();
        let name = validate_profile_name("work").unwrap();
        assert_eq!(
            p.profile_dir(&name).unwrap(),
            Path::new("/home/user/.ccred/profiles/work")
        );
        assert_eq!(
            p.profile_credentials(&name).unwrap(),
            Path::new("/home/user/.ccred/profiles/work/.credentials.json")
        );
    }

    #[test]
    fn lock_target_matches_claude_codes_own() {
        assert_eq!(
            storage_write_lock_target(Path::new("/home/user/.claude")),
            Path::new("/home/user/.claude/.storage-write")
        );
    }

    #[test]
    fn confine_accepts_a_plain_name() {
        let name = validate_profile_name("personal").unwrap();
        let root = Path::new("/root");
        assert_eq!(confine(root, &name).unwrap(), Path::new("/root/personal"));
    }
}
