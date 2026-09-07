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
