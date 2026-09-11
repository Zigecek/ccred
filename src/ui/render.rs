//! One renderer per command.
//!
//! This is the single place to audit for the rule that no output may ever
//! contain a token: operations return data, and every human-readable byte
//! this tool writes is produced by a function below.

use anstream::{eprintln, println};

use super::{
    ACCENT, Align, Cell, ERR, Fields, HEAD, LABEL, MUTED, NAME, NOMINAL_WINDOW_DAYS, OK, Table,
    Theme, VALUE, WARN, ago, days_style, heading, left, meter, paint,
};
use crate::ops::doctor::{Finding, Severity};
use crate::ops::refresh::{Decision, RefreshReport};
use crate::ops::simple::{CurrentReport, ProfileRow, SaveReport, ScheduleSetup};
use crate::ops::switch::{OutgoingSync, SwitchReport};
use crate::schedule::{Backend, Health, RenderedFile, State, Warning};

/// Every block is indented by this much. The left margin is what makes a
/// terminal read as laid out rather than dumped.
const PAD: &str = "  ";

/// Nominal lifetime of an access token. Like `NOMINAL_WINDOW_DAYS`, this
/// scales a meter rather than claiming to be the issued value.
const NOMINAL_ACCESS_MS: i64 = 8 * 3_600_000;

/// Width of every meter, so the columns of `current` and `list` agree.
const METER_CELLS: usize = 14;

// --- shared pieces --------------------------------------------------------

/// A leading status glyph plus a message, with any following lines hung
/// underneath it.
fn callout(style: anstyle::Style, glyph: &str, head: &str, body: &[&str]) {
    println!("{PAD}{} {}", paint(style, glyph), paint(VALUE, head));
    for line in body {
        println!("{PAD}  {}", paint(MUTED, line));
    }
}

/// A pid list that stays one line. Six live sessions is normal, and printing
/// all six teaches the reader nothing the count does not.
fn pids(list: &[u32]) -> String {
    const SHOWN: usize = 3;
    let head = list
        .iter()
        .take(SHOWN)
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    match list.len().checked_sub(SHOWN) {
        Some(rest) if rest > 0 => format!("{head} and {rest} more"),
        _ => head,
    }
}

/// Turn an API tier into the name the plan is sold under.
///
/// The raw values are `default_claude_max_20x` and friends. The fallback
/// keeps an unrecognised tier readable instead of hiding it.
fn plan_label(raw: &str) -> String {
    match raw {
        "default_claude_max_20x" => "Max 20x".to_string(),
        "default_claude_max_5x" => "Max 5x".to_string(),
        "default_claude_pro" => "Pro".to_string(),
        "default_claude_free" | "free" => "Free".to_string(),
        other => other
            .trim_start_matches("default_claude_")
            .replace('_', " "),
    }
}

/// "1 warning", "3 warnings". Singular counts read as bugs otherwise.
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// `12 days` in the colour that says how worried to be.
fn window_cell(days: Option<i64>) -> Cell {
    match days {
        Some(d) if d < 0 => Cell::new("expired", ERR),
        Some(d) => Cell::new(format!("{d}d"), days_style(d)),
        None => Cell::new("-", MUTED),
    }
}

// --- current --------------------------------------------------------------

