//! Terminal presentation: palette, glyphs and layout primitives.
//!
//! Rendering lives apart from the operations, which return data and never
//! printed text. Two rules hold everywhere below this line:
//!
//! - **Nothing here may print a secret.** This module never receives a
//!   `Secret` and must never call `expose()`; CI enforces the second half.
//! - **Width is measured on unstyled text.** Colour is applied after a column
//!   width is computed, never before, or every table drifts by the length of
//!   an escape sequence.

pub mod render;

use anstyle::{AnsiColor, Color, Style};

// --- palette --------------------------------------------------------------
//
// Six roles, not six colours. Anything that seems to need a new colour almost
// always needs one of these instead.

/// Field names, units, and anything the eye should skip on a second read.
pub const LABEL: Style = Style::new().dimmed();
/// The answer to a label. Plain, so it wins by contrast rather than colour.
pub const VALUE: Style = Style::new();
/// Profile names and other identifiers the user types back at us.
pub const NAME: Style = fg(AnsiColor::Cyan).bold();
/// Column headings and section titles.
pub const HEAD: Style = Style::new().bold();
/// Healthy, done, nothing to do.
pub const OK: Style = fg(AnsiColor::Green);
/// Worth knowing, not worth stopping for.
pub const WARN: Style = fg(AnsiColor::Yellow);
/// Broken, refused, or needs a person.
pub const ERR: Style = fg(AnsiColor::Red);
/// The active profile, and only that.
pub const ACCENT: Style = fg(AnsiColor::Magenta).bold();
/// Hints, paths, and the parts of a line that are context rather than content.
pub const MUTED: Style = Style::new().dimmed();

const fn fg(c: AnsiColor) -> Style {
    Style::new().fg_color(Some(Color::Ansi(c)))
}

/// Wrap text in a style. `anstream` removes these sequences again when the
/// stream is not a terminal, so callers never branch on colour support.
pub fn paint(style: Style, text: &str) -> String {
    format!("{}{}{}", style.render(), text, style.render_reset())
}

// --- glyphs ---------------------------------------------------------------

/// The drawing characters, in the two sets this tool can emit.
///
/// Source stays ASCII (see CLAUDE.md); the Unicode set is written as escapes.
#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    pub active: &'static str,
    pub bullet: &'static str,
    pub ok: &'static str,
    pub warn: &'static str,
    pub err: &'static str,
    pub arrow: &'static str,
    pub rule: &'static str,
    pub bar_full: &'static str,
    pub bar_empty: &'static str,
    pub corner: &'static str,
}

const UNICODE: Glyphs = Glyphs {
    active: "\u{25cf}",    // black circle
    bullet: "\u{00b7}",    // middle dot
    ok: "\u{2713}",        // check mark
    warn: "\u{25b3}",      // white up-pointing triangle
    err: "\u{2717}",       // ballot x
    arrow: "\u{2192}",     // rightwards arrow
    rule: "\u{2500}",      // box drawings light horizontal
    bar_full: "\u{2588}",  // full block
    bar_empty: "\u{2591}", // light shade
    corner: "\u{2514}",    // box drawings light up and right
};

const ASCII: Glyphs = Glyphs {
    active: "*",
    bullet: "-",
    ok: "+",
    warn: "!",
    err: "x",
    arrow: "->",
    rule: "-",
    bar_full: "#",
    bar_empty: ".",
    corner: "`",
};

/// What the terminal can draw.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub glyphs: Glyphs,
}

impl Theme {
    /// Decide once, from the environment.
    ///
    /// The Unicode set is opt-in rather than opt-out. A wrong guess here is
    /// not cosmetic: a legacy Windows console renders the block characters as
    /// mojibake, and this tool's whole job is to stay readable when something
    /// has already gone wrong.
    pub fn detect() -> Self {
        Theme {
            glyphs: if unicode_ok() { UNICODE } else { ASCII },
        }
    }

    pub fn ascii() -> Self {
        Theme { glyphs: ASCII }
    }
}

fn unicode_ok() -> bool {
    // An explicit answer always wins, and is how a user fixes a bad guess.
    match std::env::var("CCRED_UNICODE").as_deref() {
        Ok("1") | Ok("true") | Ok("yes") => return true,
        Ok("0") | Ok("false") | Ok("no") => return false,
        _ => {}
    }

    if cfg!(windows) {
        // Windows Terminal sets WT_SESSION and is UTF-8 clean. The legacy
        // console host sets neither and is not, so it gets ASCII.
        std::env::var_os("WT_SESSION").is_some()
    } else {
        let locale = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_CTYPE"))
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        locale.contains("utf-8") || locale.contains("utf8")
    }
}

