//! Installing the background refresh on each platform.
//!
//! # The rule this module exists to enforce
//!
//! Its predecessor installed a systemd timer that combined `Persistent=true`
//! with a monotonic `OnBootSec=`. That combination is legal, `systemctl`
//! reported the timer as `active`, and it would never have fired -- because
//! `Persistent=` only applies to calendar timers. The failure was invisible:
//! everything looked installed and nothing ever ran.
//!
//! Every backend here can fail that way in its own dialect. launchd accepts a
//! `Weekday` of 9 and silently never fires. A Windows task defaults to
//! `DisallowStartIfOnBatteries`, so on a laptop it reports Ready and never
//! runs. Rather than enumerate those traps, [`install_checked`] asserts the
//! one property they all violate: **immediately after installing, `status()`
//! must be able to name a concrete next run.** If it cannot, the install is
//! rolled back and reported as a failure.
//!
//! # Rendering is pure
//!
//! [`Scheduler::render`] never touches the disk, so `ccred schedule install
//! --dry-run` can print exactly what would be registered -- worth having in a
//! tool that holds credentials -- and so the generated unit, plist and task
//! XML can all be tested from any host.

pub mod launchd;
pub mod systemd;
pub mod taskschd;

use std::path::PathBuf;

use serde::Serialize;

use crate::error::CcredError;

/// Days the refresh runs. Two runs a week means a gap of three or four days,
/// which fits comfortably inside every refresh window we have seen while
/// keeping the number of calls low.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    /// `systemd.time(7)` spelling.
    pub fn systemd(self) -> &'static str {
        match self {
            Weekday::Mon => "Mon",
            Weekday::Tue => "Tue",
            Weekday::Wed => "Wed",
            Weekday::Thu => "Thu",
            Weekday::Fri => "Fri",
            Weekday::Sat => "Sat",
            Weekday::Sun => "Sun",
        }
    }

    /// launchd `Weekday`: 0 and 7 both mean Sunday.
    pub fn launchd(self) -> u8 {
        match self {
            Weekday::Mon => 1,
            Weekday::Tue => 2,
            Weekday::Wed => 3,
            Weekday::Thu => 4,
            Weekday::Fri => 5,
            Weekday::Sat => 6,
            Weekday::Sun => 0,
        }
    }

    /// Task Scheduler element name.
    pub fn windows(self) -> &'static str {
        match self {
            Weekday::Mon => "Monday",
            Weekday::Tue => "Tuesday",
            Weekday::Wed => "Wednesday",
            Weekday::Thu => "Thursday",
            Weekday::Fri => "Friday",
            Weekday::Sat => "Saturday",
            Weekday::Sun => "Sunday",
        }
    }
}

/// Everything a backend needs to render its artifacts.
#[derive(Debug, Clone)]
pub struct ScheduleSpec {
    /// Absolute path to the `ccred` binary.
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub days: Vec<Weekday>,
    pub hour: u8,
    pub minute: u8,
    pub home: PathBuf,
    pub log_dir: PathBuf,
    /// The user account, for the Windows task principal.
    pub user: String,
}

impl ScheduleSpec {
    /// The default schedule: twice a week, at a fixed minute past the hour.
    pub fn new(exe: PathBuf, home: PathBuf, log_dir: PathBuf, user: String) -> Self {
        ScheduleSpec {
            exe,
            args: vec!["refresh".into(), "--if-older-than".into(), "48".into()],
            days: vec![Weekday::Mon, Weekday::Thu],
            hour: 9,
            minute: 17,
            home,
            log_dir,
            user,
        }
    }

    pub fn command_line(&self) -> String {
        let mut parts = vec![quote_if_needed(&self.exe.to_string_lossy())];
        parts.extend(self.args.iter().map(|a| quote_if_needed(a)));
        parts.join(" ")
    }
}

fn quote_if_needed(s: &str) -> String {
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

/// A file a backend would write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFile {
    pub path: PathBuf,
    pub contents: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Systemd,
    Launchd,
    TaskScheduler,
}

/// Something that will keep the job from ever running, even though it is
/// registered. Each of these has bitten a real installation somewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Warning {
    /// Linux: without lingering the timer stops when the user logs out.
    LingerDisabled,
    /// macOS: a LaunchAgent only runs inside a GUI login session.
    NoGuiSession,
    /// macOS: someone turned it off in Login Items.
    DisabledByUser,
    /// Windows: the default refuses to start on battery.
    OnBatteryBlocked,
    /// Registered, but no next run -- the failure this module exists to catch.
    RegisteredButNeverFires,
    BinaryMissing(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Health {
    pub enabled: bool,
    /// `None` here is a red flag, not a detail.
    pub next_run: Option<String>,
    pub last_run: Option<String>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum State {
    NotInstalled,
    Unsupported {
        reason: String,
        remedy: Option<String>,
    },
    Installed(Health),
}

pub trait Scheduler {
    fn backend(&self) -> Backend;

    /// Pure: what would be written, without writing it.
    fn render(&self, spec: &ScheduleSpec) -> crate::Result<Vec<RenderedFile>>;

    /// Is this backend usable here at all?
    fn probe(&self) -> crate::Result<()>;

    fn install(&self, spec: &ScheduleSpec) -> crate::Result<()>;
    fn uninstall(&self) -> crate::Result<()>;
    fn status(&self) -> crate::Result<State>;
}

/// Install, then prove it will actually run.
///
/// This is the whole point of the module. A backend that registers something
/// which never fires has failed, however cheerfully it reported success, so
/// the install is undone and the caller told.
pub fn install_checked(sched: &dyn Scheduler, spec: &ScheduleSpec) -> crate::Result<Health> {
    sched.probe()?;
    sched.install(spec)?;

    match sched.status()? {
        State::Installed(health) if health.next_run.is_some() => Ok(health),
        other => {
            // Leave nothing behind that pretends to work.
            let _ = sched.uninstall();
            Err(CcredError::Schedule(format!(
                "the job was registered but reports no next run, so it would never \
                 have fired; the installation has been undone (status: {other:?})"
            )))
        }
    }
}

/// The backend for this platform.
pub fn detect() -> Box<dyn Scheduler> {
    #[cfg(target_os = "macos")]
    {
        Box::new(launchd::Launchd)
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(taskschd::TaskScheduler)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Box::new(systemd::Systemd)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn spec() -> ScheduleSpec {
        ScheduleSpec::new(
            PathBuf::from("/usr/local/bin/ccred"),
            PathBuf::from("/home/user"),
            PathBuf::from("/home/user/.local/state/ccred/logs"),
            "user".into(),
        )
    }

    #[test]
    fn the_default_schedule_leaves_a_gap_of_at_most_four_days() {
        let s = spec();
        assert_eq!(s.days, vec![Weekday::Mon, Weekday::Thu]);
    }

    #[test]
    fn the_default_command_rate_limits_itself() {
        // The schedule is a hint; the real limit is in the command.
        let s = spec();
        assert!(s.args.contains(&"--if-older-than".to_string()));
    }

    #[test]
    fn a_path_with_spaces_is_quoted() {
        let mut s = spec();
        s.exe = PathBuf::from("/opt/my tools/ccred");
        assert!(s.command_line().starts_with('"'), "{}", s.command_line());
    }

    #[test]
    fn weekday_spellings_match_each_platform() {
        assert_eq!(Weekday::Mon.systemd(), "Mon");
        assert_eq!(Weekday::Mon.launchd(), 1);
        assert_eq!(Weekday::Mon.windows(), "Monday");
        // launchd uses 0 for Sunday, not 7.
        assert_eq!(Weekday::Sun.launchd(), 0);
    }
}