pub fn current(theme: &Theme, r: &CurrentReport) {
    let g = theme.glyphs;
    println!();

    // Identity line: who this is, in one glance.
    let who = match &r.active_profile {
        Some(name) => format!("{} {}", paint(ACCENT, g.active), paint(NAME, name)),
        None => paint(MUTED, "(no active profile)"),
    };
    let mut ident = format!("{PAD}{who}   {}", paint(VALUE, &r.account));
    if let Some(plan) = &r.plan {
        ident.push_str(&format!(
            "   {} {}",
            paint(MUTED, g.bullet),
            paint(MUTED, &plan_label(plan))
        ));
    }
    println!("{ident}");
    println!();

    if r.logged_in {
        let mut f = Fields::new();
        if let Some(ms) = r.access_ms_left {
            f.add("Access", token_meter(theme, ms, NOMINAL_ACCESS_MS));
        }
        if let Some(ms) = r.refresh_ms_left {
            f.add(
                "Refresh",
                token_meter(theme, ms, NOMINAL_WINDOW_DAYS * 86_400_000),
            );
        }
        if let Some(t) = r.last_synced_at_ms {
            f.add("Synced", paint(VALUE, &ago(t, crate::store::now_ms())));
        }
        f.add(
            "Profiles",
            format!(
                "{}   {}",
                paint(VALUE, &format!("{} saved", r.profile_count)),
                paint(MUTED, "(ccred list)")
            ),
        );
        println!("{}", f.render(PAD));
    } else {
        callout(
            ERR,
            g.err,
            "not logged in",
            &["run `claude login`, then `ccred save <name>`"],
        );
    }

    if !r.claude_running.is_empty() {
        println!();
        callout(
            WARN,
            g.warn,
            &format!("Claude Code is running (pid {})", pids(&r.claude_running)),
            &["quit it before switching, or pass --force and accept the risk"],
        );
    }

    if let Some(msg) = &r.pointer_mismatch {
        println!();
        callout(
            ERR,
            g.err,
            msg,
            &["run `ccred doctor` for what to do about it"],
        );
    }
    println!();
}

/// A meter and the remaining time beside it, coloured by urgency.
fn token_meter(theme: &Theme, ms_left: i64, nominal_ms: i64) -> String {
    let days = ms_left / 86_400_000;
    let bar = meter(theme, ms_left, nominal_ms, METER_CELLS);
    let style = if ms_left <= 0 {
        ERR
    } else if nominal_ms > 86_400_000 {
        days_style(days)
    } else {
        // An access token is meant to be short-lived, so a low reading is
        // routine rather than a warning.
        OK
    };
    format!("{}  {}", paint(style, &bar), paint(VALUE, &left(ms_left)))
}

// --- list -----------------------------------------------------------------

pub fn list(theme: &Theme, rows: &[ProfileRow]) {
    let g = theme.glyphs;
    if rows.is_empty() {
        println!();
        callout(
            MUTED,
            g.bullet,
            "no profiles yet",
            &["run `ccred save <name>` while logged in to create the first one"],
        );
        println!();
        return;
    }

    let now = crate::store::now_ms();
    let mut t = Table::new(&[
        ("PROFILE", Align::Left),
        ("ACCOUNT", Align::Left),
        ("PLAN", Align::Left),
        ("REFRESH WINDOW", Align::Left),
        ("LEFT", Align::Right),
        ("SYNCED", Align::Right),
        ("STATE", Align::Left),
    ]);

    for r in rows {
        // The marker rides in the name column: a column of its own would
        // need a blank header, which reads as a stray indent.
        let gutter = " ".repeat(g.active.chars().count());
        let name = if r.active {
            Cell::new(format!("{} {}", g.active, r.name), ACCENT)
        } else {
            Cell::new(format!("{gutter} {}", r.name), NAME)
        };
        let bar = match r.refresh_days_left {
            Some(d) => Cell::new(
                meter(theme, d, NOMINAL_WINDOW_DAYS, METER_CELLS),
                days_style(d),
            ),
            None => Cell::new("-", MUTED),
        };
        let state = if r.needs_login {
            Cell::new("needs login", ERR)
        } else if !r.healthy {
            Cell::new("broken", ERR)
        } else if r.refresh_days_left.is_some_and(|d| d <= 5) {
            Cell::new("expiring", WARN)
        } else {
            Cell::new("ok", OK)
        };

        t.row(vec![
            name,
            Cell::plain(&r.account),
            Cell::new(
                r.subscription
                    .as_deref()
                    .map(plan_label)
                    .unwrap_or_else(|| "-".into()),
                MUTED,
            ),
            bar,
            window_cell(r.refresh_days_left),
            Cell::new(
                r.last_synced_at_ms
                    .map(|t| ago(t, now))
                    .unwrap_or_else(|| "-".into()),
                MUTED,
            ),
            state,
        ]);
        if let Some(note) = &r.note {
            t.note(note);
        }
    }

    println!();
    println!("{}", t.render(theme, PAD));
    println!();

    let attention = rows
        .iter()
        .filter(|r| !r.healthy || r.needs_login || r.refresh_days_left.is_some_and(|d| d <= 5))
        .count();
    let summary = if attention == 0 {
        paint(
            MUTED,
            &format!("{}, all healthy", plural(rows.len(), "profile", "profiles")),
        )
    } else {
        format!(
            "{}  {}",
            paint(
                MUTED,
                &format!("{},", plural(rows.len(), "profile", "profiles"))
            ),
            paint(WARN, &format!("{attention} need attention"))
        )
    };
    println!("{PAD}{summary}");
    println!();
}

