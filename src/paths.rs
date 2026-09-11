//! Where everything lives, on all three platforms.
//!
//! Written by hand rather than using the `directories` crate: we need only a
//! handful of paths, none of those crates gets both macOS `~/Library/Logs` and
//! the XDG state dir right, and in a tool that holds tokens, forty lines you
//! can read in one screen beat a transitive dependency.

use std::path::{Path, PathBuf};

use crate::error::CcredError;
use crate::validate::ProfileName;

/// Derived paths. Built from the environment; injectable in tests.
#[derive(Debug, Clone)]
pub struct Paths {
    home: PathBuf,
    ccred_home: PathBuf,
    claude_config_dir: PathBuf,
    claude_config_file: PathBuf,
}

fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var_os(key) {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => None,
    }
}

fn home_dir() -> Option<PathBuf> {
    env_path("HOME").or_else(|| env_path("USERPROFILE"))
}

impl Paths {
    pub fn from_env() -> crate::Result<Self> {
        let home = home_dir().ok_or_else(|| CcredError::Io {
            path: PathBuf::from("$HOME"),
            source: std::io::Error::other("cannot determine the home directory"),
        })?;
        Ok(Self::with_overrides(
            home.clone(),
            env_path("CCRED_HOME"),
            env_path("CLAUDE_CONFIG_DIR"),
        ))
    }

    /// Constructor for tests and for explicit directory choices.
    pub fn with_overrides(
        home: PathBuf,
        ccred_home: Option<PathBuf>,
        claude_config_dir: Option<PathBuf>,
    ) -> Self {
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

        Paths {
            home,
            ccred_home,
            claude_config_dir,
            claude_config_file,
        }
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
