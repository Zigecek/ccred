//! systemd user timer.

use std::path::PathBuf;
use std::process::Command;

use super::{Backend, Health, RenderedFile, ScheduleSpec, Scheduler, State, Warning};
use crate::error::CcredError;

const UNIT_NAME: &str = "ccred-refresh";

#[derive(Debug, Default)]
pub struct Systemd;

impl Systemd {
    fn unit_dir(&self) -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|h| h.join(".config"))
            })
            .unwrap_or_else(|| PathBuf::from(".config"));
        base.join("systemd").join("user")
    }

    fn systemctl(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("systemctl").arg("--user").args(args).output()
    }
}

/// The service unit.
///
/// `ReadWritePaths` lists `.claude.json` separately on purpose. Without
/// `CLAUDE_CONFIG_DIR` that file sits *next to* the `.claude` directory rather
/// than inside it, so whitelisting only the directory leaves it read-only --
/// which is precisely the latent bug the predecessor shipped: switching from
/// the timer would have failed with EROFS.
///
/// The `-` prefix means "tolerate absence"; without it, a missing path makes
/// the unit fail to start with a mount error rather than simply doing nothing.
pub fn render_service(spec: &ScheduleSpec, hardened: bool) -> String {
    let mut s = String::new();
    s.push_str("[Unit]\n");
    s.push_str("Description=Refresh stored Claude Code credentials (ccred)\n");
    s.push_str("X-CCred-Spec=1\n");
    s.push_str(if hardened {
        "X-CCred-Profile=hardened\n"
    } else {
        "X-CCred-Profile=minimal\n"
    });
    s.push_str("\n[Service]\n");
    s.push_str("Type=oneshot\n");
    s.push_str(&format!("ExecStart={}\n", spec.command_line()));
    s.push_str("TimeoutStartSec=600\n");
    s.push_str("Nice=10\n");
    s.push_str("UMask=0077\n");
    s.push_str("\n# Writable surface\n");
    s.push_str("ReadWritePaths=-%h/.claude\n");
    s.push_str("ReadWritePaths=-%h/.claude.json\n");
    s.push_str("ReadWritePaths=-%h/.ccred\n");
    if hardened {
        s.push_str("\n# Namespace hardening (needs unprivileged user namespaces)\n");
        s.push_str("ProtectSystem=strict\n");
        s.push_str("ProtectHome=read-only\n");
        s.push_str("PrivateTmp=true\n");
        s.push_str("ProtectKernelTunables=true\n");
        s.push_str("ProtectControlGroups=true\n");
    }
    s.push_str("\n# Always safe\n");
    s.push_str("NoNewPrivileges=true\n");
    s.push_str("RestrictSUIDSGID=true\n");
    s.push_str("RestrictRealtime=true\n");
    s.push_str("LockPersonality=true\n");
    s
}

/// The timer unit.
///
/// `OnCalendar=`, never `OnBootSec=`/`OnUnitActiveSec=`. `Persistent=true`
/// only has an effect on calendar timers; paired with a monotonic one it
/// produces a timer that reports `active` and never fires.
pub fn render_timer(spec: &ScheduleSpec) -> String {
    let days: Vec<&str> = spec.days.iter().map(|d| d.systemd()).collect();
    format!(
        "[Unit]\n\
         Description=Periodic Claude Code credential refresh (ccred)\n\
         X-CCred-Spec=1\n\
         \n[Timer]\n\
         # Calendar, not monotonic: Persistent= below only affects OnCalendar=.\n\
         OnCalendar={} *-*-* {:02}:{:02}:00\n\
         Persistent=true\n\
         RandomizedDelaySec=2h\n\
         AccuracySec=1m\n\
         Unit={UNIT_NAME}.service\n\
         \n[Install]\n\
         WantedBy=timers.target\n",
        days.join(","),
        spec.hour,
        spec.minute
    )
}

impl Scheduler for Systemd {
    fn backend(&self) -> Backend {
        Backend::Systemd
    }