// --- save, switch, rm -----------------------------------------------------

pub fn save(theme: &Theme, r: &SaveReport) {
    let g = theme.glyphs;
    println!();
    println!(
        "{PAD}{} saved {} {}",
        paint(OK, g.ok),
        paint(NAME, &r.name),
        paint(MUTED, &format!("({})", r.account))
    );
    println!("{PAD}  {}", paint(MUTED, &r.outcome));
    match &r.schedule {
        Some(ScheduleSetup::Installed { next_run }) => {
            println!();
            callout(
                OK,
                g.ok,
                "automatic refresh registered, so the idle profile cannot expire",
                &[
                    &format!("next run {}", next_run.as_deref().unwrap_or("unknown")),
                    "`ccred schedule uninstall` removes it again",
                ],
            );
        }
        Some(ScheduleSetup::Failed { reason }) => {
            println!();
            callout(
                WARN,
                g.warn,
                "the profile is saved, but automatic refresh could not be registered",
                &[reason, "run `ccred schedule install` to see the full error"],
            );
        }
        None => {}
    }
    println!();
}

pub fn removed(theme: &Theme, name: &str) {
    let g = theme.glyphs;
    println!();
    println!("{PAD}{} removed {}", paint(OK, g.ok), paint(NAME, name));
    println!();
}

pub fn restored(theme: &Theme, name: &str) {
    let g = theme.glyphs;
    println!();
    println!(
        "{PAD}{} restored {} from its last-known-good copy",
        paint(OK, g.ok),
        paint(NAME, name)
    );
    println!(
        "{PAD}  {}",
        paint(
            MUTED,
            "run `ccred doctor` to confirm, and log in again if it is still refused"
        )
    );
    println!();
}

pub fn switch(theme: &Theme, r: &SwitchReport) {
    let g = theme.glyphs;
    println!();

    if let Some(from) = &r.from {
        println!(
            "{PAD}{} {} {} {}",
            paint(OK, g.ok),
            paint(MUTED, from),
            paint(MUTED, g.arrow),
            paint(NAME, &r.to)
        );
    } else {
        println!(
            "{PAD}{} switched to {}",
            paint(OK, g.ok),
            paint(NAME, &r.to)
        );
    }
    println!("{PAD}  {}", paint(MUTED, &r.account));

    let mut notes: Vec<(anstyle::Style, String)> = Vec::new();
    if let Some(recovered) = &r.recovered {
        notes.push((
            WARN,
            format!("recovered an interrupted switch: {recovered}"),
        ));
    }
    match &r.outgoing {
        OutgoingSync::Synced(name) => notes.push((
            MUTED,
            format!("the credentials that were live are now saved in '{name}'"),
        )),
        OutgoingSync::Skipped {
            profile,
            reason,
            backup,
        } => {
            notes.push((ERR, format!("did not update '{profile}': {reason}")));
            if let Some(path) = backup {
                notes.push((
                    WARN,
                    format!(
                        "the credentials that were live are not in any profile; copied to {path}"
                    ),
                ));
            }
        }
        OutgoingSync::NothingActive => {}
    }
    if !r.identity_restored {
        notes.push((
            MUTED,
            "account details were not restored; Claude Code will refetch them".into(),
        ));
    }
    for w in &r.warnings {
        notes.push((WARN, w.clone()));
    }
    if !r.claude_running.is_empty() {
        notes.push((
            WARN,
            format!(
                "Claude Code is still running (pid {}); restart it to pick this up",
                pids(&r.claude_running)
            ),
        ));
    }

    if !notes.is_empty() {
        println!();
        for (style, text) in notes {
            let glyph = if style == MUTED { g.bullet } else { g.warn };
            println!("{PAD}{} {}", paint(style, glyph), paint(VALUE, &text));
        }
    }
    println!();
}

// --- refresh --------------------------------------------------------------

