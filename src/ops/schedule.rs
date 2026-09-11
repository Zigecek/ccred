//! Building a schedule spec from the running environment.

use std::path::PathBuf;

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
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into());

    Ok(ScheduleSpec::new(
        exe,
        ctx.paths().home().to_path_buf(),
        ctx.paths().log_dir(),
        user,
    ))
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
