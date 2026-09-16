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
    s.push_str(&format!("ExecStart={}\n", exec_line(spec)));
    s.push_str("TimeoutStartSec=600\n");
    s.push_str("Nice=10\n");
    s.push_str("UMask=0077\n");
    s.push_str("\n# Writable surface\n");
    s.push_str("ReadWritePaths=-%h/.claude\n");
    s.push_str("ReadWritePaths=-%h/.claude.json\n");
    s.push_str("ReadWritePaths=-%h/.ccred\n");
    // Relocated directories.
    for dir in &spec.writable {
        s.push_str(&format!("ReadWritePaths={}\n", writable_path(dir)));
    }
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
        if let Some(why) = unusable_binary_path(&spec.exe) {
            return Err(CcredError::Schedule(why));
        }
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

        let timer_file = self.unit_dir().join(format!("{UNIT_NAME}.timer"));
        if let Some(state) = unanswered(
            out.status.success(),
            &props,
            timer_file.exists(),
            &String::from_utf8_lossy(&out.stderr),
        ) {
            return Ok(state);
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
        let command = std::fs::read_to_string(self.unit_dir().join(format!("{UNIT_NAME}.service")))
            .ok()
            .and_then(|unit| registered_command(&unit));
        warnings.extend(super::missing_binary(command.as_deref()));

        Ok(State::Installed(Health {
            enabled: props.get("UnitFileState").map(String::as_str) == Some("enabled"),
            next_run,
            last_run: props
                .get("LastTriggerUSec")
                .filter(|v| !v.is_empty() && *v != "n/a")
                .cloned(),
            command,
            warnings,
        }))
    }
}

/// One word of a unit file line, exactly as systemd will read it back.
///
/// systemd splits on whitespace, honours `"` and `'` quoting and C escapes,
/// expands `%` specifiers everywhere and, in `ExecStart=`, `$VAR`. Escaping
/// only `%` let a path such as `/data/o'neil` produce "unbalanced quoting":
/// the service then had no `ExecStart` at all, while the timer still showed
/// a next run and the install was reported as working.
fn unit_word(word: &str, dollars: bool) -> String {
    let mut escaped = String::new();
    for c in word.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '%' => escaped.push_str("%%"),
            '$' if dollars => escaped.push_str("$$"),
            c => escaped.push(c),
        }
    }
    let plain = !word.is_empty()
        && word != ";"
        && !word
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\'));
    if plain {
        escaped
    } else {
        format!("\"{escaped}\"")
    }
}

