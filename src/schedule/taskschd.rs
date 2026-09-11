//! Windows Task Scheduler.
//!
//! Registered through `schtasks /Create /XML` rather than COM: the artifact is
//! auditable text, which matters for a tool that holds credentials, and it
//! avoids a COM dependency for what is ultimately one string.

use std::path::PathBuf;
use std::process::Command;

use super::{Backend, Health, RenderedFile, ScheduleSpec, Scheduler, State, Warning};
use crate::error::CcredError;

const TASK_PATH: &str = "\\ccred\\refresh";

#[derive(Debug, Default)]
pub struct TaskScheduler;

impl TaskScheduler {
    fn xml_path(&self) -> PathBuf {
        std::env::temp_dir().join("ccred-refresh-task.xml")
    }

    fn schtasks(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("schtasks")
            // If schtasks ever decides it wants a password it prompts on
            // stdin; with a live stdin the install hangs, with null it fails
            // fast and the caller can react.
            .stdin(std::process::Stdio::null())
            .args(args)
            .output()
    }

    /// One registration attempt at a given privilege level.
    fn create(&self, spec: &ScheduleSpec, privilege: Privilege) -> crate::Result<()> {
        let xml = self.xml_path();
        std::fs::write(&xml, to_utf16le_bom(&render_task_xml(spec, privilege))).map_err(
            |source| CcredError::Io {
                path: xml.clone(),
                source,
            },
        )?;

        let out = self
            .schtasks(&[
                "/Create",
                "/F",
                "/TN",
                TASK_PATH,
                "/XML",
                &xml.to_string_lossy(),
            ])
            .map_err(|e| CcredError::Schedule(format!("schtasks /Create failed: {e}")));
        let _ = std::fs::remove_file(&xml);
        let out = out?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            return Err(CcredError::Schedule(format!(
                "schtasks /Create failed: {}",
                if stderr.trim().is_empty() {
                    stdout.trim()
                } else {
                    stderr.trim()
                }
            )));
        }
        Ok(())
    }
}

/// How much the registering user is allowed to ask for.
///
/// Measured on Windows 11 as a standard user: `schtasks /Create` answers
/// `Access is denied` for `<LogonType>S4U</LogonType>` and, independently, for
/// any `<BootTrigger>`. Either one alone is enough to be refused. Ordinary
/// task creation needs no elevation at all, so this feature is only out of
/// reach if we insist on the better definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privilege {
    /// S4U plus a boot trigger: runs while signed out, and a reboot spanning a
    /// scheduled run does not skip it. Needs elevation to register.
    Elevated,
    /// What a standard user may register. The task runs only while that user
    /// is signed in; `StartWhenAvailable` still catches up a run missed while
    /// the machine was off, so what is lost is the signed-out case rather than
    /// reliability in general.
    UserOnly,
}

