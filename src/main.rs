//! Entry point: parse, dispatch, render, and map failures onto exit codes.
//!
//! Formatting lives in `ccred::ui::render`, which is the one place to audit
//! for the rule that no output may ever contain a token. This file decides
//! *what* to show and what to exit with; it does not decide how it looks.

use anstream::println;
use clap::Parser;

use ccred::cli::{Cli, Command, ScheduleAction};
use ccred::error::ExitCode;
use ccred::ops::{Ctx, doctor, refresh, schedule as sched_ops, simple, switch};
use ccred::ui::{Theme, render};
use ccred::validate::validate_profile_name;

fn main() {
    let cli = Cli::parse();
    let theme = Theme::detect();
    let code = match run(&cli, &theme) {
        Ok(code) => code,
        Err(e) => {
            render::error(&theme, &e);
            e.exit_code()
        }
    };
    std::process::exit(code as i32);
}

fn run(cli: &Cli, theme: &Theme) -> ccred::Result<ExitCode> {
    let ctx = Ctx::from_env()?;

    match cli.command.as_ref() {
        None | Some(Command::Current) => {
            let report = simple::current(&ctx)?;
            if cli.json {
                print_json(&report);
            } else {
                render::current(theme, &report);
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::List) => {
            let rows = simple::list(&ctx)?;
            if cli.json {
                print_json(&rows);
            } else {
                render::list(theme, &rows);
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::Save { name }) => {
            let name = validate_profile_name(name)?;
            let report = simple::save(&ctx, &name)?;
            if cli.json {
                print_json(&report);
            } else {
                render::save(theme, &report);
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::Switch { name, force }) => {
            let name = validate_profile_name(name)?;
            let report = switch::switch(&ctx, &name, *force)?;
            if cli.json {
                print_json(&report);
            } else {
                render::switch(theme, &report);
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::Rm { name }) => {
            let name = validate_profile_name(name)?;
            simple::remove(&ctx, &name)?;
            if !cli.json {
                render::removed(theme, name.as_str());
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::Refresh {
            if_older_than,
            all: _,
            claude_path,
        }) => {
            let opts = refresh::RefreshOptions {
                if_older_than_ms: if_older_than.map(|h| i64::from(h) * 3_600_000),
                claude_path: claude_path.clone(),
                ..Default::default()
            };
            let report = refresh::refresh(&ctx, &opts)?;
            if cli.json {
                print_json(&report);
            } else {
                render::refresh(theme, &report);
            }
            // A scheduler must be able to tell "nothing to do" from "a person
            // is needed". Transient trouble stays at 0 on purpose, so a lost
            // network connection does not paint the unit red.
            Ok(if report.needs_attention() {
                ExitCode::NeedsLogin
            } else {
                ExitCode::Ok
            })
        }

        Some(Command::Schedule(args)) => {
            let backend = sched_ops::backend();
            let kind = backend.backend();
            match &args.action {
                ScheduleAction::Install { dry_run } => {
                    let spec = sched_ops::spec_for(&ctx)?;
                    if *dry_run {
                        render::dry_run(theme, &backend.render(&spec)?);
                        return Ok(ExitCode::Ok);
                    }
                    let health = ccred::schedule::install_checked(backend.as_ref(), &spec)?;
                    if cli.json {
                        print_json(&health);
                    } else {
                        render::schedule_installed(theme, &health, kind);
                    }
                    Ok(ExitCode::Ok)
                }
                ScheduleAction::Uninstall => {
                    backend.uninstall()?;
                    if !cli.json {
                        render::schedule_removed(theme);
                    }
                    Ok(ExitCode::Ok)
                }
                ScheduleAction::Status => {
                    let state = backend.status()?;
                    if cli.json {
                        print_json(&state);
                    } else {
                        render::schedule_status(theme, &state, kind);
                    }
                    Ok(ExitCode::Ok)
                }
            }
        }

        Some(Command::Doctor) => {
            let findings = doctor::doctor(&ctx)?;
            if cli.json {
                print_json(&findings);
            } else {
                render::doctor(theme, &findings);
            }
            Ok(match doctor::worst(&findings) {
                doctor::Severity::Error => ExitCode::Unsafe,
                _ => ExitCode::Ok,
            })
        }

        Some(Command::Unknown(args)) => {
            let guess = args.first().map(String::as_str).unwrap_or("<name>");
            render::unknown_command(theme, guess);
            Ok(ExitCode::Usage)
        }
    }
}

fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(e) => anstream::eprintln!("ccred: could not render JSON: {e}"),
    }
}
