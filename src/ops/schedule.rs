//! Building a schedule spec from the running environment.

use std::path::{Path, PathBuf};

use super::Ctx;
use crate::error::CcredError;
use crate::schedule::{ScheduleSpec, Scheduler, detect};

/// The spec for this machine: this binary, this home, this user.
pub fn spec_for(ctx: &Ctx) -> crate::Result<ScheduleSpec> {
    // The scheduler needs an absolute path -- it runs with a minimal
    // environment and almost never inherits a useful PATH.
    let exe = std::env::current_exe().map_err(|source| CcredError::Io {
        path: PathBuf::from("<current exe>"),
        source,
    })?;
    let exe = stable_exe(&exe, |p| p.exists());
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into());

    Ok(ScheduleSpec::new(
        exe,
        ctx.paths().home().to_path_buf(),
        ctx.paths().log_dir(),
        user,
    )
    .with_locations(ctx.paths().overrides()))
}

/// The path to register: one that survives the package manager upgrading
/// the binary.
///
/// On Linux the running executable is reported with every link resolved, so
/// a Homebrew install shows up as `Cellar/ccred/<version>/bin/ccred`. That
/// directory is deleted by the upgrade after next, and the job with it --
/// started on time, failing at once, with nothing to show for it. Homebrew
/// keeps `<prefix>/opt/ccred` pointing at the current version, and Scoop keeps
/// `apps/ccred/current`; those are registered instead when they exist.
pub fn stable_exe(exe: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let parts: Vec<&std::ffi::OsStr> = exe.iter().collect();
    let is = |i: usize, name: &str| {
        parts
            .get(i)
            .is_some_and(|p| p.to_string_lossy().eq_ignore_ascii_case(name))
    };
    let prefix = |end: usize| parts[..end].iter().collect::<PathBuf>();
    let n = parts.len();

    // <prefix>/Cellar/ccred/<version>/bin/ccred -> <prefix>/opt/ccred/bin/ccred
    if n >= 5 && is(n - 5, "Cellar") && is(n - 4, "ccred") && is(n - 2, "bin") {
        let candidate = prefix(n - 5)
            .join("opt")
            .join("ccred")
            .join("bin")
            .join(parts[n - 1]);
        if exists(&candidate) {
            return candidate;
        }
    }
    // <root>/apps/ccred/<version>/ccred.exe -> <root>/apps/ccred/current/ccred.exe
    if n >= 4 && is(n - 4, "apps") && is(n - 3, "ccred") && !is(n - 2, "current") {
        let candidate = prefix(n - 2).join("current").join(parts[n - 1]);
        if exists(&candidate) {
            return candidate;
        }
    }
    exe.to_path_buf()
}

pub fn backend() -> Box<dyn Scheduler> {
    detect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    fn ctx_in(home: &std::path::Path) -> Ctx {
        Ctx::with_paths(Paths::with_overrides(home.to_path_buf(), None, None))
    }

    #[test]
    fn an_upgrade_does_not_orphan_the_registered_binary() {
        let all = |_: &Path| true;
        assert_eq!(
            stable_exe(
                Path::new("/home/linuxbrew/.linuxbrew/Cellar/ccred/0.2.17/bin/ccred"),
                all
            ),
            PathBuf::from("/home/linuxbrew/.linuxbrew/opt/ccred/bin/ccred")
        );
        // Backslashes only separate components on Windows.
        #[cfg(windows)]
        assert_eq!(
            stable_exe(
                Path::new(r"C:\Users\x\scoop\apps\ccred\0.2.17\ccred.exe"),
                all
            ),
            Path::new(r"C:\Users\x\scoop\apps\ccred")
                .join("current")
                .join("ccred.exe")
        );
    }

    #[test]
    fn a_stable_path_is_only_used_when_it_exists() {
        let none = |_: &Path| false;
        let cellar = Path::new("/usr/local/Cellar/ccred/0.2.17/bin/ccred");
        assert_eq!(stable_exe(cellar, none), cellar);
    }

    #[test]
    fn ordinary_paths_are_left_alone() {
        let all = |_: &Path| true;
        for p in [
            "/usr/bin/ccred",
            "/home/x/.cargo/bin/ccred",
            r"C:\Users\x\scoop\apps\ccred\current\ccred.exe",
            "/opt/Cellar/other/1.0/bin/ccred",
        ] {
            assert_eq!(stable_exe(Path::new(p), all), Path::new(p), "{p}");
        }
    }

    /// The scheduler runs with a minimal environment and almost never
    /// inherits a useful PATH, so a relative command would register cleanly
    /// and then fail to start every time -- the exact silent-failure shape
    /// this module exists to prevent.
    #[test]
    fn the_registered_command_is_an_absolute_path() {
        let home = tempfile::TempDir::new().unwrap();
        let spec = spec_for(&ctx_in(home.path())).unwrap();
        assert!(spec.exe.is_absolute(), "{:?}", spec.exe);
        assert!(
            spec.home.is_absolute() && spec.log_dir.is_absolute(),
            "{spec:?}"
        );
    }

    /// `USER` is unset under some service managers. Falling over there would
    /// mean scheduling could not be installed at all from a context that has
    /// every other right it needs.
    #[test]
    fn a_missing_user_name_does_not_stop_a_spec_being_built() {
        let home = tempfile::TempDir::new().unwrap();
        let spec = spec_for(&ctx_in(home.path())).unwrap();
        assert!(!spec.user.is_empty(), "the principal must always be named");
    }
}
