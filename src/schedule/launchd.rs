//! macOS LaunchAgent.
//!
//! **Not verified on real hardware.** Everything here follows from the
//! documented behaviour of `launchd` and from static analysis, but no Mac was
//! available while writing it. Treat the install path as unproven until it has
//! run on one.

use std::path::PathBuf;
use std::process::Command;

use super::{Backend, Health, RenderedFile, ScheduleSpec, Scheduler, State, Warning};
use crate::error::CcredError;

const LABEL: &str = "dev.ccred.refresh";

#[derive(Debug, Default)]
pub struct Launchd;

impl Launchd {
    fn plist_path(&self) -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    fn domain(&self) -> String {
        // gui/<uid> is the Aqua domain, which is where a plist in
        // ~/Library/LaunchAgents is auto-loaded at login and the only domain
        // where the login keychain is unlocked.
        format!("gui/{}", uid())
    }

    fn launchctl(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("launchctl").args(args).output()
    }
}

fn uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".to_string())
}

/// Render the agent plist.
///
/// `StartCalendarInterval`, never `StartInterval`. A `StartInterval` firing
/// that elapses while the machine is asleep is simply lost, so on a laptop
/// that sleeps nightly a multi-day cadence drifts and can skip weeks.
/// Calendar intervals coalesce missed firings into one on wake, which is the
/// behaviour a refresher needs.
///
/// Paths are written out in full: `launchd` does not expand `~`, and a tilde
/// here produces a job that silently never works.
pub fn render_plist(spec: &ScheduleSpec) -> String {
    let mut args = String::new();
    args.push_str(&format!(
        "    <string>{}</string>\n",
        xml_escape(&spec.exe.to_string_lossy())
    ));
    for a in &spec.args {
        args.push_str(&format!("    <string>{}</string>\n", xml_escape(a)));
    }

    let mut calendar = String::new();
    for day in &spec.days {
        calendar.push_str(&format!(
            "    <dict>\n      <key>Weekday</key><integer>{}</integer>\n      \
             <key>Hour</key><integer>{}</integer>\n      \
             <key>Minute</key><integer>{}</integer>\n    </dict>\n",
            day.launchd(),
            spec.hour,
            spec.minute
        ));
    }

    let log = spec.log_dir.join("launchd.err");

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n  \
         <key>Label</key><string>{LABEL}</string>\n  \
         <key>CCredSpec</key><integer>1</integer>\n  \
         <key>ProgramArguments</key>\n  <array>\n{args}  </array>\n  \
         <key>StartCalendarInterval</key>\n  <array>\n{calendar}  </array>\n  \
         <key>RunAtLoad</key><false/>\n  \
         <key>ProcessType</key><string>Background</string>\n  \
         <key>LowPriorityIO</key><true/>\n  \
         <key>WorkingDirectory</key><string>{home}</string>\n  \
         <key>StandardOutPath</key><string>{log}</string>\n  \
         <key>StandardErrorPath</key><string>{log}</string>\n\
         </dict>\n\
         </plist>\n",
        home = xml_escape(&spec.home.to_string_lossy()),
        log = xml_escape(&log.to_string_lossy()),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl Scheduler for Launchd {
    fn backend(&self) -> Backend {
        Backend::Launchd
    }

    fn render(&self, spec: &ScheduleSpec) -> crate::Result<Vec<RenderedFile>> {
        Ok(vec![RenderedFile {
            path: self.plist_path(),
            contents: render_plist(spec),
        }])
    }

    fn probe(&self) -> crate::Result<()> {
        if self.launchctl(&["print", &self.domain()]).is_err() {
            return Err(CcredError::Schedule("launchctl is not usable here".into()));
        }
        Ok(())
    }

    fn install(&self, spec: &ScheduleSpec) -> crate::Result<()> {
        let path = self.plist_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CcredError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // launchd does not create the directory for StandardOutPath, and a
        // missing one makes the job error out instead of running.
        std::fs::create_dir_all(&spec.log_dir).ok();

        crate::atomic::write_atomic(&path, render_plist(spec).as_bytes(), false)?;

        let target = format!("{}/{LABEL}", self.domain());
        // bootstrap is not idempotent: bootstrapping an already-loaded label
        // fails with "Input/output error". Tear down first, ignoring failure.
        let _ = self.launchctl(&["bootout", &target]);
        // A previous `disable` is persistent and survives bootout and even
        // deleting the file, so an install that skipped this would appear to
        // succeed and never run.
        let _ = self.launchctl(&["enable", &target]);

        let out = self
            .launchctl(&["bootstrap", &self.domain(), &path.to_string_lossy()])
            .map_err(|e| CcredError::Schedule(format!("launchctl bootstrap failed: {e}")))?;
        if !out.status.success() {
            return Err(CcredError::Schedule(format!(
                "launchctl bootstrap failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    fn uninstall(&self) -> crate::Result<()> {
        let target = format!("{}/{LABEL}", self.domain());
        let _ = self.launchctl(&["bootout", &target]);
        let _ = self.launchctl(&["enable", &target]); // clear any sticky disable
        let _ = std::fs::remove_file(self.plist_path());
        Ok(())
    }

    fn status(&self) -> crate::Result<State> {
        if !self.plist_path().exists() {
            return Ok(State::NotInstalled);
        }
        let target = format!("{}/{LABEL}", self.domain());
        let printed = self.launchctl(&["print", &target]);

        let loaded = printed
            .as_ref()
            .map(|o| o.status.success())
            .unwrap_or(false);

        let mut warnings = Vec::new();
        if !loaded {
            warnings.push(Warning::NoGuiSession);
        }
        if let Ok(out) = self.launchctl(&["print-disabled", &self.domain()]) {
            let text = String::from_utf8_lossy(&out.stdout);
            if text
                .lines()
                .any(|l| l.contains(LABEL) && l.contains("true"))
            {
                warnings.push(Warning::DisabledByUser);
            }
        }

        // launchd exposes no reliable machine-readable next-fire time for
        // calendar intervals, so it is computed from the spec rather than
        // queried. Being loaded and not disabled is what we can actually check.
        let next_run = if loaded && !warnings.contains(&Warning::DisabledByUser) {
            Some("per StartCalendarInterval".to_string())
        } else {
            warnings.push(Warning::RegisteredButNeverFires);
            None
        };

        Ok(State::Installed(Health {
            enabled: loaded,
            next_run,
            last_run: None,
            warnings,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::tests::spec;

    #[test]
    fn the_schedule_is_calendar_based_not_interval_based() {
        // StartInterval loses firings that elapse during sleep; calendar
        // intervals coalesce them and fire on wake.
        let p = render_plist(&spec());
        assert!(p.contains("<key>StartCalendarInterval</key>"), "{p}");
        assert!(
            !p.contains("<key>StartInterval</key>"),
            "StartInterval drifts on a sleeping laptop:\n{p}"
        );
    }

    #[test]
    fn both_days_are_emitted_with_launchd_numbering() {
        let p = render_plist(&spec());
        assert!(p.contains("<key>Weekday</key><integer>1</integer>"), "{p}");
        assert!(p.contains("<key>Weekday</key><integer>4</integer>"), "{p}");
    }

    #[test]
    fn paths_are_absolute_because_launchd_does_not_expand_tilde() {
        let p = render_plist(&spec());
        assert!(
            !p.contains('~'),
            "a tilde here silently breaks the job:\n{p}"
        );
        assert!(p.contains("/usr/local/bin/ccred"), "{p}");
    }

    #[test]
    fn the_label_matches_the_filename_stem() {
        // launchd requires this; a mismatch is rejected at bootstrap.
        let files = Launchd.render(&spec()).unwrap();
        let stem = files[0]
            .path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(stem, LABEL);
        assert!(
            files[0]
                .contents
                .contains(&format!("<string>{LABEL}</string>"))
        );
    }

    #[test]
    fn xml_special_characters_are_escaped() {
        let mut s = spec();
        s.args = vec!["a&b".into(), "c<d".into()];
        let p = render_plist(&s);
        assert!(p.contains("a&amp;b"), "{p}");
        assert!(p.contains("c&lt;d"), "{p}");
    }

    #[test]
    fn the_plist_is_well_formed_enough_to_have_matching_tags() {
        let p = render_plist(&spec());
        assert_eq!(p.matches("<dict>").count(), p.matches("</dict>").count());
        assert_eq!(p.matches("<array>").count(), p.matches("</array>").count());
        assert!(p.trim_end().ends_with("</plist>"));
    }
}
