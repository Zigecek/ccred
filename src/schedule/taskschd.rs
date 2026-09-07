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
/// `LogonType` is `S4U`: it runs whether or not the user is signed in, without
/// storing a password. It also solves the console-window problem for free,
/// because an S4U task has no desktop to draw on. (`Hidden` does not do that;
/// it only hides the task from the Task Scheduler list.)
pub fn render_task_xml(spec: &ScheduleSpec) -> String {
    let days: String = spec
        .days
        .iter()
        .map(|d| format!("<{}/>", d.windows()))
        .collect();

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
         \x20   <BootTrigger>\r\n\
         \x20     <Enabled>true</Enabled>\r\n\
         \x20     <Delay>PT5M</Delay>\r\n\
         \x20   </BootTrigger>\r\n\
         \x20 </Triggers>\r\n\
         \x20 <Principals>\r\n\
         \x20   <Principal id=\"Author\">\r\n\
         \x20     <UserId>{user}</UserId>\r\n\
         \x20     <LogonType>S4U</LogonType>\r\n\
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
        Ok(vec![RenderedFile {
            path: self.xml_path(),
            contents: render_task_xml(spec),
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

        let xml = self.xml_path();
        std::fs::write(&xml, to_utf16le_bom(&render_task_xml(spec))).map_err(|source| {
            CcredError::Io {
                path: xml.clone(),
                source,
            }
        })?;

        let out = self
            .schtasks(&[
                "/Create",
                "/F",
                "/TN",
                TASK_PATH,
                "/XML",
                &xml.to_string_lossy(),
            ])
            .map_err(|e| CcredError::Schedule(format!("schtasks /Create failed: {e}")))?;
        let _ = std::fs::remove_file(&xml);

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
        let (next_run, disabled) = parse_query(&text);

        let mut warnings = Vec::new();
        if next_run.is_none() {
            warnings.push(Warning::RegisteredButNeverFires);
        }
        if text.contains("DisallowStartIfOnBatteries: TRUE") {
            warnings.push(Warning::OnBatteryBlocked);
        }

        Ok(State::Installed(Health {
            enabled: !disabled,
            next_run,
            last_run: None,
            warnings,
        }))
    }
}

/// Pull the next-run time out of `schtasks /FO LIST /V`.
///
/// Values are matched, not header names: `/V` headers are localised, so
/// matching on "Next Run Time" would break on a non-English Windows. A row
/// whose value is `N/A` means the task is registered and will never fire.
pub fn parse_query(text: &str) -> (Option<String>, bool) {
    let mut next = None;
    let mut disabled = false;
    for line in text.lines() {
        let Some((_, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.eq_ignore_ascii_case("disabled") {
            disabled = true;
        }
        // A date-like value that is not N/A is the schedule.
        if next.is_none()
            && value.len() >= 8
            && value.chars().next().is_some_and(|c| c.is_ascii_digit())
            && value.contains(':')
        {
            next = Some(value.to_string());
        }
    }
    if text.contains("N/A") && next.is_none() {
        return (None, disabled);
    }
    (next, disabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::tests::spec;

    #[test]
    fn the_battery_defaults_are_overridden() {
        // Both default to true. Shipping the defaults gives a task that says
        // Ready, shows a next run, and never executes on a laptop.
        let x = render_task_xml(&spec());
        assert!(x.contains("<DisallowStartIfOnBatteries>false<"), "{x}");
        assert!(x.contains("<StopIfGoingOnBatteries>false<"), "{x}");
    }

    #[test]
    fn missed_runs_are_caught_up() {
        let x = render_task_xml(&spec());
        assert!(x.contains("<StartWhenAvailable>true<"), "{x}");
        // An independent boot trigger, because StartWhenAvailable's wording
        // about "repeats infinitely" is ambiguous for a weekly trigger.
        assert!(x.contains("<BootTrigger>"), "{x}");
    }

    #[test]
    fn s4u_runs_without_a_stored_password_or_a_console() {
        let x = render_task_xml(&spec());
        assert!(x.contains("<LogonType>S4U</LogonType>"), "{x}");
        assert!(x.contains("<RunLevel>LeastPrivilege</RunLevel>"), "{x}");
    }

    #[test]
    fn both_days_are_named() {
        let x = render_task_xml(&spec());
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
        let x = render_task_xml(&s);
        assert!(x.contains("a&amp;b"), "{x}");
        assert!(!x.contains("a&b"), "{x}");
    }

    #[test]
    fn a_registered_task_with_no_next_run_is_detected() {
        // The Windows dialect of the failure this design exists to catch.
        let listing = "TaskName:      \\ccred\\refresh\nStatus:        Ready\nNext Run Time: N/A\n";
        let (next, _) = parse_query(listing);
        assert!(next.is_none(), "N/A must not be read as a schedule");
    }

    #[test]
    fn a_real_next_run_is_extracted() {
        let listing =
            "TaskName:      \\ccred\\refresh\nNext Run Time: 09/14/2026 09:17:00\nStatus: Ready\n";
        let (next, disabled) = parse_query(listing);
        assert!(next.is_some(), "{listing}");
        assert!(!disabled);
    }

    #[test]
    fn a_disabled_task_is_detected() {
        let listing = "TaskName: \\ccred\\refresh\nStatus: Disabled\nNext Run Time: N/A\n";
        let (_, disabled) = parse_query(listing);
        assert!(disabled);
    }
}
