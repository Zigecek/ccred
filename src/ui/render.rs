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
use crate::desktop::DesktopSwitch;
use crate::ops::desktop::{DesktopRow, DesktopSave, DesktopState, DesktopStatus};
use crate::ops::doctor::{Finding, Severity};
use crate::ops::refresh::{Decision, RefreshReport};
use crate::ops::simple::{
    CurrentReport, Listing, PointerNote, ProfileRow, SaveReport, ScheduleSetup,
};
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
        "default_claude_free" | "free" | "claude_free" => "Free".to_string(),
        "claude_pro" => "Pro".to_string(),
        "claude_max" => "Max".to_string(),
        "claude_team" => "Team".to_string(),
        "claude_enterprise" => "Enterprise".to_string(),
        // The generic tier with no organization type to go on.
        "default_claude_ai" => "claude.ai".to_string(),
        other => other
            .trim_start_matches("default_claude_")
            .trim_start_matches("claude_")
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
        // Negative days, not the word: `-20d` and `-20000d` call for very
        // different conclusions, and "expired" read the same for both.
        Some(d) if d < 0 => Cell::new(format!("{d}d"), ERR),
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
        let mut desktop_advice = None;
        if let Some(d) = &r.desktop {
            let (value, advice) = desktop_status_line(d, r.active_profile.as_deref());
            f.add("Desktop", value);
            // Its own row, rather than a comma list inside the value above.
            if !d.parked.is_empty() {
                f.add("Parked", paint(MUTED, &d.parked.join(", ")));
            }
            desktop_advice = advice;
        }
        println!("{}", f.render(PAD));
        if let Some(advice) = desktop_advice {
            callout(
                WARN,
                g.warn,
                &advice,
                &["`ccred switch <name>-desktop` moves the Desktop's login"],
            );
        }
    } else if let Some(why) = &r.live_error {
        callout(
            ERR,
            g.err,
            "the live credentials cannot be read",
            &[why, "`ccred doctor` has the details"],
        );
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
    // Context, not a warning: these sessions are on another login by design,
    // and the line is what stops "I switched, but the Desktop did not" from
    // reading as a bug in either program.
    if !r.desktop_sessions.is_empty() {
        println!();
        callout(
            MUTED,
            g.bullet,
            &format!(
                "Claude Desktop is running ({})",
                plural(r.desktop_sessions.len(), "session", "sessions")
            ),
            &["its sessions use the Desktop's own login; `ccred switch <name>-desktop` moves that"],
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

/// Which account the Desktop is on, and whether that is the one Claude Code
/// is on. The name is the point; everything else is context.
///
/// One value, and the advice that goes with it kept separate. This used to
/// be four segments joined into a single `Fields` value -- the account, an
/// advisory, `(running)` and a comma list of parked names, ~130 columns in
/// the worst case, in a row whose peers read `2 saved   (ccred list)`.
fn desktop_status_line(d: &DesktopStatus, active: Option<&str>) -> (String, Option<String>) {
    let same_account =
        |p: &str| active.is_some_and(|a| p.strip_suffix(crate::desktop::SUFFIX) == Some(a));
    let (value, advice) = match (&d.profile, d.installed, d.logged_in) {
        (Some(p), _, _) if d.signed_out_in_app => (
            paint(NAME, p),
            Some("signed out inside the app".to_string()),
        ),
        (Some(p), _, _) if same_account(p) => (paint(NAME, p), None),
        (Some(p), _, _) => (
            paint(NAME, p),
            Some("not the active profile's account".to_string()),
        ),
        (None, true, true) => (paint(MUTED, "an account that is not a saved profile"), None),
        (None, true, false) => (paint(MUTED, "not logged in"), None),
        (None, false, _) => (paint(MUTED, "no active login"), None),
    };
    let value = if d.running {
        format!("{value}   {}", paint(MUTED, "(running)"))
    } else {
        value
    };
    (value, advice)
}

/// What a Desktop switch did: a headline, and the rest underneath it.
///
/// One line each is what this was, with the parts joined by semicolons and
/// colons -- 155 columns of it in the worst case. `callout` exists for
/// exactly this shape and every other report in this file uses it.
fn desktop_switch_note(
    d: &DesktopSwitch,
    to: &str,
    arrow: &str,
) -> (anstyle::Style, String, Vec<String>) {
    match d {
        DesktopSwitch::AlreadyOn => (
            MUTED,
            format!("Claude Desktop is already logged in as {to}"),
            Vec::new(),
        ),
        DesktopSwitch::LeftAlone => (
            WARN,
            "Claude Desktop was left as it is".to_string(),
            vec![
                "it is logged in as an account that is not a saved profile".to_string(),
                format!("and nothing is parked for {to}"),
            ],
        ),
        DesktopSwitch::Moved {
            parked_as,
            restored: true,
        } => match parked_as {
            Some(p) => (
                OK,
                format!("{p} {arrow} {to}"),
                vec!["start Claude Desktop".to_string()],
            ),
            None => (
                OK,
                format!("{to} restored"),
                vec!["start Claude Desktop".to_string()],
            ),
        },
        DesktopSwitch::Moved {
            parked_as,
            restored: false,
        } => {
            let mut body = vec![
                format!("start Claude Desktop and log in as {to}"),
                format!("that login is {to}'s from then on"),
            ];
            if let Some(p) = parked_as {
                body.insert(0, format!("{p} was parked in its place"));
            }
            (OK, format!("{to} has no parked login yet"), body)
        }
        DesktopSwitch::Failed { error, parked_as } => {
            let mut body = vec![error.clone()];
            if let Some(where_it_went) = parked_as {
                body.push(format!("the login that was live is now at {where_it_went}"));
            }
            body.push("move the directory by hand to finish it".to_string());
            (ERR, "Claude Desktop was not switched".to_string(), body)
        }
    }
}

// --- list -----------------------------------------------------------------

pub fn list(theme: &Theme, listing: &Listing, pointer: Option<&PointerNote>) {
    let g = theme.glyphs;
    let rows = &listing.profiles;
    if rows.is_empty() && listing.desktop.is_empty() {
        println!();
        callout(
            MUTED,
            g.bullet,
            "no profiles yet",
            &["run `ccred save <name>` while logged in to create the first one"],
        );
        pointer_note(theme, pointer);
        println!();
        return;
    }
    // Two tables, each with a heading. Without them the second reads as a
    // second header row of a broken first one -- and the pointer note, which
    // is about the Claude Code half, came between them.
    let both = !rows.is_empty() && !listing.desktop.is_empty();
    if rows.is_empty() {
        println!();
        callout(MUTED, g.bullet, "no Claude Code profiles", &[]);
    } else {
        if both {
            println!();
            println!("{}", heading(theme, PAD, "Claude Code"));
        }
        profile_table(theme, rows);
    }
    if !listing.desktop.is_empty() {
        if both {
            println!("{}", heading(theme, PAD, "Claude Desktop"));
        }
        desktop_table(theme, &listing.desktop);
    }
    pointer_note(theme, pointer);
    println!();
}

/// The Desktop logins: one row per `-desktop` profile, and where its login
/// is. No meters -- there is nothing this tool can read to fill one.
fn desktop_table(theme: &Theme, rows: &[DesktopRow]) {
    let g = theme.glyphs;
    let now = crate::store::now_ms();
    let mut t = Table::new(&[
        ("DESKTOP", Align::Left),
        ("ACCOUNT", Align::Left),
        ("SYNCED", Align::Right),
        ("STATE", Align::Left),
    ]);
    for r in rows {
        let gutter = " ".repeat(g.active.chars().count());
        let name = if r.active {
            Cell::new(format!("{} {}", g.active, r.name), ACCENT)
        } else {
            Cell::new(format!("{gutter} {}", r.name), NAME)
        };
        let state = match r.state {
            // Not "logged in, running": every other status in this program
            // is one phrase, and `current` reports a running Desktop.
            DesktopState::LoggedIn => Cell::new("logged in", OK),
            DesktopState::SignedOut => Cell::new("signed out in the app", WARN),
            DesktopState::Parked => Cell::new("parked", VALUE),
            DesktopState::NoLogin => Cell::new("no login", WARN),
        };
        t.row(vec![
            name,
            Cell::plain(&r.account),
            Cell::new(
                r.last_synced_at_ms
                    .map(|t| ago(t, now))
                    .unwrap_or_else(|| "-".into()),
                MUTED,
            ),
            state,
        ]);
        // The words for a count live here, once. The listing built its own
        // sentence out of them, 128 columns of it, while `desktop_switch`
        // had the same phrase a few lines further down.
        if !r.waiting.is_empty() {
            t.note(format!(
                "{} and {} from a sign-out are waiting; `ccred switch {}` puts them back",
                plural(r.waiting.sessions, "session", "sessions"),
                plural(r.waiting.groups, "sidebar group", "sidebar groups"),
                r.name
            ));
        }
        if let Some(note) = &r.note {
            t.note(note);
        }
    }
    println!();
    println!("{}", t.render(theme, PAD));
}

fn profile_table(theme: &Theme, rows: &[ProfileRow]) {
    let g = theme.glyphs;

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
        // One space after the comma. Two of them read as a typo here, where
        // the comma already does the separating; the refresh summary uses two
        // because its clauses carry no punctuation between them.
        format!(
            "{} {}",
            paint(
                MUTED,
                &format!("{},", plural(rows.len(), "profile", "profiles"))
            ),
            paint(
                WARN,
                &plural(attention, "needs attention", "need attention")
            )
        )
    };
    println!("{PAD}{summary}");
}

/// A pointer that names nothing, or that cannot be read. The table alone
/// shows either as an absent marker, which reads as "none active" rather than
/// "the one you were using is gone".
fn pointer_note(theme: &Theme, note: Option<&PointerNote>) {
    let Some(note) = note else { return };
    println!();
    match note {
        PointerNote::Missing { name } => callout(
            ERR,
            theme.glyphs.err,
            &format!("the active profile '{name}' does not exist"),
            &["`ccred switch <name>` to point at one that does"],
        ),
        PointerNote::Damaged { why } => callout(
            ERR,
            theme.glyphs.err,
            "the active-profile pointer cannot be read",
            &[why, "`ccred switch <name>` writes a new one"],
        ),
    }
}

// --- save, switch, rm -----------------------------------------------------

pub fn save(theme: &Theme, r: &SaveReport) {
    let g = theme.glyphs;
    println!();
    // One row per half, always, so what was and was not saved is never a
    // matter of what is missing from the output. A table rather than hand-laid
    // columns: the two names differ by the eight characters of the Desktop
    // suffix, so two spaces between them line up nowhere.
    let mut t = Table::new(&[
        ("", Align::Left),
        ("PROFILE", Align::Left),
        ("ACCOUNT", Align::Left),
        ("RESULT", Align::Left),
    ]);
    match (&r.code, &r.code_skipped) {
        (Some(c), _) => {
            t.row(vec![
                Cell::new(g.ok, OK),
                Cell::new(&r.name, NAME),
                Cell::new(&c.account, VALUE),
                Cell::new(&c.outcome, MUTED),
            ]);
        }
        (None, Some(why)) => {
            t.row(vec![
                Cell::new(g.bullet, MUTED),
                Cell::new(&r.name, MUTED),
                Cell::new("-", MUTED),
                Cell::new(format!("not saved: {why}"), MUTED),
            ]);
        }
        (None, None) => {}
    }
    let desktop_name = format!("{}{}", r.name, crate::desktop::SUFFIX);
    match &r.desktop {
        Some(DesktopSave::Created { account }) => {
            t.row(vec![
                Cell::new(g.ok, OK),
                Cell::new(&desktop_name, NAME),
                Cell::new(account, VALUE),
                Cell::new("created", MUTED),
            ]);
        }
        Some(DesktopSave::Updated { account }) => {
            t.row(vec![
                Cell::new(g.ok, OK),
                Cell::new(&desktop_name, NAME),
                Cell::new(account, VALUE),
                Cell::new("updated", MUTED),
            ]);
        }
        Some(DesktopSave::Nothing { reason }) => {
            t.row(vec![
                Cell::new(g.bullet, MUTED),
                Cell::new(&desktop_name, MUTED),
                Cell::new("-", MUTED),
                Cell::new(format!("not saved: {reason}"), MUTED),
            ]);
        }
        Some(DesktopSave::Refused { reason }) => {
            t.row(vec![
                Cell::new(g.warn, WARN),
                Cell::new(&desktop_name, NAME),
                Cell::new("-", MUTED),
                Cell::new(format!("not saved: {reason}"), WARN),
            ]);
        }
        None => {}
    }
    println!("{}", t.render(theme, PAD));
    for w in &r.warnings {
        callout(WARN, g.warn, w, &[]);
    }
    let Some(code) = &r.code else {
        println!();
        return;
    };
    if code.already_expired {
        println!();
        callout(
            ERR,
            g.err,
            "but these credentials are already past their refresh deadline",
            &[
                "run `claude auth login`, then save again",
                concat!(
                    "if Claude Code works anyway, `ccred doctor` says whether it ",
                    "is logging in some other way"
                ),
            ],
        );
    }
    match &code.schedule {
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

pub fn removed(theme: &Theme, r: &crate::ops::simple::RemoveReport) {
    let g = theme.glyphs;
    println!();
    println!("{PAD}{} removed {}", paint(OK, g.ok), paint(NAME, &r.name));
    if r.purged_asked {
        // `--purge` with nothing to purge says so, rather than leaving the
        // impression that copies were found and kept.
        if r.purged.is_empty() {
            println!(
                "{PAD}  {}",
                paint(MUTED, "there were no earlier copies to delete")
            );
        }
        for dir in &r.purged {
            println!(
                "{PAD}  {}",
                paint(MUTED, &format!("deleted the copies in {dir}"))
            );
        }
    } else {
        match &r.backup_dir {
            Some(dir) => println!(
                "{PAD}  {}",
                paint(MUTED, &format!("a copy of its credentials is in {dir}"))
            ),
            None => println!(
                "{PAD}  {}",
                paint(WARN, "there was nothing left to copy aside first")
            ),
        }
    }
    println!();
}

pub fn desktop_removed(theme: &Theme, r: &crate::ops::desktop::DesktopRemoveReport) {
    let g = theme.glyphs;
    println!();
    println!("{PAD}{} removed {}", paint(OK, g.ok), paint(NAME, &r.name));
    if r.parked_login_removed {
        println!(
            "{PAD}  {}",
            paint(MUTED, "its parked login was deleted with it")
        );
    }
    if r.still_logged_in {
        println!(
            "{PAD}  {}",
            paint(
                MUTED,
                "Claude Desktop stays logged in as that account; its directory now belongs to \
                 no profile"
            )
        );
    }
    println!();
}

pub fn renamed(theme: &Theme, r: &crate::ops::simple::RenameReport) {
    let g = theme.glyphs;
    println!();
    println!(
        "{PAD}{} {} {} {}",
        paint(OK, g.ok),
        paint(MUTED, &r.from),
        paint(MUTED, g.arrow),
        paint(NAME, &r.to)
    );
    if r.was_active {
        println!(
            "{PAD}  {}",
            paint(MUTED, "it was the active profile, and still is")
        );
    }
    for w in &r.warnings {
        println!("{PAD}  {} {}", paint(WARN, g.warn), paint(WARN, w));
    }
    println!();
}

pub fn restored(theme: &Theme, r: &crate::ops::simple::RestoreReport) {
    let g = theme.glyphs;
    println!();
    if r.recreated {
        println!(
            "{PAD}{} put {} back from {}",
            paint(OK, g.ok),
            paint(NAME, &r.name),
            paint(MUTED, &r.from)
        );
        println!(
            "{PAD}  {}",
            paint(
                MUTED,
                "the credentials only: `ccred switch` then `ccred save` fills in the account"
            )
        );
        println!();
        return;
    }
    println!(
        "{PAD}{} restored {} from its last-known-good copy",
        paint(OK, g.ok),
        paint(NAME, &r.name)
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

    if r.from.as_deref() == Some(r.to.as_str()) {
        // Switching to the profile already active is a resync, not a move:
        // the live credentials are mirrored into it and written back out.
        // Printed as `work -> work` it reads like something went wrong.
        println!(
            "{PAD}{} {} {}",
            paint(OK, g.ok),
            paint(NAME, &r.to),
            paint(MUTED, "was already active; re-synced")
        );
    } else if let Some(from) = &r.from {
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
    if !r.desktop_sessions.is_empty() {
        notes.push((
            MUTED,
            format!(
                "Claude Desktop is running ({}); its sessions stay on the Desktop's own login",
                plural(r.desktop_sessions.len(), "session", "sessions")
            ),
        ));
    }

    if !notes.is_empty() {
        println!();
        for (style, text) in notes {
            let glyph = match style {
                s if s == MUTED => g.bullet,
                s if s == OK => g.ok,
                s if s == ERR => g.err,
                _ => g.warn,
            };
            println!("{PAD}{} {}", paint(style, glyph), paint(VALUE, &text));
        }
    }
    println!();
}

// --- desktop switch -------------------------------------------------------

pub fn desktop_switch(theme: &Theme, r: &crate::ops::desktop::DesktopReport) {
    let g = theme.glyphs;
    println!();
    let carried = |c: &crate::desktop::Carried| {
        format!(
            "{} and {}",
            plural(c.sessions, "session", "sessions"),
            plural(c.groups, "sidebar group", "sidebar groups")
        )
    };
    let (style, headline, body) = desktop_switch_note(&r.desktop, &r.to, g.arrow);
    let glyph = match style {
        s if s == OK => g.ok,
        s if s == ERR => g.err,
        s if s == WARN => g.warn,
        _ => g.bullet,
    };
    let body: Vec<&str> = body.iter().map(String::as_str).collect();
    callout(style, glyph, &headline, &body);
    if !r.restored_to_sidebar.is_empty() {
        callout(
            OK,
            g.ok,
            &format!(
                "{} put back into its sidebar",
                carried(&r.restored_to_sidebar)
            ),
            &[],
        );
    }
    if !r.waiting.is_empty() {
        callout(
            WARN,
            g.warn,
            &format!("{} from a sign-out are waiting", carried(&r.waiting)),
            &[
                "log in as that account, then quit Claude Desktop",
                &format!("`ccred switch {}` again puts them back", r.to),
            ],
        );
    }
    // One fact per row. This was three independent counts joined by commas
    // into a single 118-column line -- the shape `Fields` exists to replace,
    // and the one `schedule status` and `uninstall` already use.
    if !r.sidebar.is_empty() {
        println!();
        println!("{}", heading(theme, PAD, "Sidebar"));
        let mut f = Fields::new();
        if r.sidebar.written > 0 {
            f.add(
                "Added",
                paint(VALUE, &plural(r.sidebar.written, "session", "sessions")),
            );
        }
        if r.sidebar.removed > 0 {
            f.add(
                "Removed",
                paint(
                    VALUE,
                    &format!(
                        "{}, deleted under another account",
                        plural(r.sidebar.removed, "session", "sessions")
                    ),
                ),
            );
        }
        if r.sidebar.groups > 0 {
            f.add(
                "Groups",
                paint(VALUE, &plural(r.sidebar.groups, "added", "added")),
            );
        }
        println!("{}", f.render(PAD));
    }
    for w in &r.warnings {
        callout(WARN, g.warn, w, &[]);
    }
    println!();
}

// --- refresh --------------------------------------------------------------

/// What each decision means, in the words a reader would use.
fn decision_label(d: Decision, done: bool) -> (&'static str, anstyle::Style) {
    match d {
        // The four that describe an action read differently before it
        // happens: a preview that says "refreshed" is a report of something
        // that did not occur.
        Decision::MirrorActive => (if done { "mirrored" } else { "would mirror" }, OK),
        Decision::Refresh => (if done { "refreshed" } else { "would refresh" }, OK),
        Decision::SkipFresh => ("up to date", MUTED),
        Decision::SkipAccessLive => ("not due yet", MUTED),
        Decision::SkipBackoff => ("backing off", WARN),
        Decision::SkipCap => ("rate capped", WARN),
        Decision::ExpiringSoon => ("expiring", WARN),
        Decision::NeedsLogin => ("needs login", ERR),
        Decision::Broken => ("broken", ERR),
        Decision::Blocked => ("blocked", ERR),
    }
}

pub fn refresh(theme: &Theme, r: &RefreshReport) {
    refresh_report(theme, r, true);
}

/// The same table for a preview, in the tense of something that has not
/// happened yet.
pub fn refresh_preview(theme: &Theme, r: &RefreshReport) {
    refresh_report(theme, r, false);
    callout(
        MUTED,
        theme.glyphs.bullet,
        "dry run -- nothing was spawned and nothing was written",
        &[],
    );
    println!();
}

fn refresh_report(theme: &Theme, r: &RefreshReport, done: bool) {
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
        let (label, style) = decision_label(p.decision, done);
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

    let count = |d: Decision| r.profiles.iter().filter(|p| p.decision == d).count();
    // Separately: "log in again" is the wrong advice for a profile that is
    // broken for some other reason, such as a mirror refused because another
    // account is logged in.
    let logins = count(Decision::NeedsLogin);
    let broken = count(Decision::Broken) + count(Decision::Blocked);
    // A mirror that found nothing new wrote nothing, so it is not an update.
    let moved = r
        .profiles
        .iter()
        .filter(|p| match p.decision {
            Decision::Refresh => true,
            Decision::MirrorActive => p.detail.as_deref() != Some("unchanged"),
            _ => false,
        })
        .count();
    let mut summary = paint(
        MUTED,
        &format!(
            "{}, {moved} {}",
            plural(r.profiles.len(), "profile", "profiles"),
            if done { "updated" } else { "to update" }
        ),
    );
    if logins > 0 {
        summary.push_str(&format!(
            "  {}",
            paint(ERR, &plural(logins, "needs a login", "need a login"))
        ));
    }
    if broken > 0 {
        summary.push_str(&format!(
            "  {}",
            paint(ERR, &plural(broken, "needs attention", "need attention"))
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
            if let Some(c) = &h.command {
                f.add(
                    "Runs",
                    if c.exists() {
                        paint(MUTED, &c.display().to_string())
                    } else {
                        paint(ERR, &format!("{} (missing)", c.display()))
                    },
                );
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

// --- log ------------------------------------------------------------------

pub fn log(theme: &Theme, entries: &[crate::logbook::Entry], path: &std::path::Path) {
    let g = theme.glyphs;
    println!();
    if entries.is_empty() {
        callout(
            MUTED,
            g.bullet,
            "no runs recorded yet",
            &[&format!("the log lives at {}", path.display())],
        );
        println!();
        return;
    }

    let now = crate::store::now_ms();
    let mut t = Table::new(&[
        ("WHEN", Align::Left),
        ("FROM", Align::Left),
        ("STATUS", Align::Left),
        ("PROFILES", Align::Left),
    ]);
    for e in entries {
        let summary = if e.profiles.is_empty() {
            "-".to_string()
        } else {
            e.profiles
                .iter()
                .map(|p| format!("{} {}", p.name, p.decision.replace('_', " ")))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let style = if e.status.starts_with("skipped") {
            MUTED
        } else {
            VALUE
        };
        // Records written before the field existed say nothing rather than
        // guessing, which is why this is three-valued.
        let from = match e.scheduled {
            Some(true) => "timer",
            Some(false) => "you",
            None => "-",
        };
        t.row(vec![
            Cell::new(ago(e.at_ms, now), MUTED),
            Cell::new(from, MUTED),
            Cell::new(&e.status, style),
            Cell::new(summary, VALUE),
        ]);
    }
    println!("{}", t.render(theme, PAD));
    println!();
    println!("{PAD}{}", paint(MUTED, &path.display().to_string()));
    println!();
}

// --- uninstall ------------------------------------------------------------

fn owner_line(owner: &crate::ops::uninstall::Owner) -> (anstyle::Style, String) {
    use crate::ops::uninstall::Owner;
    match owner {
        Owner::Installer => (OK, "will be removed (release installer)".into()),
        Owner::Unmanaged => (OK, "will be removed".into()),
        Owner::PackageManager { name, command } => {
            (WARN, format!("left for {name} -- run `{command}`"))
        }
    }
}

/// What `uninstall` is about to do. Shown by `--dry-run`, and above the
/// question when the plan would delete stored profiles.
pub fn uninstall_plan(theme: &Theme, p: &crate::ops::uninstall::Plan, dry_run: bool) {
    let g = theme.glyphs;
    println!();
    println!("{}", heading(theme, PAD, "Uninstall"));
    println!();

    let mut f = Fields::new();
    let (style, binary) = owner_line(&p.owner);
    f.add(
        "Binary",
        format!(
            "{}   {}",
            paint(style, &binary),
            paint(MUTED, &p.exe.display().to_string())
        ),
    );
    let schedule = match (&p.schedule, &p.schedule_for) {
        (true, _) => paint(OK, "will be removed"),
        (false, Some(other)) => format!(
            "{}   {}",
            paint(WARN, "left alone, it starts another copy"),
            paint(MUTED, &other.display().to_string())
        ),
        (false, None) => paint(MUTED, "none registered"),
    };
    f.add("Schedule", schedule);
    if let Some(r) = &p.receipt {
        f.add(
            "Receipt",
            format!(
                "{}   {}",
                paint(OK, "will be removed"),
                paint(MUTED, &r.display().to_string())
            ),
        );
    }
    let data = p.data_dir.display().to_string();
    let profiles = if p.profiles.is_empty() {
        "no profiles".to_string()
    } else {
        p.profiles.join(", ")
    };
    let row = if !p.data_exists {
        paint(MUTED, "none stored")
    } else if p.deletes_data() {
        format!(
            "{}   {}",
            paint(ERR, &format!("will be DELETED: {profiles}")),
            paint(MUTED, &data)
        )
    } else if let (true, Some(why)) = (p.purge, &p.purge_refused) {
        format!(
            "{}   {}",
            paint(WARN, "kept, purge refused"),
            paint(MUTED, &format!("{data} -- {why}"))
        )
    } else {
        format!(
            "{}   {}",
            paint(OK, &format!("kept: {profiles}")),
            paint(MUTED, &format!("{data} -- --purge deletes them"))
        )
    };
    f.add("Profiles", row);
    // Its own row. The confirmation counted these and the JSON reported
    // them, while the plan a person reads before answering did not -- and a
    // parked Desktop login is the one thing here that no copy can replace:
    // its token is encrypted, so nothing was ever kept aside.
    if p.desktop_logins > 0 {
        let logins = plural(p.desktop_logins, "login", "logins");
        f.add(
            "Desktop",
            if p.deletes_data() {
                paint(ERR, &format!("will be DELETED: {logins}, parked here"))
            } else {
                paint(
                    OK,
                    &format!("kept: {logins}, parked here -- --purge deletes them"),
                )
            },
        );
    }
    f.add(
        "Claude Code",
        paint(MUTED, "not touched -- the login in ~/.claude stays"),
    );
    println!("{}", f.render(PAD));
    println!();
    if dry_run {
        println!(
            "{PAD}{} {}",
            paint(MUTED, g.bullet),
            paint(MUTED, "dry run -- nothing was removed")
        );
        println!();
    }
}

pub fn uninstall_confirm(theme: &Theme, p: &crate::ops::uninstall::Plan) {
    let mut what = Vec::new();
    if !p.profiles.is_empty() {
        what.push(plural(
            p.profiles.len(),
            "stored profile",
            "stored profiles",
        ));
    }
    if p.backups > 0 {
        what.push(plural(p.backups, "backup", "backups"));
    }
    if p.desktop_logins > 0 {
        what.push(plural(
            p.desktop_logins,
            "Claude Desktop profile",
            "Claude Desktop profiles",
        ));
    }
    if p.unreadable {
        what.push("whatever it could not list".to_string());
    }
    println!(
        "{PAD}{} {}",
        paint(ERR, theme.glyphs.err),
        paint(
            HEAD,
            &format!(
                "this deletes {}, and it cannot be undone",
                what.join(" and ")
            )
        )
    );
    println!(
        "{PAD}  {}",
        paint(
            MUTED,
            "they are the only copies of accounts that are not logged in right now"
        )
    );
    // No trailing newline: the answer is typed on this line.
    anstream::print!("{PAD}  type yes to continue: ");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

pub fn uninstall_cancelled(theme: &Theme) {
    println!();
    println!(
        "{PAD}{} {}",
        paint(MUTED, theme.glyphs.bullet),
        paint(VALUE, "cancelled -- nothing was removed")
    );
    println!();
}

pub fn uninstalled(
    theme: &Theme,
    p: &crate::ops::uninstall::Plan,
    o: &crate::ops::uninstall::Outcome,
) {
    use crate::ops::uninstall::Owner;
    let g = theme.glyphs;
    println!();
    if o.schedule_removed {
        println!("{PAD}{} refresh schedule removed", paint(OK, g.ok));
    }
    if o.data_removed {
        println!(
            "{PAD}{} deleted {}",
            paint(OK, g.ok),
            paint(MUTED, &p.data_dir.display().to_string())
        );
    }
    if o.receipt_removed {
        println!("{PAD}{} installer receipt removed", paint(OK, g.ok));
    }
    match &p.owner {
        Owner::PackageManager { name, command } => println!(
            "{PAD}{} the binary belongs to {name}; remove it with {}",
            paint(WARN, g.warn),
            paint(NAME, command)
        ),
        _ if o.binary_removed => println!(
            "{PAD}{} ccred removed {}",
            paint(OK, g.ok),
            paint(
                MUTED,
                if cfg!(windows) {
                    "(the file goes as soon as this command exits)"
                } else {
                    ""
                }
            )
        ),
        _ => {}
    }
    if !p.purge && p.data_exists {
        println!();
        println!(
            "{PAD}{} {}",
            paint(MUTED, g.bullet),
            paint(
                MUTED,
                &format!(
                    concat!(
                        "your profiles are kept in {} -- delete that folder ",
                        "yourself once you no longer need them"
                    ),
                    p.data_dir.display()
                )
            )
        );
    }
    for problem in &o.problems {
        println!("{PAD}{} {}", paint(ERR, g.err), paint(VALUE, problem));
    }
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
        E::Json { .. } => Some(
            "`ccred doctor` names the file; `ccred restore <name>` puts back the last good copy",
        ),
        E::Encoding { .. } => Some(concat!(
            "PowerShell writes UTF-16 whenever it redirects output, so a file copied ",
            "with `Get-Content | Out-File` arrives like this; re-save it as UTF-8"
        )),
        E::RefusedSymlink(_) => Some(concat!(
            "a write through a symlink lands on whatever it points at, which for a ",
            "0600 credential file is a hole; replace the link with a real file"
        )),
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
        assert_eq!(plan_label("claude_pro"), "Pro");
        assert_eq!(plan_label("claude_education"), "education");
        assert_ne!(
            plan_label("default_claude_ai"),
            "ai",
            "the label a user saw"
        );
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
            // Both tenses: a preview says what would happen, a run what did.
            for done in [true, false] {
                let (label, _) = decision_label(d, done);
                assert!(!label.is_empty(), "{d:?} has no label");
                assert!(
                    label.chars().all(|c| c.is_ascii_lowercase() || c == ' '),
                    "{d:?} produced {label:?}, which looks like a Debug spelling"
                );
            }
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