// --- primitives -----------------------------------------------------------

/// Character count of `s`, used for column alignment.
///
/// Every value this tool aligns is an ASCII identifier, an email address, a
/// number, or one of the glyphs above, all of which occupy one cell. That
/// makes `chars().count()` correct here without a width table.
pub fn width(s: &str) -> usize {
    s.chars().count()
}

/// A section title with a rule under it. Both lines carry the indent, or the
/// rule hangs off the left margin.
pub fn heading(theme: &Theme, indent: &str, title: &str) -> String {
    let rule = theme.glyphs.rule.repeat(width(title));
    format!(
        "{indent}{}\n{indent}{}",
        paint(HEAD, title),
        paint(MUTED, &rule)
    )
}

/// An aligned label/value block. Labels are padded to the widest one.
#[derive(Default)]
pub struct Fields {
    rows: Vec<(String, String)>,
}

impl Fields {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a row whose value is already styled.
    pub fn add(&mut self, label: &str, value: impl Into<String>) -> &mut Self {
        self.rows.push((label.to_string(), value.into()));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn render(&self, indent: &str) -> String {
        let w = self.rows.iter().map(|(l, _)| width(l)).max().unwrap_or(0);
        self.rows
            .iter()
            .map(|(l, v)| {
                let pad = " ".repeat(w - width(l));
                format!("{indent}{}{pad}   {v}", paint(LABEL, l))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// One table cell: the text to measure, and the style to paint it with.
pub struct Cell {
    text: String,
    style: Style,
}

impl Cell {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Cell {
            text: text.into(),
            style,
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Cell::new(text, VALUE)
    }
}

/// A column-aligned table. Trailing blanks are trimmed, so nothing pads the
/// end of a line -- which matters when output is pasted into an issue.
pub struct Table {
    headers: Vec<(String, Align)>,
    rows: Vec<Row>,
}

enum Row {
    Cells(Vec<Cell>),
    /// A continuation line under the previous row, hung off the first column.
    /// Used for notes that would otherwise need a column of their own.
    Note(String),
}

impl Table {
    pub fn new(headers: &[(&str, Align)]) -> Self {
        Table {
            headers: headers.iter().map(|(h, a)| (h.to_string(), *a)).collect(),
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, cells: Vec<Cell>) -> &mut Self {
        debug_assert_eq!(cells.len(), self.headers.len(), "row/header width mismatch");
        self.rows.push(Row::Cells(cells));
        self
    }

    pub fn note(&mut self, text: impl Into<String>) -> &mut Self {
        self.rows.push(Row::Note(text.into()));
        self
    }

    pub fn render(&self, theme: &Theme, indent: &str) -> String {
        let mut widths: Vec<usize> = self.headers.iter().map(|(h, _)| width(h)).collect();
        for row in &self.rows {
            if let Row::Cells(cells) = row {
                for (i, c) in cells.iter().enumerate() {
                    widths[i] = widths[i].max(width(&c.text));
                }
            }
        }

        let mut out = Vec::new();

        // Headers pad outside the styled run for the same reason rows do, and
        // so that a trailing short column leaves blanks `trim_end` can reach.
        let head: Vec<String> = self
            .headers
            .iter()
            .enumerate()
            .map(|(i, (h, a))| {
                let extra = " ".repeat(widths[i].saturating_sub(width(h)));
                match a {
                    Align::Left => format!("{}{extra}", paint(LABEL, h)),
                    Align::Right => format!("{extra}{}", paint(LABEL, h)),
                }
            })
            .collect();
        out.push(trim_line(indent, &head.join("  ")));

        for row in &self.rows {
            match row {
                Row::Cells(cells) => {
                    let line: Vec<String> = cells
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            // Pad outside the styled run, never inside it, so
                            // a background colour cannot bleed into the gutter.
                            let extra = " ".repeat(widths[i].saturating_sub(width(&c.text)));
                            match self.headers[i].1 {
                                Align::Left => format!("{}{extra}", paint(c.style, &c.text)),
                                Align::Right => format!("{extra}{}", paint(c.style, &c.text)),
                            }
                        })
                        .collect();
                    out.push(trim_line(indent, &line.join("  ")));
                }
                Row::Note(text) => {
                    out.push(format!(
                        "{indent}{} {}",
                        paint(MUTED, theme.glyphs.corner),
                        paint(MUTED, text)
                    ));
                }
            }
        }
        out.join("\n")
    }
}

/// Join an indent to a line and drop trailing blanks, which the padding of a
/// short final column would otherwise leave behind.
fn trim_line(indent: &str, line: &str) -> String {
    format!("{indent}{}", line.trim_end())
}

// --- meters ---------------------------------------------------------------

/// Nominal length of a full refresh window, in days.
///
/// Nothing tells us how long the window was when it was issued, so the meter
/// is drawn against a fixed scale rather than a true percentage. It exists to
/// make "running out" obvious at a glance; the number beside it is the fact.
pub const NOMINAL_WINDOW_DAYS: i64 = 30;

/// A meter `cells` wide, filled by `left / full`.
pub fn meter(theme: &Theme, left: i64, full: i64, cells: usize) -> String {
    let frac = if full <= 0 {
        0.0
    } else {
        (left as f64 / full as f64).clamp(0.0, 1.0)
    };
    let filled = (frac * cells as f64).round() as usize;
    format!(
        "{}{}",
        theme.glyphs.bar_full.repeat(filled),
        theme.glyphs.bar_empty.repeat(cells - filled)
    )
}

/// Green with room to spare, yellow when it is time to think about it, red
/// when it is too late. The thresholds match the ones `doctor` warns on.
pub fn days_style(days: i64) -> Style {
    match days {
        d if d < 0 => ERR,
        d if d <= 5 => WARN,
        _ => OK,
    }
}

// --- humanising -----------------------------------------------------------

/// "3 days ago", "just now". Coarse on purpose: the exact minute never
/// changes what the reader does next.
pub fn ago(then_ms: i64, now_ms: i64) -> String {
    let secs = (now_ms - then_ms) / 1000;
    match secs {
        s if s < 0 => "in the future".to_string(),
        s if s < 90 => "just now".to_string(),
        s if s < 5400 => format!("{} min ago", s / 60),
        s if s < 172_800 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

/// "22 days", "8 hours", "expired". Used for anything counting down.
pub fn left(ms: i64) -> String {
    if ms <= 0 {
        return "expired".to_string();
    }
    let mins = ms / 60_000;
    match mins {
        m if m < 90 => format!("{m} min"),
        m if m < 2880 => format!("{} hours", m / 60),
        m => format!("{} days", m / 1440),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip every SGR sequence, so a test can assert on what is actually
    /// seen rather than on the bytes written.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn table_columns_line_up_regardless_of_styling() {
        let theme = Theme::ascii();
        let mut t = Table::new(&[("NAME", Align::Left), ("N", Align::Right)]);
        t.row(vec![Cell::new("a", NAME), Cell::new("1", OK)]);
        t.row(vec![Cell::new("longer", VALUE), Cell::new("22", ERR)]);

        let plain: Vec<String> = t.render(&theme, "").lines().map(strip_ansi).collect();
        assert_eq!(
            plain[1].chars().count(),
            plain[2].chars().count(),
            "styled rows drifted: {plain:?}"
        );
        assert!(plain[1].starts_with("a     "), "got {:?}", plain[1]);
        assert!(plain[2].ends_with("22"), "got {:?}", plain[2]);
    }

    #[test]
    fn meter_is_clamped_at_both_ends() {
        let theme = Theme::ascii();
        assert_eq!(meter(&theme, -5, 30, 10), "..........");
        assert_eq!(meter(&theme, 99, 30, 10), "##########");
        assert_eq!(meter(&theme, 15, 30, 10), "#####.....");
    }

    #[test]
    fn ascii_theme_emits_no_multibyte_characters() {
        let g = Theme::ascii().glyphs;
        for s in [
            g.active,
            g.bullet,
            g.ok,
            g.warn,
            g.err,
            g.arrow,
            g.rule,
            g.bar_full,
            g.bar_empty,
            g.corner,
        ] {
            assert!(s.is_ascii(), "{s:?} is not ASCII");
        }
    }

    #[test]
    fn countdowns_round_to_the_unit_a_reader_acts_on() {
        assert_eq!(left(0), "expired");
        assert_eq!(left(-1), "expired");
        assert_eq!(left(45 * 60_000), "45 min");
        assert_eq!(left(8 * 3_600_000), "8 hours");
        assert_eq!(left(22 * 86_400_000), "22 days");
    }

    #[test]
    fn no_line_ends_in_padding() {
        let theme = Theme::ascii();
        let mut t = Table::new(&[("A", Align::Left), ("LONGHEADER", Align::Left)]);
        t.row(vec![Cell::plain("x"), Cell::plain("y")]);
        for line in t.render(&theme, "  ").lines() {
            let plain = strip_ansi(line);
            assert_eq!(plain.trim_end(), plain, "trailing blanks in {plain:?}");
        }
    }
}
