# ccred

Save, list and switch between named sets of local Claude Code credentials.

**Status: early development.** The commands below work on Linux and Windows.
macOS is written but unverified -- no Mac was available -- and nothing is
published to any registry so far.

> **Unofficial and independent.** Not affiliated with, endorsed by, or sponsored
> by Anthropic PBC. See [TRADEMARKS.md](TRADEMARKS.md).

## What it is for

If you use more than one Claude Code account on the same machine, switching
between them means logging out and back in. `ccred` stores each account as a
named profile and swaps the active one, so switching is one command.

It also keeps idle profiles alive. An account you have not used for a couple of
weeks has an expired refresh token and needs a manual login; `ccred` refreshes
profiles on a schedule to prevent that.

## Usage

```
ccred current             who is logged in, and which profile is active
ccred list                saved profiles and how much refresh window each has
ccred save <name>         store the account that is logged in, under a name
ccred switch <name>       make a saved profile the active account
ccred rm <name>           delete a profile
ccred refresh             keep stored profiles from expiring
ccred schedule install    run that refresh automatically, twice a week
ccred schedule status     is it registered, and when does it next run
ccred doctor              check for anything quietly wrong
```

Add `--json` to any of them for machine-readable output.

A bare `ccred <name>` is deliberately not a switch alias -- a profile called
`list` would then be unreachable -- so it prints a hint instead of guessing.

`refresh` is safe to run more often than needed. It records when it last ran
and exits successfully without doing anything if that was recent, so a
scheduler that double-fires -- systemd catching up, launchd coalescing, a
Windows task with both a boot trigger and a schedule -- costs nothing. The
real rate limit lives in the command, not the schedule.

`ccred schedule install --dry-run` prints the exact unit file, plist or task
XML it would register, without writing anything.

Installing verifies its own work: right after registering, it asks the
scheduler when the job will next run, and if the answer is "never" it undoes
the installation and says so. Each platform has a way to accept a schedule
that silently never fires -- a systemd timer with `Persistent=` on a monotonic
trigger, a launchd `Weekday` out of range, a Windows task left with the
default refusal to start on battery -- and all of them look installed.

Switching refuses to run while Claude Code is open. A live session holds the
old account in memory and would write its next refreshed token into what is by
then a different profile's file. Quit it first, or pass `--force` knowing that.

## Design notes

**It never talks to Anthropic's OAuth endpoint.** Refreshing is done by running
the real `claude` binary against each profile's own credential store, so the
refresh happens through Claude Code's own code path, with its own lock and its
own identity. Re-implementing the token exchange would risk single-use refresh
token replay detection and, on a server, being classified as bot traffic.

**One account per machine.** A single account's refresh token rotates on every
refresh, so copying one account's credentials to two machines makes them log
each other out. Each machine should log in independently.

**It assumes the files are not ours.** Claude Code's config holds a lot of
unrelated state, so every write patches rather than regenerates, and a runtime
check refuses any rewrite that would drop a key we do not model.

## Development

```sh
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Inspect a real credentials file without printing any secret:

```sh
cargo run --example check_roundtrip -- ~/.claude/.credentials.json
```

Conventions live in [CLAUDE.md](CLAUDE.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