/// What each decision means, in the words a reader would use.
fn decision_label(d: Decision) -> (&'static str, anstyle::Style) {
    match d {
        Decision::MirrorActive => ("mirrored", OK),
        Decision::SkipFresh => ("up to date", MUTED),
        Decision::SkipAccessLive => ("not due yet", MUTED),
        Decision::SkipBackoff => ("backing off", WARN),
        Decision::SkipCap => ("rate capped", WARN),
        Decision::Refresh => ("refreshed", OK),
        Decision::NeedsLogin => ("needs login", ERR),
        Decision::Broken => ("broken", ERR),
    }
}

pub fn refresh(theme: &Theme, r: &RefreshReport) {
    let g = theme.glyphs;
    println!();

    if let Some(reason) = r.status.strip_prefix("skipped: ") {
        callout(MUTED, g.bullet, "nothing to do", &[reason]);
        println!();
        return;
    }
    if r.profiles.is_empty() {
        callout(MUTED, g.bullet, "no profiles to refresh", &[]);
        println!();
        return;
    }

    let mut t = Table::new(&[
        ("PROFILE", Align::Left),
        ("RESULT", Align::Left),
        ("WINDOW", Align::Left),
    ]);
    for p in &r.profiles {
        let (label, style) = decision_label(p.decision);
        let window = match (p.window_days_before, p.window_days_after) {
            (Some(before), Some(after)) if after != before => {
                format!("{before}d {} {after}d", g.arrow)
            }
            (Some(before), _) => format!("{before}d"),
            _ => "-".to_string(),
        };
        t.row(vec![
            Cell::new(&p.name, NAME),
            Cell::new(label, style),
            Cell::new(window, VALUE),
        ]);
        if let Some(detail) = &p.detail {
            t.note(detail);
        }
    }
    println!("{}", t.render(theme, PAD));
    println!();

    let needs = r
        .profiles
        .iter()
        .filter(|p| matches!(p.decision, Decision::NeedsLogin | Decision::Broken))
        .count();
    let moved = r
        .profiles
        .iter()
        .filter(|p| matches!(p.decision, Decision::Refresh | Decision::MirrorActive))
        .count();
    let mut summary = paint(
        MUTED,
        &format!(
            "{}, {moved} updated",
            plural(r.profiles.len(), "profile", "profiles")
        ),
    );
    if needs > 0 {
        summary.push_str(&format!(
            "  {}",
            paint(ERR, &format!("{needs} need a login"))
        ));
    }
    println!("{PAD}{summary}");
    println!();
}

// --- schedule -------------------------------------------------------------

fn backend_label(b: Backend) -> &'static str {
    match b {
        Backend::Systemd => "systemd user timer",
        Backend::Launchd => "launchd agent",
        Backend::TaskScheduler => "Windows Task Scheduler",
    }
}

pub fn schedule_status(theme: &Theme, state: &State, backend: Backend) {
    let g = theme.glyphs;
    println!();
    match state {
        State::NotInstalled => {
            callout(
                MUTED,
                g.bullet,
                "not installed",
                &["run `ccred schedule install` to refresh profiles automatically"],
            );
        }
        State::Unsupported { reason, remedy } => {
            let body: Vec<&str> = remedy.as_deref().into_iter().collect();
            callout(WARN, g.warn, reason, &body);
        }
        State::Installed(h) => {
            let mut f = Fields::new();
            f.add("Backend", paint(VALUE, backend_label(backend)));
            f.add(
                "State",
                if h.enabled {
                    paint(OK, "installed and enabled")
                } else {
                    paint(WARN, "installed, but disabled")
                },
            );
            match &h.next_run {
                Some(t) => f.add("Next run", paint(VALUE, t)),
                None => f.add("Next run", paint(ERR, "never")),
            };
            if let Some(t) = &h.last_run {
                f.add("Last run", paint(MUTED, t));
            }
            println!("{}", f.render(PAD));
            print_warnings(theme, &h.warnings);
        }
    }
    println!();
}

