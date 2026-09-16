//! Entry point: parse, dispatch, render, and map failures onto exit codes.
//!
//! Formatting lives in `ccred::ui::render`, which is the one place to audit
//! for the rule that no output may ever contain a token. This file decides
//! *what* to show and what to exit with; it does not decide how it looks.

use anstream::println;
use clap::Parser;

use ccred::cli::{Cli, Command, ScheduleAction};
use ccred::error::ExitCode;
use ccred::ops::{Ctx, doctor, refresh, schedule as sched_ops, simple, switch, uninstall};
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
    let ctx = Ctx::resolve(ccred::paths::Locations {
        ccred_home: cli.ccred_home.clone(),
        claude_config_dir: cli.claude_config_dir.clone(),
    })?;

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
                // The shape stays an array of profiles: a script that reads
                // `ccred list --json` predates the note below, and `current
                // --json` already reports a pointer that matches nothing.
                print_json(&rows);
            } else {
                render::list(theme, &rows, simple::pointer_note(&ctx).as_ref());
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

        Some(Command::Rm { name, purge }) => {
            let name = validate_profile_name(name)?;
            let report = simple::remove(&ctx, &name, *purge)?;
            if cli.json {
                print_json(&report);
            } else {
                render::removed(theme, &report);
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::Restore { name }) => {
            let name = validate_profile_name(name)?;
            let report = simple::restore(&ctx, &name)?;
            if cli.json {
                print_json(&report);
            } else {
                render::restored(theme, &report.name);
            }
            Ok(ExitCode::Ok)
        }

        Some(Command::Refresh {
            if_older_than,
            force,
            all: _,
            claude_path,
            dry_run,
        }) => {
            let opts = refresh::RefreshOptions {
                if_older_than_ms: if_older_than.map(|h| i64::from(h) * 3_600_000),
                claude_path: claude_path.clone(),
                force: *force,
                ..Default::default()
            };
            if *dry_run {
                let report = refresh::preview(&ctx, &opts)?;
                if cli.json {
                    print_json(&report);
                } else {
                    render::refresh_preview(theme, &report);
                }
                // A preview is a report, not a verdict: it must not turn a
                // scheduler red for something it only looked at.
                return Ok(ExitCode::Ok);
            }
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
                ScheduleAction::Install {
                    dry_run,
                    claude_path,
                } => {
                    // Refused here rather than twice a week in a log nobody
                    // reads: the job cannot ask a person for a better path.
                    if let Some(p) = claude_path
                        && !p.is_file()
                    {
                        return Err(ccred::CcredError::ClaudeMissing(format!(
                            "the path given with --claude-path does not exist: {}",
                            p.display()
                        )));
                    }
                    let spec = sched_ops::spec_for(&ctx)?.with_claude_path(claude_path.as_deref());
                    if *dry_run {
                        let files = backend.render(&spec)?;
                        if cli.json {
                            print_json(&files);
                        } else {
                            render::dry_run(theme, &files);
                        }
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
                    ccred::schedule::uninstall_checked(backend.as_ref())?;
                    if cli.json {
                        // The state afterwards, in the shape `schedule status`
                        // reports, so a script reads one thing either way.
                        print_json(&ccred::schedule::State::NotInstalled);
                    } else {
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

        Some(Command::Uninstall {
            purge,
            yes,
            dry_run,
        }) => {
            let plan = uninstall::plan(&ctx, *purge)?;
            if *dry_run {
                if cli.json {
                    print_json(&plan);
                } else {
                    render::uninstall_plan(theme, &plan, true);
                }
                return Ok(ExitCode::Ok);
            }

            // JSON output has no room for a prompt, so it counts as unattended.
            let can_ask = {
                use std::io::IsTerminal;
                !cli.json && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
            };
            match uninstall::consent(&plan, *yes, can_ask) {
                uninstall::Consent::Proceed => {}
                uninstall::Consent::Blocked(why) => {
                    return Err(ccred::CcredError::UnsafeWrite(format!(
                        "{why}; nothing was removed"
                    )));
                }
                uninstall::Consent::Refuse => {
                    return Err(ccred::CcredError::UnsafeWrite(
                        concat!(
                            "`--purge` would delete stored credentials; pass --yes ",
                            "to confirm when running without a terminal"
                        )
                        .into(),
                    ));
                }
                uninstall::Consent::Ask => {
                    render::uninstall_plan(theme, &plan, false);
                    render::uninstall_confirm(theme, &plan);
                    let mut answer = String::new();
                    let _ = std::io::stdin().read_line(&mut answer);
                    if answer.trim() != "yes" {
                        render::uninstall_cancelled(theme);
                        return Ok(ExitCode::Usage);
                    }
                }
            }

            let outcome = uninstall::execute(&plan);
            if cli.json {
                print_json(&outcome);
            } else {
                render::uninstalled(theme, &plan, &outcome);
            }
            Ok(if outcome.problems.is_empty() {
                ExitCode::Ok
            } else {
                ExitCode::Internal
            })
        }

        Some(Command::Log { count }) => {
            let entries = ccred::logbook::tail(&ctx.paths().log_dir(), *count);
            if cli.json {
                print_json(&entries);
            } else {
                render::log(
                    theme,
                    &entries,
                    &ccred::logbook::log_path(&ctx.paths().log_dir()),
                );
            }
            Ok(ExitCode::Ok)
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