    fn render(&self, spec: &ScheduleSpec) -> crate::Result<Vec<RenderedFile>> {
        let dir = self.unit_dir();
        Ok(vec![
            RenderedFile {
                path: dir.join(format!("{UNIT_NAME}.service")),
                contents: render_service(spec, true),
            },
            RenderedFile {
                path: dir.join(format!("{UNIT_NAME}.timer")),
                contents: render_timer(spec),
            },
        ])
    }

    fn probe(&self) -> crate::Result<()> {
        // `systemctl --user` needs a live user manager. Under sudo, plain su,
        // cron, or a container it is simply absent, and it does not fall back.
        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
            return Err(CcredError::Schedule(
                "no user session bus (XDG_RUNTIME_DIR is unset); run this from a normal \
                 login shell, not under sudo or from cron"
                    .into(),
            ));
        }
        match self.systemctl(&["is-system-running"]) {
            Ok(_) => Ok(()),
            Err(e) => Err(CcredError::Schedule(format!(
                "systemctl --user is not usable here: {e}"
            ))),
        }
    }

    fn install(&self, spec: &ScheduleSpec) -> crate::Result<()> {
        let dir = self.unit_dir();
        std::fs::create_dir_all(&dir).map_err(|source| CcredError::Io {
            path: dir.clone(),
            source,
        })?;
        std::fs::create_dir_all(&spec.log_dir).ok();

        for file in self.render(spec)? {
            crate::atomic::write_atomic(&file.path, file.contents.as_bytes(), false)?;
        }
        let _ = self.systemctl(&["daemon-reload"]);

        // Verify the hardened profile actually starts. Namespace options can
        // fail at start time on kernels with unprivileged user namespaces
        // disabled, and the failure looks nothing like a config error.
        let started = self.systemctl(&["start", &format!("{UNIT_NAME}.service")]);
        let hardened_ok = started.map(|o| o.status.success()).unwrap_or(false);
        if !hardened_ok {
            let path = dir.join(format!("{UNIT_NAME}.service"));
            crate::atomic::write_atomic(&path, render_service(spec, false).as_bytes(), false)?;
            let _ = self.systemctl(&["daemon-reload"]);
        }

        let out = self
            .systemctl(&["enable", "--now", &format!("{UNIT_NAME}.timer")])
            .map_err(|e| CcredError::Schedule(format!("could not enable the timer: {e}")))?;
        if !out.status.success() {
            return Err(CcredError::Schedule(format!(
                "systemctl enable failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    fn uninstall(&self) -> crate::Result<()> {
        let _ = self.systemctl(&["disable", "--now", &format!("{UNIT_NAME}.timer")]);
        let _ = self.systemctl(&["stop", &format!("{UNIT_NAME}.service")]);
        let dir = self.unit_dir();
        let _ = std::fs::remove_file(dir.join(format!("{UNIT_NAME}.timer")));
        let _ = std::fs::remove_file(dir.join(format!("{UNIT_NAME}.service")));
        let _ = self.systemctl(&["daemon-reload"]);
        // Without this a previously failed unit keeps showing up in --failed
        // long after its files are gone.
        let _ = self.systemctl(&["reset-failed", &format!("{UNIT_NAME}.service")]);
        Ok(())
    }

    fn status(&self) -> crate::Result<State> {
        let out = match self.systemctl(&[
            "show",
            &format!("{UNIT_NAME}.timer"),
            "-p",
            "LoadState",
            "-p",
            "ActiveState",
            "-p",
            "UnitFileState",
            "-p",
            "NextElapseUSecRealtime",
            "-p",
            "LastTriggerUSec",
        ]) {
            Ok(o) => o,
            Err(e) => {
                return Ok(State::Unsupported {
                    reason: format!("systemctl unavailable: {e}"),
                    remedy: None,
                });
            }
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let props = parse_properties(&text);

        if props.get("LoadState").map(String::as_str) == Some("not-found") {
            return Ok(State::NotInstalled);
        }

        let mut warnings = Vec::new();
        let next_run = props
            .get("NextElapseUSecRealtime")
            .filter(|v| !v.is_empty() && *v != "0" && *v != "n/a" && *v != "infinity")
            .cloned();
        if next_run.is_none() {
            warnings.push(Warning::RegisteredButNeverFires);
        }
        if linger_enabled() == Some(false) {
            warnings.push(Warning::LingerDisabled);
        }

        Ok(State::Installed(Health {
            enabled: props.get("UnitFileState").map(String::as_str) == Some("enabled"),
            next_run,
            last_run: props
                .get("LastTriggerUSec")
                .filter(|v| !v.is_empty() && *v != "n/a")
                .cloned(),
            warnings,
        }))
    }
}

/// `key=value` lines, as `systemctl show` emits them.
pub fn parse_properties(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn linger_enabled() -> Option<bool> {
    let user = std::env::var("USER").ok()?;
    let out = Command::new("loginctl")
        .args(["show-user", &user, "-p", "Linger", "--value"])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&out.stdout)
        .trim()
        .to_ascii_lowercase();
    match value.as_str() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::tests::spec;

    #[test]
    fn the_timer_is_calendar_based_not_monotonic() {
        // The predecessor's bug: Persistent= with a monotonic timer yields a
        // unit that is active and never fires.
        let t = render_timer(&spec());
        assert!(t.contains("OnCalendar="), "{t}");
        assert!(t.contains("Persistent=true"), "{t}");
        assert!(
            !t.contains("OnBootSec="),
            "monotonic timer reintroduced:\n{t}"
        );
        assert!(!t.contains("OnUnitActiveSec="), "monotonic timer:\n{t}");
    }

    #[test]
    fn the_calendar_expression_names_both_days() {
        let t = render_timer(&spec());
        assert!(t.contains("OnCalendar=Mon,Thu *-*-* 09:17:00"), "{t}");
    }

    #[test]
    fn claude_json_is_writable_separately_from_the_directory() {
        // Without CLAUDE_CONFIG_DIR this file sits NEXT TO ~/.claude, so
        // whitelisting only the directory leaves it read-only.
        let s = render_service(&spec(), true);
        assert!(s.contains("ReadWritePaths=-%h/.claude\n"), "{s}");
        assert!(s.contains("ReadWritePaths=-%h/.claude.json\n"), "{s}");
        assert!(s.contains("ReadWritePaths=-%h/.ccred\n"), "{s}");
    }

    #[test]
    fn writable_paths_tolerate_absence() {
        // A missing path without the '-' prefix makes the unit fail to start.
        let s = render_service(&spec(), true);
        for line in s.lines().filter(|l| l.starts_with("ReadWritePaths=")) {
            assert!(
                line.starts_with("ReadWritePaths=-"),
                "must tolerate absence: {line}"
            );
        }
    }

    #[test]
    fn the_minimal_profile_drops_only_the_namespace_options() {
        let hardened = render_service(&spec(), true);
        let minimal = render_service(&spec(), false);
        assert!(hardened.contains("ProtectSystem=strict"));
        assert!(!minimal.contains("ProtectSystem=strict"));
        // Options that need no namespaces must survive the fallback.
        for keep in [
            "NoNewPrivileges=true",
            "RestrictSUIDSGID=true",
            "UMask=0077",
        ] {
            assert!(minimal.contains(keep), "{keep} lost in minimal profile");
        }
    }

    #[test]
    fn the_profile_is_recorded_in_the_unit() {
        assert!(render_service(&spec(), true).contains("X-CCred-Profile=hardened"));
        assert!(render_service(&spec(), false).contains("X-CCred-Profile=minimal"));
    }

    #[test]
    fn rendering_names_both_unit_files() {
        let files = Systemd.render(&spec()).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files[0].path.to_string_lossy().ends_with(".service"));
        assert!(files[1].path.to_string_lossy().ends_with(".timer"));
    }

    #[test]
    fn property_parsing_handles_values_containing_equals() {
        let props = parse_properties("A=1\nB=x=y\nC=\n");
        assert_eq!(props["A"], "1");
        assert_eq!(props["B"], "x=y");
        assert_eq!(props["C"], "");
    }
}