pub fn schedule_installed(theme: &Theme, h: &Health, backend: Backend) {
    let g = theme.glyphs;
    println!();
    println!(
        "{PAD}{} scheduled with {}",
        paint(OK, g.ok),
        paint(VALUE, backend_label(backend))
    );
    println!(
        "{PAD}  {} {}",
        paint(LABEL, "next run"),
        paint(VALUE, h.next_run.as_deref().unwrap_or("unknown"))
    );
    print_warnings(theme, &h.warnings);
    println!();
}

fn print_warnings(theme: &Theme, warnings: &[Warning]) {
    if warnings.is_empty() {
        return;
    }
    println!();
    for w in warnings {
        println!(
            "{PAD}{} {}",
            paint(WARN, theme.glyphs.warn),
            paint(VALUE, &w.to_string())
        );
    }
}

pub fn schedule_removed(theme: &Theme) {
    println!();
    println!("{PAD}{} schedule removed", paint(OK, theme.glyphs.ok));
    println!();
}

pub fn dry_run(theme: &Theme, files: &[RenderedFile]) {
    for file in files {
        println!();
        println!("{}", heading(theme, PAD, &file.path.display().to_string()));
        println!();
        for line in file.contents.lines() {
            println!("{PAD}{}", paint(MUTED, line));
        }
    }
    println!();
    println!(
        "{PAD}{}",
        paint(
            MUTED,
            "nothing was written -- drop --dry-run to register it"
        )
    );
    println!();
}

// --- doctor ---------------------------------------------------------------

pub fn doctor(theme: &Theme, findings: &[Finding]) {
    let g = theme.glyphs;
    println!();
    println!("{}", heading(theme, PAD, "Checks"));
    println!();

    for f in findings {
        let (glyph, style) = match f.severity {
            Severity::Ok => (g.ok, OK),
            Severity::Warn => (g.warn, WARN),
            Severity::Error => (g.err, ERR),
        };
        let title_style = if f.severity == Severity::Ok {
            MUTED
        } else {
            VALUE
        };
        println!(
            "{PAD}{} {}",
            paint(style, glyph),
            paint(title_style, &f.title)
        );
        if let Some(detail) = &f.detail {
            println!("{PAD}  {}", paint(MUTED, detail));
        }
    }

    let count = |s: Severity| findings.iter().filter(|f| f.severity == s).count();
    let (errors, warns, oks) = (
        count(Severity::Error),
        count(Severity::Warn),
        count(Severity::Ok),
    );
    println!();
    let summary = if errors > 0 {
        paint(
            ERR,
            &format!(
                "{}, {}, {oks} ok",
                plural(errors, "error", "errors"),
                plural(warns, "warning", "warnings")
            ),
        )
    } else if warns > 0 {
        paint(
            WARN,
            &format!("{}, {oks} ok", plural(warns, "warning", "warnings")),
        )
    } else {
        paint(OK, &format!("all {oks} checks passed"))
    };
    println!("{PAD}{summary}");
    println!();
}

// --- failures -------------------------------------------------------------

/// The next command to run, for the failures where there is an obvious one.
///
/// Deliberately not exhaustive: a wrong suggestion costs more than none, so
/// anything whose remedy depends on context gets no hint at all.
fn hint_for(e: &crate::CcredError) -> Option<&'static str> {
    use crate::CcredError as E;
    match e {
        E::ProfileNotFound(_) => Some("ccred list  shows the profiles that do exist"),
        E::InvalidCredentials(_) => Some("run `claude login`, then `ccred save <name>`"),
        E::AccountMismatch { .. } => {
            Some("save this account under its own name instead of overwriting that profile")
        }
        E::AccountUnverifiable { .. } => {
            Some("`ccred doctor` reports what is known about the live account")
        }
        E::InvalidProfileName { .. } | E::PathEscape { .. } => {
            Some("names may hold letters, digits, dot, underscore and hyphen")
        }
        _ => None,
    }
}

/// Errors go to stderr, so they survive `ccred list > file` and stay visible.
pub fn error(theme: &Theme, e: &crate::CcredError) {
    eprintln!();
    eprintln!(
        "{PAD}{} {}",
        paint(ERR, theme.glyphs.err),
        paint(HEAD, &e.to_string())
    );
    // "I/O error at <path>" alone is rarely enough to act on.
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        eprintln!("{PAD}  {}", paint(MUTED, &format!("caused by: {cause}")));
        source = cause.source();
    }
    if let Some(hint) = hint_for(e) {
        eprintln!();
        eprintln!(
            "{PAD}{} {}",
            paint(MUTED, theme.glyphs.bullet),
            paint(MUTED, hint)
        );
    }
    eprintln!();
}