/// An `ExecStart=` value.
///
/// The program path follows different rules from the arguments: systemd
/// expands no variables in it, so a `$` stays single, and it refuses a path
/// holding a quote or a backslash however it is written -- see
/// [`unusable_binary_path`].
fn exec_line(spec: &ScheduleSpec) -> String {
    std::iter::once(unit_word(&spec.exe.to_string_lossy(), false))
        .chain(spec.args.iter().map(|w| unit_word(w, true)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why systemd would refuse this binary path, if it would.
///
/// Its `ExecStart=` parser rejects a program path containing a quote or a
/// backslash, quoted or not. Registering one anyway produced a unit with no
/// usable command; refusing up front says why, and what to do.
fn unusable_binary_path(exe: &std::path::Path) -> Option<String> {
    let text = exe.to_string_lossy();
    let refused = |c: char| matches!(c, '\'' | '"' | '\\') || c.is_control();
    text.contains(refused).then(|| {
        format!(
            concat!(
                "systemd will not start a program whose path contains a quote, a ",
                "backslash or a control character ({:?}); install ccred somewhere ",
                "else and run this again"
            ),
            text
        )
    })
}

/// A `ReadWritePaths=` value that tolerates absence. The `-` goes inside the
/// quotes: systemd unquotes the word first and looks for it afterwards.
fn writable_path(dir: &std::path::Path) -> String {
    let word = unit_word(&format!("-{}", dir.to_string_lossy()), false);
    if word.starts_with('"') {
        word
    } else {
        format!("\"{word}\"")
    }
}

/// The program a unit starts: the first word of `ExecStart=`, read the way
/// `unit_word` wrote it.
pub fn registered_command(unit: &str) -> Option<PathBuf> {
    let value = unit
        .lines()
        .find_map(|line| line.trim().strip_prefix("ExecStart="))?
        .trim();

    let mut word = String::new();
    let mut chars = value.chars();
    let quoted = value.starts_with('"');
    if quoted {
        chars.next();
    }
    while let Some(c) = chars.next() {
        match c {
            '\\' => word.push(chars.next()?),
            '"' if quoted => break,
            c if !quoted && c.is_whitespace() => break,
            c => word.push(c),
        }
    }
    // The program path gets no `$` doubling; see `exec_line`.
    let word = word.replace("%%", "%");
    (!word.is_empty()).then(|| PathBuf::from(word))
}

/// The states `systemctl show` settles before any health check: the timer
/// is unknown to systemd, or systemd did not answer at all.
///
/// Without the second case a user manager that cannot be reached -- WSL
/// without systemd, `sudo -u` -- printed nothing, no `LoadState` was found,
/// and the job counted as installed. A removal could then never be confirmed,
/// so `ccred uninstall` stopped on a timer that had never existed.
fn unanswered(
    answered: bool,
    props: &std::collections::HashMap<String, String>,
    timer_file_exists: bool,
    stderr: &str,
) -> Option<State> {
    match props.get("LoadState").map(String::as_str) {
        Some("not-found") => Some(State::NotInstalled),
        Some(_) if answered => None,
        _ if !timer_file_exists => Some(State::NotInstalled),
        _ => Some(State::Unsupported {
            reason: format!(
                "the timer's unit files exist, but `systemctl --user` did not answer: {}",
                stderr.trim()
            ),
            remedy: Some("run this from a normal login session".into()),
        }),
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

    #[test]
    fn the_registered_command_reads_back_what_was_written() {
        for exe in crate::schedule::tests::awkward_exes() {
            let mut s = crate::schedule::tests::spec();
            s.exe = exe.clone();
            for hardened in [true, false] {
                let unit = render_service(&s, hardened);
                assert_eq!(registered_command(&unit), Some(exe.clone()), "{unit}");
            }
        }
        assert_eq!(registered_command("[Service]\nType=oneshot\n"), None);
    }

    /// Under `ProtectHome=read-only` a relocated directory is unwritable
    /// unless the unit names it, and the refresh fails on every run.
    #[test]
    fn relocated_directories_are_writable_in_both_profiles() {
        let s = crate::schedule::tests::spec().with_locations(&crate::paths::Locations {
            ccred_home: Some("/data/my ccred".into()),
            claude_config_dir: Some("/data/100%/claude".into()),
        });
        for hardened in [true, false] {
            let unit = render_service(&s, hardened);
            assert!(
                unit.contains("ReadWritePaths=\"-/data/my ccred\"\n"),
                "{unit}"
            );
            assert!(
                unit.contains("ReadWritePaths=\"-/data/100%%/claude\"\n"),
                "{unit}"
            );
            assert!(
                unit.contains("--claude-config-dir /data/100%%/claude "),
                "{unit}"
            );
        }
    }

    #[test]
    fn a_percent_in_the_binary_path_survives_the_round_trip() {
        let mut s = crate::schedule::tests::spec();
        s.exe = PathBuf::from("/opt/100%/ccred");
        let unit = render_service(&s, true);
        assert!(unit.contains("ExecStart=/opt/100%%/ccred "), "{unit}");
        assert_eq!(registered_command(&unit), Some(s.exe));
    }

    #[test]
    fn a_user_manager_that_does_not_answer_is_not_a_timer() {
        let none = std::collections::HashMap::new();
        assert!(matches!(
            unanswered(false, &none, false, "Failed to connect to bus"),
            Some(State::NotInstalled)
        ));
        assert!(matches!(
            unanswered(false, &none, true, "Failed to connect to bus"),
            Some(State::Unsupported { .. })
        ));

        let props = |state: &str| parse_properties(&format!("LoadState={state}\n"));
        assert!(matches!(
            unanswered(true, &props("not-found"), true, ""),
            Some(State::NotInstalled)
        ));
        assert!(unanswered(true, &props("loaded"), true, "").is_none());
    }

    #[test]
    fn a_binary_path_systemd_would_refuse_is_refused_first() {
        for exe in [
            "/opt/it's/ccred",
            "/opt/say\"hi\"/ccred",
            "/opt/back\\slash/ccred",
        ] {
            let mut s = crate::schedule::tests::spec();
            s.exe = PathBuf::from(exe);
            let err = Systemd.render(&s).unwrap_err();
            assert!(
                err.to_string().contains("install ccred somewhere else"),
                "{err}"
            );
        }
        let mut s = crate::schedule::tests::spec();
        s.exe = PathBuf::from("/opt/tab\there/ccred");
        assert!(Systemd.render(&s).is_err(), "a control character too");
        let mut s = crate::schedule::tests::spec();
        s.exe = PathBuf::from("/opt/100% $HOME/ccred");
        assert!(Systemd.render(&s).is_ok());
    }

    /// Split a unit file value the way systemd does, for the characters
    /// `unit_word` has to handle -- and refuse anything systemd would expand.
    fn systemd_words(value: &str, dollars_expand: bool) -> Vec<String> {
        let mut words = Vec::new();
        let mut chars = value.chars().peekable();
        loop {
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            let Some(&first) = chars.peek() else {
                return words;
            };
            let quoted = first == '"';
            if quoted {
                chars.next();
            }
            let mut raw = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => raw.push(chars.next().expect("dangling escape")),
                    '"' if quoted => break,
                    '"' | '\'' if !quoted => panic!("unbalanced quoting in {value:?}"),
                    c if !quoted && c.is_whitespace() => break,
                    c => raw.push(c),
                }
            }
            let mut word = String::new();
            let mut it = raw.chars().peekable();
            while let Some(c) = it.next() {
                match c {
                    '%' => match it.next() {
                        Some('%') => word.push('%'),
                        other => panic!("specifier %{other:?} would expand in {value:?}"),
                    },
                    '$' if dollars_expand => match it.peek() {
                        Some('$') => {
                            it.next();
                            word.push('$');
                        }
                        Some(n) if n.is_alphanumeric() || *n == '_' || *n == '{' => {
                            panic!("a variable would expand in {value:?}")
                        }
                        _ => word.push('$'),
                    },
                    c => word.push(c),
                }
            }
            words.push(word);
        }
    }

    /// Any of these once meant a unit with no usable command, or a different
    /// one: `'` and `"` unbalance the quoting, `\` escapes the next
    /// character, `$` expands a variable and `%` a specifier.
    #[test]
    fn hostile_characters_reach_the_command_intact() {
        let mut s = crate::schedule::tests::spec().with_locations(&crate::paths::Locations {
            ccred_home: Some("/data/o'neil/$HOME/100%".into()),
            claude_config_dir: Some(r#"/data/say"hi"\there x"#.into()),
        });
        s.exe = PathBuf::from("/opt/my $x tools/ccred");
        let unit = render_service(&s, true);
        let line = unit
            .lines()
            .find_map(|l| l.strip_prefix("ExecStart="))
            .unwrap();

        // The program path first, with no variable expansion; the arguments
        // after it, with.
        let (program, args) = match line.strip_prefix('"') {
            Some(rest) => {
                let end = rest.find('"').unwrap();
                (&line[..end + 2], &line[end + 2..])
            }
            None => line.split_at(line.find(' ').unwrap()),
        };
        assert_eq!(
            systemd_words(program, false),
            [s.exe.to_string_lossy().into_owned()],
            "{line}"
        );
        assert_eq!(systemd_words(args, true), s.args, "{line}");
        assert_eq!(registered_command(&unit), Some(s.exe.clone()));

        for dir in &s.writable {
            let value = writable_path(dir);
            assert_eq!(
                systemd_words(&value, false),
                [format!("-{}", dir.display())],
                "{value}"
            );
        }
    }

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