/// Render the task definition.
///
/// Two settings are load-bearing and both default the wrong way:
///
/// * `DisallowStartIfOnBatteries` and `StopIfGoingOnBatteries` default to
///   **true**. Ship the defaults and on a laptop the task registers cleanly,
///   reports Ready, shows a next run time, and never executes -- the same
///   failure signature as the systemd bug this design exists to prevent.
/// * `StartWhenAvailable` defaults to **false**, so a run missed while the
///   machine was off is simply dropped. A boot trigger backs it up.
///
/// `LogonType` is `S4U` where it can be: it runs whether or not the user is
/// signed in, without storing a password. It also solves the console-window
/// problem for free, because an S4U task has no desktop to draw on. (`Hidden`
/// does not do that; it only hides the task from the Task Scheduler list.)
///
/// Both of those good ideas need elevation, which is what `Privilege` is for.
pub fn render_task_xml(spec: &ScheduleSpec, privilege: Privilege) -> String {
    let days: String = spec
        .days
        .iter()
        .map(|d| format!("<{}/>", d.windows()))
        .collect();

    // Spliced in whole rather than branched around, so the template below
    // stays one readable document.
    let boot_trigger = match privilege {
        Privilege::Elevated => concat!(
            "\x20   <BootTrigger>\r\n",
            "\x20     <Enabled>true</Enabled>\r\n",
            "\x20     <Delay>PT5M</Delay>\r\n",
            "\x20   </BootTrigger>\r\n",
        ),
        Privilege::UserOnly => "",
    };
    let logon_type = match privilege {
        Privilege::Elevated => "S4U",
        Privilege::UserOnly => "InteractiveToken",
    };

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
         <Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
         \x20 <RegistrationInfo>\r\n\
         \x20   <Author>ccred</Author>\r\n\
         \x20   <Description>Refresh stored Claude Code credentials.</Description>\r\n\
         \x20   <URI>{TASK_PATH}</URI>\r\n\
         \x20   <Source>ccred spec 1</Source>\r\n\
         \x20 </RegistrationInfo>\r\n\
         \x20 <Triggers>\r\n\
         \x20   <CalendarTrigger>\r\n\
         \x20     <StartBoundary>2026-01-05T{hour:02}:{minute:02}:00</StartBoundary>\r\n\
         \x20     <Enabled>true</Enabled>\r\n\
         \x20     <RandomDelay>PT2H</RandomDelay>\r\n\
         \x20     <ScheduleByWeek>\r\n\
         \x20       <WeeksInterval>1</WeeksInterval>\r\n\
         \x20       <DaysOfWeek>{days}</DaysOfWeek>\r\n\
         \x20     </ScheduleByWeek>\r\n\
         \x20   </CalendarTrigger>\r\n\
         {boot_trigger}\
         \x20 </Triggers>\r\n\
         \x20 <Principals>\r\n\
         \x20   <Principal id=\"Author\">\r\n\
         \x20     <UserId>{user}</UserId>\r\n\
         \x20     <LogonType>{logon_type}</LogonType>\r\n\
         \x20     <RunLevel>LeastPrivilege</RunLevel>\r\n\
         \x20   </Principal>\r\n\
         \x20 </Principals>\r\n\
         \x20 <Settings>\r\n\
         \x20   <Enabled>true</Enabled>\r\n\
         \x20   <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\r\n\
         \x20   <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\r\n\
         \x20   <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\r\n\
         \x20   <StartWhenAvailable>true</StartWhenAvailable>\r\n\
         \x20   <RunOnlyIfNetworkAvailable>true</RunOnlyIfNetworkAvailable>\r\n\
         \x20   <AllowStartOnDemand>true</AllowStartOnDemand>\r\n\
         \x20   <RunOnlyIfIdle>false</RunOnlyIfIdle>\r\n\
         \x20   <WakeToRun>false</WakeToRun>\r\n\
         \x20   <Hidden>false</Hidden>\r\n\
         \x20   <ExecutionTimeLimit>PT10M</ExecutionTimeLimit>\r\n\
         \x20   <Priority>7</Priority>\r\n\
         \x20 </Settings>\r\n\
         \x20 <Actions Context=\"Author\">\r\n\
         \x20   <Exec>\r\n\
         \x20     <Command>{exe}</Command>\r\n\
         \x20     <Arguments>{args}</Arguments>\r\n\
         \x20     <WorkingDirectory>{home}</WorkingDirectory>\r\n\
         \x20   </Exec>\r\n\
         \x20 </Actions>\r\n\
         </Task>\r\n",
        hour = spec.hour,
        minute = spec.minute,
        user = xml_escape(&spec.user),
        exe = xml_escape(&spec.exe.to_string_lossy()),
        args = xml_escape(&spec.args.join(" ")),
        home = xml_escape(&spec.home.to_string_lossy()),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Task Scheduler wants UTF-16LE with a BOM; `schtasks /Create /XML` rejects
/// UTF-8 on some builds with an unhelpful "incorrectly formatted" error.
pub fn to_utf16le_bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

impl Scheduler for TaskScheduler {
    fn backend(&self) -> Backend {
        Backend::TaskScheduler
    }

    fn render(&self, spec: &ScheduleSpec) -> crate::Result<Vec<RenderedFile>> {
        // The privileged definition is what `install` asks for first, so it is
        // what a dry run should show. If elevation is refused the fallback is
        // this document without the boot trigger and with `InteractiveToken`.
        Ok(vec![RenderedFile {
            path: self.xml_path(),
            contents: render_task_xml(spec, Privilege::Elevated),
        }])
    }

    fn probe(&self) -> crate::Result<()> {
        match self.schtasks(&["/Query", "/?"]) {
            Ok(_) => Ok(()),
            Err(e) => Err(CcredError::Schedule(format!("schtasks unavailable: {e}"))),
        }
    }

    fn install(&self, spec: &ScheduleSpec) -> crate::Result<()> {
        std::fs::create_dir_all(&spec.log_dir).ok();

        // Ask for the better definition, then settle for the one a standard
        // user is allowed to register. Falling back beats refusing: without
        // this, scheduling simply does not work for anyone who is not an
        // administrator, which is most people.
        match self.create(spec, Privilege::Elevated) {
            Ok(()) => {}
            Err(elevated_err) => {
                self.create(spec, Privilege::UserOnly).map_err(|_| {
                    // Report the first failure, not the second: the fallback
                    // is the narrower attempt, so its error explains less.
                    elevated_err
                })?;
            }
        }
        Ok(())
    }

    fn uninstall(&self) -> crate::Result<()> {
        let _ = self.schtasks(&["/End", "/TN", TASK_PATH]);
        let _ = self.schtasks(&["/Delete", "/TN", TASK_PATH, "/F"]);
        Ok(())
    }

    fn status(&self) -> crate::Result<State> {
        let out = match self.schtasks(&["/Query", "/TN", TASK_PATH, "/FO", "LIST", "/V"]) {
            Ok(o) => o,
            Err(e) => {
                return Ok(State::Unsupported {
                    reason: format!("schtasks unavailable: {e}"),
                    remedy: None,
                });
            }
        };
        if !out.status.success() {
            return Ok(State::NotInstalled);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let next_run = parse_query(&text);

        let mut warnings = Vec::new();
        if next_run.is_none() {
            warnings.push(Warning::RegisteredButNeverFires);
        }
        if text.contains("DisallowStartIfOnBatteries: TRUE") {
            warnings.push(Warning::OnBatteryBlocked);
        }

        // Read the registered definition back rather than trusting the `/V`
        // listing. Its headers are localised, and its *values* are ambiguous:
        // `Idle Time` and `Delete Task If Not Rescheduled` both read
        // `Disabled` on a perfectly healthy task. The XML says each thing
        // once, in one place, in English.
        let xml = self
            .schtasks(&["/Query", "/TN", TASK_PATH, "/XML", "ONE"])
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if xml.contains("InteractiveToken") {
            warnings.push(Warning::RunsOnlyWhenSignedIn);
        }

        Ok(State::Installed(Health {
            enabled: settings_enabled(&xml),
            next_run,
            last_run: None,
            warnings,
        }))
    }
}

/// Is the task itself switched on?
///
/// Only the `<Enabled>` inside `<Settings>` answers that. Every trigger
/// carries an `<Enabled>` of its own, so a search across the whole document
/// reports on whichever element happens to come first.
///
/// An unreadable or empty document counts as enabled: `status` has already
/// established that the task exists, and claiming it is switched off on the
/// strength of a failed second query would invent a fault.
pub fn settings_enabled(xml: &str) -> bool {
    let Some(start) = xml.find("<Settings") else {
        return true;
    };
    let rest = &xml[start..];
    let end = rest.find("</Settings>").unwrap_or(rest.len());
    !rest[..end].contains("<Enabled>false</Enabled>")
}

/// Pull the next-run time out of `schtasks /FO LIST /V`.
///
/// Values are matched, not header names: `/V` headers are localised, so
/// matching on "Next Run Time" would break on a non-English Windows. A row
/// whose value is `N/A` means the task is registered and will never fire.
///
/// Whether the task is *enabled* is deliberately not read from here. Header
/// names are localised and the values are ambiguous: on a healthy task both
/// `Idle Time` and `Delete Task If Not Rescheduled` read `Disabled`. See
/// `settings_enabled`.
pub fn parse_query(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some((_, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        // A date-like value that is not N/A is the schedule.
        if value.len() >= 8
            && value.chars().next().is_some_and(|c| c.is_ascii_digit())
            && value.contains(':')
        {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::tests::spec;

    #[test]
    fn the_battery_defaults_are_overridden() {
        // Both default to true. Shipping the defaults gives a task that says
        // Ready, shows a next run, and never executes on a laptop.
        let x = render_task_xml(&spec(), Privilege::Elevated);
        assert!(x.contains("<DisallowStartIfOnBatteries>false<"), "{x}");
        assert!(x.contains("<StopIfGoingOnBatteries>false<"), "{x}");
    }

    #[test]
    fn missed_runs_are_caught_up() {
        let x = render_task_xml(&spec(), Privilege::Elevated);
        assert!(x.contains("<StartWhenAvailable>true<"), "{x}");
        // An independent boot trigger, because StartWhenAvailable's wording
        // about "repeats infinitely" is ambiguous for a weekly trigger.
        assert!(x.contains("<BootTrigger>"), "{x}");
    }

    #[test]
    fn s4u_runs_without_a_stored_password_or_a_console() {
        let x = render_task_xml(&spec(), Privilege::Elevated);
        assert!(x.contains("<LogonType>S4U</LogonType>"), "{x}");
        assert!(x.contains("<RunLevel>LeastPrivilege</RunLevel>"), "{x}");
    }

    /// Measured on Windows 11: a standard user is refused `Access is denied`
    /// for S4U and, separately, for any boot trigger. The fallback definition
    /// must contain neither, or scheduling does not work without elevation --
    /// which is the state most people are in.
    #[test]
    fn the_unelevated_variant_drops_exactly_what_needs_elevation() {
        let x = render_task_xml(&spec(), Privilege::UserOnly);
        assert!(x.contains("<LogonType>InteractiveToken</LogonType>"), "{x}");
        assert!(!x.contains("S4U"), "{x}");
        assert!(!x.contains("<BootTrigger>"), "{x}");
    }

    /// The fallback gives up running while signed out. It must not also give
    /// up catching a run missed while the machine was off.
    #[test]
    fn the_unelevated_variant_still_catches_up_missed_runs() {
        let x = render_task_xml(&spec(), Privilege::UserOnly);
        assert!(x.contains("<StartWhenAvailable>true<"), "{x}");
        assert!(x.contains("<ScheduleByWeek>"), "{x}");
    }

    /// Dropping the boot trigger must not leave a malformed `<Triggers>`.
    #[test]
    fn both_variants_are_well_formed_enough_to_have_matching_tags() {
        for p in [Privilege::Elevated, Privilege::UserOnly] {
            let x = render_task_xml(&spec(), p);
            for tag in ["Task", "Triggers", "Principals", "Settings", "Actions"] {
                assert_eq!(
                    x.matches(&format!("<{tag}")).count(),
                    x.matches(&format!("</{tag}>")).count(),
                    "unbalanced <{tag}> for {p:?}:
{x}"
                );
            }
        }
    }

    #[test]
    fn both_days_are_named() {
        let x = render_task_xml(&spec(), Privilege::Elevated);
        assert!(x.contains("<Monday/>"), "{x}");
        assert!(x.contains("<Thursday/>"), "{x}");
    }

    #[test]
    fn the_xml_is_encoded_as_utf16le_with_a_bom() {
        // schtasks rejects UTF-8 on some builds.
        let bytes = to_utf16le_bom("AB");
        assert_eq!(bytes, vec![0xFF, 0xFE, b'A', 0x00, b'B', 0x00]);
    }

    #[test]
    fn special_characters_in_paths_are_escaped() {
        let mut s = spec();
        s.exe = PathBuf::from("C:\\Program Files\\a&b\\ccred.exe");
        let x = render_task_xml(&s, Privilege::Elevated);
        assert!(x.contains("a&amp;b"), "{x}");
        assert!(!x.contains("a&b"), "{x}");
    }

    #[test]
    fn a_registered_task_with_no_next_run_is_detected() {
        // The Windows dialect of the failure this design exists to catch.
        let listing = "TaskName:      \\ccred\\refresh\nStatus:        Ready\nNext Run Time: N/A\n";
        assert!(
            parse_query(listing).is_none(),
            "N/A must not be read as a schedule"
        );
    }

    #[test]
    fn a_real_next_run_is_extracted() {
        let listing =
            "TaskName:      \\ccred\\refresh\nNext Run Time: 09/14/2026 09:17:00\nStatus: Ready\n";
        assert!(parse_query(listing).is_some(), "{listing}");
    }

    /// The bug this replaced: `enabled` came from any `/V` row whose value
    /// read `Disabled`, and a healthy task has two of them -- `Idle Time` and
    /// `Delete Task If Not Rescheduled`. Every Windows install therefore
    /// reported "installed, but disabled". The old test passed because its
    /// sample listing was tidier than anything Windows actually prints.
    #[test]
    fn rows_that_merely_say_disabled_are_not_the_task_state() {
        let listing = "TaskName: \\ccred\\refresh\nStatus: Ready\n\
                       Idle Time: Disabled\nDelete Task If Not Rescheduled: Disabled\n\
                       Next Run Time: 09/14/2026 09:17:00\n";
        assert!(parse_query(listing).is_some(), "{listing}");

        let xml = "<Task><Triggers><CalendarTrigger><Enabled>true</Enabled>\
                   </CalendarTrigger></Triggers><Settings><Enabled>true</Enabled>\
                   <RunOnlyIfIdle>false</RunOnlyIfIdle></Settings></Task>";
        assert!(settings_enabled(xml), "{xml}");
    }

    #[test]
    fn a_task_switched_off_in_the_ui_is_reported_as_disabled() {
        let xml = "<Task><Settings><Enabled>false</Enabled></Settings></Task>";
        assert!(!settings_enabled(xml), "{xml}");
    }

    /// A trigger carries an `<Enabled>` of its own, so a search across the
    /// whole document reports on whichever element comes first.
    #[test]
    fn a_disabled_trigger_does_not_disable_the_task() {
        let xml = "<Task><Triggers><BootTrigger><Enabled>false</Enabled>\
                   </BootTrigger></Triggers><Settings><Enabled>true</Enabled>\
                   </Settings></Task>";
        assert!(settings_enabled(xml), "{xml}");
    }

    /// Failing to read the definition back is not evidence of anything.
    #[test]
    fn an_unreadable_definition_is_not_reported_as_switched_off() {
        assert!(settings_enabled(""));
    }
}