pub fn unknown_command(theme: &Theme, guess: &str) {
    let g = theme.glyphs;
    eprintln!();
    eprintln!(
        "{PAD}{} {}",
        paint(ERR, g.err),
        paint(HEAD, &format!("unknown command '{guess}'"))
    );
    eprintln!();
    let mut f = Fields::new();
    f.add(
        "did you mean",
        paint(NAME, &format!("ccred switch {guess}")),
    );
    f.add("full list", paint(VALUE, "ccred --help"));
    eprintln!("{}", f.render(PAD));
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unrecognised tier must stay readable rather than disappear. The
    /// mapping is cosmetic; losing information to it would not be.
    #[test]
    fn an_unknown_plan_is_tidied_but_never_hidden() {
        assert_eq!(plan_label("default_claude_max_20x"), "Max 20x");
        assert_eq!(plan_label("default_claude_pro"), "Pro");
        assert_eq!(plan_label("default_claude_team_premium"), "team premium");
        assert_eq!(plan_label("something_new"), "something new");
    }

    #[test]
    fn a_long_pid_list_stays_on_one_line() {
        assert_eq!(pids(&[1, 2]), "1, 2");
        assert_eq!(pids(&[1, 2, 3]), "1, 2, 3");
        assert_eq!(pids(&[1, 2, 3, 4, 5]), "1, 2, 3 and 2 more");
        assert_eq!(pids(&[]), "");
    }

    #[test]
    fn counts_of_one_do_not_read_as_a_bug() {
        assert_eq!(plural(1, "profile", "profiles"), "1 profile");
        assert_eq!(plural(0, "profile", "profiles"), "0 profiles");
        assert_eq!(plural(2, "profile", "profiles"), "2 profiles");
    }

    /// Every decision needs words. A `{:?}` leaking into the output would be
    /// the same class of slip that printed `pid [12204, 26656]` at users.
    #[test]
    fn every_decision_has_a_human_label() {
        for d in [
            Decision::MirrorActive,
            Decision::SkipFresh,
            Decision::SkipBackoff,
            Decision::SkipCap,
            Decision::Refresh,
            Decision::NeedsLogin,
            Decision::Broken,
        ] {
            let (label, _) = decision_label(d);
            assert!(!label.is_empty(), "{d:?} has no label");
            assert!(
                label.chars().all(|c| c.is_ascii_lowercase() || c == ' '),
                "{d:?} produced {label:?}, which looks like a Debug spelling"
            );
        }
    }

    /// Same rule for scheduler warnings, which reach `doctor` as well.
    #[test]
    fn every_scheduler_warning_has_a_human_label() {
        for w in [
            Warning::LingerDisabled,
            Warning::NoGuiSession,
            Warning::DisabledByUser,
            Warning::OnBatteryBlocked,
            Warning::RegisteredButNeverFires,
            Warning::RunsOnlyWhenSignedIn,
            Warning::BinaryMissing("/nope/ccred".into()),
        ] {
            let text = w.to_string();
            assert!(!text.is_empty(), "{w:?} has no wording");
            assert!(
                !text.contains('_') || text.contains("/nope"),
                "{w:?} produced {text:?}, which looks like an enum name"
            );
        }
    }

    /// A wrong suggestion costs more than none, so anything whose remedy
    /// depends on context must not get one.
    #[test]
    fn hints_are_offered_only_where_the_next_step_is_unambiguous() {
        use crate::CcredError as E;
        assert!(hint_for(&E::ProfileNotFound("x".into())).is_some());
        assert!(
            hint_for(&E::InvalidProfileName {
                name: "x".into(),
                reason: "bad"
            })
            .is_some()
        );
        assert!(
            hint_for(&E::Schedule("systemd said no".into())).is_none(),
            "a scheduler failure has no single next step"
        );
        assert!(
            hint_for(&E::LossyRewrite {
                dropped: vec!["k".into()]
            })
            .is_none(),
            "a lossy rewrite needs a human to look, not a command to run"
        );
    }
}
