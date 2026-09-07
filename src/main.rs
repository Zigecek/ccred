//! Entry point: parse, dispatch, print, and map failures onto exit codes.
//!
//! All formatting lives here so there is exactly one place to audit for the
//! rule that no output may ever contain a token.

use clap::Parser;

use ccred::cli::{Cli, Command};
use ccred::error::ExitCode;
use ccred::ops::{Ctx, doctor, simple, switch};
use ccred::validate::validate_profile_name;

fn main() {
    let cli = Cli::parse();
    let code = match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ccred: {e}");
            // Show the underlying cause too -- "I/O error at <path>" alone is
            // rarely enough to act on.
            let mut source = std::error::Error::source(&e);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            e.exit_code()
        }
    };
    std::process::exit(code as i32);
}

fn run(cli: &Cli) -> ccred::Result<ExitCode> {
    let ctx = Ctx::from_env()?;

    match cli.command.as_ref() {
        None | Some(Command::Current) => {
            let report = simple::current(&ctx)?;
            if cli.json {
                print_json(&report);
            } else {
                print_current(&report);
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::List) => {
            let rows = simple::list(&ctx)?;
            if cli.json {
                print_json(&rows);
            } else {
                print_list(&rows);
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::Save { name }) => {
            let name = validate_profile_name(name)?;
            let report = simple::save(&ctx, &name)?;
            if cli.json {
                print_json(&report);
            } else {
                println!(
                    "saved profile '{}' ({}) -- {}",
                    report.name, report.account, report.outcome
                );
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::Switch { name, force }) => {
            let name = validate_profile_name(name)?;
            let report = switch::switch(&ctx, &name, *force)?;
            if cli.json {
                print_json(&report);
            } else {
                print_switch(&report);
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::Rm { name }) => {
            let name = validate_profile_name(name)?;
            simple::remove(&ctx, &name)?;
            println!("removed profile '{name}'");
            Ok(ExitCode::Ok)
        }

        Some(Command::Doctor) => {
            let findings = doctor::doctor(&ctx)?;
            if cli.json {
                print_json(&findings);
            } else {
                for f in &findings {
                    let mark = match f.severity {
                        doctor::Severity::Ok => "ok  ",
                        doctor::Severity::Warn => "warn",
                        doctor::Severity::Error => "FAIL",
                    };
                    println!("{mark}  {}", f.title);
                    if let Some(detail) = &f.detail {
                        println!("        {detail}");
                    }
                }
            }
            Ok(match doctor::worst(&findings) {
                doctor::Severity::Error => ExitCode::Unsafe,
                _ => ExitCode::Ok,
            })
        }

        Some(Command::Unknown(args)) => {
            let guess = args.first().map(String::as_str).unwrap_or("<name>");
            eprintln!("ccred: unknown command '{guess}'");
            eprintln!("       did you mean:  ccred switch {guess}");
            eprintln!("       run `ccred --help` for the full list");
            Ok(ExitCode::Usage)
        }
    }
}

fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("ccred: could not render JSON: {e}"),
    }
}

fn print_current(r: &simple::CurrentReport) {
    match &r.active_profile {
        Some(name) => println!("active profile : {name}"),
        None => println!("active profile : (none)"),
    }
    println!("account        : {}", r.account);
    if r.logged_in {
        if let Some(d) = r.access_days_left {
            println!("access token   : {d} days left");
        }
        if let Some(d) = r.refresh_days_left {
            println!("refresh token  : {d} days left");
        }
    } else {
        println!("status         : not logged in");
    }
    if !r.claude_running.is_empty() {
        println!(
            "note           : Claude Code is running (pid {:?})",
            r.claude_running
        );
    }
    if let Some(msg) = &r.pointer_mismatch {
        println!();
        println!("WARNING: {msg}");
        println!("         run `ccred doctor` for what to do about it");
    }
}

fn print_list(rows: &[simple::ProfileRow]) {
    if rows.is_empty() {
        println!("no profiles yet -- run `ccred save <name>` while logged in");
        return;
    }
    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    for r in rows {
        let mark = if r.active { "*" } else { " " };
        let days = match r.refresh_days_left {
            Some(d) => format!("{d}d left"),
            None => "-".to_string(),
        };
        let state = if r.healthy { "ok" } else { "BROKEN" };
        println!(
            "{mark} {:<width$}  {:<32}  {:>9}  {}",
            r.name, r.account, days, state
        );
        if let Some(note) = &r.note {
            println!("  {:width$}  {note}", "");
        }
    }
}

fn print_switch(r: &switch::SwitchReport) {
    if let Some(recovered) = &r.recovered {
        println!("note: {recovered}");
    }
    match &r.outgoing {
        switch::OutgoingSync::Synced(name) => {
            println!("saved the current credentials into '{name}' first")
        }
        switch::OutgoingSync::NothingActive => {}
        switch::OutgoingSync::Skipped { profile, reason } => {
            println!("WARNING: did not update '{profile}': {reason}");
        }
    }
    println!("switched to '{}' ({})", r.to, r.account);
    if !r.identity_restored {
        println!("note: account details were not restored; Claude Code will refetch them");
    }
    for w in &r.warnings {
        println!("warning: {w}");
    }
    if !r.claude_running.is_empty() {
        println!(
            "note: Claude Code is still running (pid {:?}); restart it to pick this up",
            r.claude_running
        );
    }
}
