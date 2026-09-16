//! Checks on the source text itself.
//!
//! These catch slips no compiler or linter flags, because the code they
//! produce is valid.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A string continuation that rustfmt joined into a run of spaces.
///
/// Writing `"some text \` and continuing on the next line is valid Rust, and
/// the backslash eats the newline and the indentation. But a continuation
/// written through a tool that doubled the backslash, or reformatted before
/// it was fixed, ends up as one literal with twenty spaces in the middle of a
/// sentence -- and users read it that way. It shipped four times before this
/// test existed, each time found in someone else's terminal.
///
/// The rule: inside a string literal, a word, then six or more spaces, then
/// another word. Real alignment inside strings uses `format!` padding, not
/// literal runs.
#[test]
fn no_string_literal_has_a_run_of_spaces_between_words() {
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );

    let mut offenders = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // Only the parts between quotes.
            for (i, segment) in line.split('"').enumerate() {
                if i % 2 == 0 {
                    continue;
                }
                if has_word_gap(segment) {
                    offenders.push(format!("{}:{}: {}", file.display(), n + 1, trimmed));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "string literals with a run of spaces mid-sentence -- use concat! \
         instead of a continuation:\n{}",
        offenders.join("\n")
    );
}

fn has_word_gap(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ' ' {
            let start = i;
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            let run = i - start;
            let before = start.checked_sub(1).map(|j| chars[j]);
            let after = chars.get(i).copied();
            let wordish = |c: Option<char>| {
                c.is_some_and(|c| c.is_alphanumeric() || matches!(c, ',' | ';' | '.' | ')' | '`'))
            };
            if run >= 6 && wordish(before) && wordish(after) {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

#[test]
fn the_gap_detector_finds_what_it_is_for_and_nothing_else() {
    assert!(has_word_gap("fixed at                      login"));
    assert!(has_word_gap("it and              run `claude`"));
    assert!(!has_word_gap("a normal sentence, with punctuation."));
    assert!(!has_word_gap("  leading indentation is fine"));
    assert!(!has_word_gap("trailing is fine      "));
    assert!(!has_word_gap("two  spaces between words"));
}
