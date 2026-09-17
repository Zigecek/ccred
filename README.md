# ccred

Save, list and switch between named sets of local Claude Code credentials.

**Status: early development, and exercised in anger.** Linux and Windows are
both in daily use. macOS is written but unverified -- no Mac was available, and
the Keychain backend is deliberately not wired up until it can be tested, so
that platform sees only the plaintext fallback.

> **Unofficial and independent.** Not affiliated with, endorsed by, or sponsored
> by Anthropic PBC. See [TRADEMARKS.md](TRADEMARKS.md).

## What it is for

If you use more than one Claude Code account on the same machine, switching
between them means logging out and back in. `ccred` stores each account as a
named profile and swaps the active one, so switching is one command.

It also keeps idle profiles exercised. An account you have not used for weeks
goes stale and needs a manual login; `ccred` runs Claude Code against each
stored profile on a schedule so its tokens keep rotating.

**What that does and does not buy you.** Measured against a live account: a
refresh rotates both tokens and renews the access token by eight hours, and
leaves `refreshTokenExpiresAt` where it was, give or take under a second: it
is recomputed from whole seconds on each exchange, and one measured exchange
moved it 809 ms *earlier*. That deadline is a ceiling fixed when you logged
in, and every rotated token inherits it -- so refreshing cannot postpone it,
and when it arrives only a new login will do. What refreshing keeps is the
token in active use, which is the part a schedule can actually influence.

## Install

### Windows: Scoop

```powershell
scoop bucket add ccred https://github.com/Zigecek/scoop-ccred
scoop install ccred
```

No admin rights, no execution policy, and no SmartScreen prompt -- Scoop
extracts an archive and shims the binary, and a shim is launched through
`CreateProcess` rather than `ShellExecuteEx`. Uninstalling removes the
scheduled refresh first, so no task is left pointing at a binary that is gone.

### Linux: the `.deb`

Every release carries one for `amd64` and `arm64`:

```sh
curl -fsSLO https://github.com/Zigecek/ccred/releases/download/v0.3.3/ccred_0.3.3_amd64.deb
sudo apt install ./ccred_0.3.3_amd64.deb
```

`apt install` runs as root, so it is worth checking what you are handing it.
The package carries a checksum and a provenance attestation of its own:

```sh
curl -fsSLO https://github.com/Zigecek/ccred/releases/download/v0.3.3/ccred_0.3.3_amd64.deb.sha256
sha256sum -c ccred_0.3.3_amd64.deb.sha256
gh attestation verify ccred_0.3.3_amd64.deb --repo Zigecek/ccred
```

It carries no maintainer scripts on purpose: a `postinst` must not install the
refresh schedule, because the package installs as root while the schedule
belongs to one user's session. Run `ccred schedule install` yourself.

### Anywhere: the release installer

crates.io, npm, Homebrew and the AUR are wired up but not yet live; they need
account setup that only a human can do, described in
[docs/publishing.md](docs/publishing.md). The reasoning behind which channels
are worth maintaining at all is in [docs/install.md](docs/install.md).

```sh
# Linux and macOS
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Zigecek/ccred/releases/download/v0.3.3/ccred-installer.sh | sh
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -c "irm https://github.com/Zigecek/ccred/releases/download/v0.3.3/ccred-installer.ps1 | iex"
```

`-ExecutionPolicy Bypass` is not bypassing a protection. Execution policy does
not apply to a piped `iex` in the first place; the flag is there because
cargo-dist's script checks its own policy and refuses to continue without it.
That, and why Defender intermittently flags this command line, is written up in
[docs/install.md](docs/install.md).

To read every byte before running anything:

```powershell
$v = "0.3.3"
$z = "ccred-x86_64-pc-windows-msvc.zip"
irm "https://github.com/Zigecek/ccred/releases/download/v$v/$z" -OutFile $z
(Get-FileHash $z -Algorithm SHA256).Hash    # compare against the .sha256 on the release
gh attestation verify $z --repo Zigecek/ccred
Expand-Archive $z -DestinationPath .
```

Building from source:

```sh
cargo install --path .
```

## Uninstall

```sh
ccred uninstall --dry-run   # what it would remove, and what it would keep
ccred uninstall             # schedule, installer receipt and binary; profiles stay
ccred uninstall --purge     # ...and every stored profile, backup and log
```

`--purge` asks before deleting stored credentials, because they are the only
copies of accounts that are not logged in right now. Without a terminal to ask
on it refuses, unless `--yes` was given. It also refuses -- before removing
anything -- if the data directory holds files `ccred` did not create, or
overlaps your home or Claude Code's directory, which a stray `CCRED_HOME` could
otherwise turn into a disaster.

Your Claude Code login in `~/.claude` is never touched.

A binary installed by Scoop, apt, npm, Homebrew or `cargo install` is left in
place, and the command that removes it is printed: deleting a file a package
manager tracks leaves it believing the package is still there. A schedule that
starts a *different* copy of `ccred` is left alone too.

## Usage

```
ccred current             who is logged in, and which profile is active
ccred list                saved profiles and how much refresh window each has
ccred save <name>         store what is logged in: Claude Code as <name>,
                          Claude Desktop as <name>-desktop
ccred switch <name>       make a saved profile the active account
ccred rename <old> <new>  give a profile another name
ccred rm <name>           delete a profile (--purge deletes its copies too)
ccred restore <name>      put a profile's credentials back, or the profile itself
ccred refresh             keep stored profiles from expiring (--dry-run to just look)
ccred schedule install    run that refresh automatically, twice a week
ccred schedule status     is it registered, and when does it next run
ccred doctor              check for anything quietly wrong
ccred log                 what past refreshes decided, and which ones the timer started
ccred uninstall           remove ccred from this machine
```

Add `--json` to any of them for machine-readable output.

Profiles live in `~/.ccred` and Claude Code's configuration in `~/.claude`.
`--ccred-home` and `--claude-config-dir` (or `CCRED_HOME` and
`CLAUDE_CONFIG_DIR`) move them. A schedule installed with either is
registered with the flags themselves: a systemd user service or a Windows task
does not inherit the shell that set a variable, and without them the job would
refresh the default directories -- empty -- and report success.

A bare `ccred <name>` is deliberately not a switch alias -- a profile called
`list` would then be unreachable -- so it prints a hint instead of guessing.

A scheduled `refresh` is safe to fire more often than asked. The job runs
`refresh --if-older-than 48`, and with that flag the command records when it
last ran and exits successfully without doing anything if that was recent --
so systemd catching up, launchd coalescing, or a Windows task with both a
boot trigger and a schedule all cost nothing. The rate limit lives in the
command rather than in three schedulers' settings. A bare `ccred refresh`
you type has no such limit: you asked, so it works.

The save that creates your **second** profile registers that schedule for you,
because that is the moment an idle account starts expiring unattended and the
only moment someone is certainly watching. It says so when it does it, it never
does it on a re-save, and `CCRED_NO_AUTO_SCHEDULE=1` turns it off for
provisioning or for anyone who schedules refreshes their own way. A packaged
install (apt, `.deb`) never registers anything: it runs as root, while the
timer belongs to one user's session.

On Windows the preferred task definition needs administrator rights: both
`S4U` (runs while signed out) and a boot trigger are refused with `Access is
denied` for a standard user. Rather than fail, registration falls back to a
definition that any user may create -- it runs only while you are signed in,
and says so. A missed run is still caught up afterwards, so what is lost is the
signed-out case, not reliability in general.

If the scheduler refuses, the profile is still saved. Storing credentials is
the operation that matters; the schedule is reported as failed and left to
`ccred schedule install`.

Every accepted save leaves a copy of the credentials beside the profile, and
`ccred restore <name>` puts it back. That is not theoretical tidiness: a
scheduled refresh once spawned Claude Code against a profile, the binary
decided it was signed out, and it wrote an empty credential blob over the
store. Every safety gate here guards what *ccred* writes, and that write was
not ours. `refresh` now undoes such damage by itself, and `doctor` says which
profiles have a copy worth restoring.

`ccred refresh --dry-run` says what each profile would do and stops there --
nothing spawned, nothing written, not even a log entry. It is how to find out
why the schedule left a profile alone without spending an exchange to see.

`ccred schedule install --dry-run` prints the exact unit file, plist or task
XML it would register, without writing anything.

A registered schedule that fires into a failure looks, from the terminal,
exactly like one that works: the job writes into a log, and on Windows Task
Scheduler discards even that. So `ccred doctor` reads the record of the last
run and says how long ago it was and whether anything needed attention. A
week without one means they are not happening -- the gap between scheduled
runs is four days at most.

Installing verifies its own work: right after registering, it asks the
scheduler when the job will next run, and if the answer is "never" it undoes
the installation and says so. Each platform has a way to accept a schedule
that silently never fires -- a systemd timer with `Persistent=` on a monotonic
trigger, a launchd `Weekday` out of range, a Windows task left with the
default refusal to start on battery -- and all of them look installed.

Switching refuses to run while Claude Code is open. A live session holds the
old account in memory and would write its next refreshed token into what is by
then a different profile's file. Quit it first, or pass `--force` knowing that.

**Claude Desktop is the exception, in both directions.** The Desktop app logs
in on its own and hands every Claude Code session it opens its own token, so
those sessions never read the credential file: `ccred switch work` does not
move them to the new account, and it does not need them closed either --
`ccred` tells them apart and only counts the rest. The terminal and the VS
Code extension log in through the file and follow every switch. The Desktop
has a profile of its own, `work-desktop`, switched separately; see below.
Sessions themselves are ordinary transcripts under `~/.claude/projects/`
whichever way they were opened, so a Desktop conversation can be continued
under the switched account with `claude --resume` from a terminal.

### Claude Desktop

The Desktop app logs in on its own, so `ccred save work` records two things:
`work`, Claude Code's login, and `work-desktop`, the Desktop's. `ccred list`
shows them in two tables, and they are switched separately -- `ccred switch
work` moves the terminal and the VS Code extension, `ccred switch
work-desktop` moves the Desktop -- because the two can legitimately be on
different accounts. `--only-code` and `--only-desktop` save one half; the
output always says which halves were saved and why the other was not.
`-desktop` is reserved: it cannot be a Claude Code profile's name.

One Desktop account goes under one name. Saving a second Claude Code
profile while the Desktop sits on the first account saves that profile and
says which name already holds the Desktop login: there is one login
directory, and two names for it would both claim to be the live one with
nothing to switch between.

```sh
ccred save work                  # work, and work-desktop if the Desktop is logged in
ccred switch personal-desktop    # parks work-desktop, starts Desktop fresh; log in as personal
ccred switch work-desktop        # Claude Desktop only; quit it first
ccred switch work                # Claude Code only; the Desktop can stay open
ccred rm work-desktop            # the Desktop profile only; `rm work` leaves it
```

The Desktop's token cannot be switched the way Claude Code's can. It is
encrypted with the operating system's keyring, so nothing here can read
it, check it or tell whose it is -- and every safety rule in this tool
depends on doing exactly that before a write. What can be moved is the
Desktop's whole data directory (`~/.config/Claude`, `~/Library/Application
Support/Claude`, `%APPDATA%\Claude`): one complete logged-in state, all of
it belonging to one account.

On Windows there are two of those, and which one you have does not depend on
the Store. The classic installer writes to `%APPDATA%\Claude`. The MSIX
package -- what a downloaded installer produces as readily as the Store --
runs with filesystem redirection: it writes what it believes is
`%APPDATA%\Claude`, and Windows puts it under
`%LOCALAPPDATA%\Packages\Claude_<publisher>\LocalCache\Roaming\Claude`.
A process outside the package sees only the second, so ccred looks in both
and takes whichever holds a `config.json`, the most recently written one if
both do. `ccred doctor` names the directory it settled on. A Desktop profile is therefore a name, an
account, and at most one such directory -- the live one while the Desktop
is logged in as that account, or a parked one under
`~/.ccred/desktop/<name>/data`. `save` records the identity and leaves the
live directory where it is; nothing is ever copied, because a token that
cannot be checked is not one to duplicate. `switch work-desktop` parks the
live directory under the profile whose account it holds -- read from the
directory's own `lastKnownAccountUuid`, never assumed -- and puts `work`'s
parked one in its place. When nothing is parked for `work` yet, the Desktop
starts fresh and asks for a login: log in as that account, and the next
switch away parks it under the right name. That is how a second Desktop
account is brought in: with your current login saved (`ccred save work`),
quit the Desktop and run `ccred switch personal-desktop`. It parks
`work-desktop`, prepares a fresh state for `personal-desktop`, and you
simply open the Desktop and log in -- that login belongs to
`personal-desktop` from then on.

Switch rather than signing out inside the app. Signing out throws the login
away -- the token is gone, and only a fresh login brings that account back --
while leaving its chat list behind in the directory, where the next account
to log in inherits it. Nothing is deleted, but the chats stop appearing in
the sidebar, which looks exactly like deletion. `ccred switch` parks the
whole directory intact instead; and when a sign-out has already happened,
`ccred switch <name>-desktop` takes that list out and hands it back to the
profile it belongs to.

Three things follow from moving a directory rather than writing a file:

- **The Desktop has to be closed.** A directory renamed under a running app
  is split, not moved, and on Windows the rename is refused. `switch
  <name>-desktop` checks Electron's single-instance lock and refuses before
  touching anything -- and only when it would actually move something; a
  Desktop already on the target account can stay open. A plain `switch`
  never asks, because Claude Code logs in separately.
- **A parked login ages.** The Desktop refreshes its token only while it is
  the one running, and `ccred refresh` cannot help it. When a parked login
  has expired, the Desktop asks to log in after the switch. That is the
  whole cost.
- **Chats stay.** Code sessions live in `~/.claude/projects/` and are not in
  the moved directory. The Desktop's own sidebar index is, and it is
  per-account anyway.

A Desktop logged in as an account nobody saved is left where it is, unless
the target has a parked login to bring in -- then it is parked under an
`unclaimed-<timestamp>` name that nothing switches back to, and `doctor`
says where. `ccred current` shows which account the Desktop is on.

**One sidebar for every account -- of Claude Code sessions.** This is about
the Claude Code sessions the Desktop lists, the ones started from inside the
app: it keeps that list per account, under
`claude-code-sessions/<account>/<organization>/`, as one small JSON file per
session. Your conversations with Claude are not in there and are not what
this moves -- they belong to the account on Anthropic's side, the Desktop
reads them from the server, and switching accounts shows that account's own.
Nothing local can, or should, carry those across.

The sessions, though, are not per account -- they are transcripts under
`~/.claude/projects/`, and Claude Code opens any of them under whatever
login it has. So every account's list feeds one shared sidebar under
`~/.ccred/desktop/.sidebar/`, and a switch brings the target account's list
up to it: sessions it lacks are added, a title changed under one account
changes under the others, a session deleted under any account is deleted
everywhere, and the sidebar groups follow. Only the part of an entry that
is the session's travels -- which transcript, where, what it is called,
when; what the Desktop wrote for the account (connector configuration,
bridge ids) stays with that account and is filled in for the new one when
the session is opened. What counts as a session entry is a file with a
`sessionId` in it, read rather than assumed from the name, so a Desktop that
renames them still works -- and `scheduled-tasks.json`, which lives in the
same directory, is not one. The list is read whenever a profile is saved or
parked, so a running Desktop still feeds it; writing into it needs the
Desktop closed, and a switch says so when it could not. A switch that finds
nothing to share says that too: someone who has never started a Claude Code
session from inside the Desktop has an empty list, which otherwise looks
exactly like a feature that did not run.

What this does *not* do is keep the accounts apart on the server. The
Desktop registers a session with Anthropic under the account it is logged
in as when that session runs -- that is how sessions from another machine
show up in a sidebar -- so a session you run under someone else's account
is listed under that account, title and all, for whoever uses it elsewhere.
Listing a session does not do that; running it does. If an account is not
yours alone, that is the cost of running under its tokens, with or without
a shared sidebar.

**Do not sign out inside the Desktop to change accounts; switch with
`ccred` instead.** The Desktop keeps the sidebar's list of Code sessions
per account inside its directory, and the sidebar groups in its config, and
signing out leaves both where they are. Log in as someone else and the
directory is theirs -- the first account's chats are still in it, the
transcripts are still in `~/.claude/projects/`, but the sidebar of the
first account shows nothing once it is parked under the second one's name.
`ccred switch <name>-desktop` knows this shape: it gathers that account's
list and groups from the live directory or any parked one, and puts them
back into the account's own directory once the Desktop is logged in as it
(with the Desktop closed, since that rewrites its config). `doctor` and
`list` say when something is waiting to be put back.

What has been checked against a real Desktop, on Linux: that it reads the
account from `lastKnownAccountUuid`, that the running-app check sees it,
and -- by hand -- that a directory moved away and back keeps its login. The
running-app check on macOS and Windows follows the same Chromium lock
without having been tried on real hardware.

### When something is wrong

`ccred doctor` names it and says what to do. The four answers it gives most
often:

- **"the live refresh token has expired", or a profile that says `needs
  login`.** The refresh deadline is fixed when you log in, and no amount of
  refreshing moves it -- so when it arrives, only a login will do:
  `claude auth login`, then `ccred save <name>` under the same name. That
  clears the profile's latched state too.
- **"Claude Code is authenticating from CLAUDE_CODE_OAUTH_TOKEN"** (or
  `ANTHROPIC_API_KEY`, or a Bedrock/Vertex variable). Claude Code then
  ignores the credential file, which ages out while everything appears to
  work. `ccred` manages file-based logins only: unset the variable, including
  in your shell's startup file, and log in normally.
- **"the active profile is not the account that is logged in"**, or **"the
  live credentials are the ones stored as profile X"**. The pointer and
  reality have drifted apart -- usually after logging in without saving.
  `ccred switch <name>` to the profile you meant, or `ccred save <name>` for
  the account that is logged in now.
- **A profile that says `blocked`.** Something a person has to settle;
  the line underneath says which. Re-running the refresh will not change it.
- **"no `claude` binary to refresh with".** Switching profiles does not need
  it, but every scheduled refresh spawns it. The usual cause on Linux is a
  `claude` installed by nvm, volta, bun or asdf: those put their bin
  directory on PATH from a shell profile, and a systemd timer reads no
  profile, so the timer cannot find what your terminal finds. Those
  locations are searched directly, and for anywhere else
  `ccred schedule install --claude-path /full/path/to/claude` registers the
  path with the job.

If a profile's credentials were damaged, `ccred restore <name>` puts the last
known good copy back, and `~/.ccred/backups/<name>/` keeps the ten before that.

`ccred rm <name>` adds one more copy before deleting the profile, since a
mistyped name is the one mistake here that cannot otherwise be taken back --
and `ccred restore <name>` puts a removed profile back from that copy. The
credentials only: the account details a switch restores are not in it, so the
profile reads as an unknown account until the next save fills it in.

`ccred rm <name> --purge` is the opposite request: delete the profile and
every copy of that account, including the ones a switch left behind when its
credentials belonged to no profile.

### Exit codes

For scripts and schedulers, each code means one thing to do:

| Code | Meaning | What to do |
|---|---|---|
| 0 | Done, or nothing needed doing | nothing |
| 1 | Internal error, or `uninstall` could not finish | read the message |
| 2 | Usage error, or a confirmation was declined | fix the command |
| 3 | No such profile, or a name that cannot be one | check `ccred list` |
| 4 | `refresh` found something a person must handle: a login, a blocked or broken profile, no `claude` to refresh with, a deadline within five days | read the output; retrying soon will not help |
| 6 | Busy: another process holds the credential store's lock, or another `ccred` is working on the profiles | retry shortly |
| 7 | A write was refused as unsafe, or `doctor` found an error | stop and read the message |
| 8 | Misconfigured: the scheduler cannot be used here | fix the setup |

A lost network connection during `refresh` exits 0 on purpose, so a laptop
that was offline does not mark the job as failed; the profile is retried on
the next run. Code 5 is reserved.

### What it looks like

```
$ ccred list

  PROFILE  ACCOUNT            PLAN     REFRESH WINDOW  LEFT      SYNCED  STATE
  ● work   ada@example.com    Max 20x  ███████████░░░   23d     3 h ago  ok
    home   grace@example.com  Max 5x   ██░░░░░░░░░░░░    4d  9 days ago  expiring

  2 profiles, 1 needs attention
```

```

```
$ ccred list   # with a Claude Desktop login saved too

  Claude Code
  -----------

  PROFILE  ACCOUNT            PLAN     REFRESH WINDOW  LEFT      SYNCED  STATE
  * work   ada@example.com    Max 20x  ###########...   23d     3 h ago  ok
    home   grace@example.com  Max 5x   ##............    4d  9 days ago  expiring

  2 profiles, 1 needs attention

  Claude Desktop
  --------------

  DESKTOP         ACCOUNT            SYNCED  STATE
  * work-desktop  ada@example.com   3 h ago  logged in
    home-desktop  grace@example.com       -  parked

  2 Desktop logins
```

```
$ ccred current

  ● work   ada@example.com   · Max 20x

  Access     █████████░░░░░  5 hours
  Refresh    ███████████░░░  23 days
  Synced     3 h ago
  Profiles   2 saved   (ccred list)
```

Colour follows `NO_COLOR` and `CLICOLOR_FORCE`, and is dropped whenever output
is not a terminal -- which is what keeps systemd, launchd and Task Scheduler
logs free of escape sequences. Drawing characters fall back to ASCII unless the
terminal is known to handle UTF-8; set `CCRED_UNICODE=1` or `0` to overrule
that guess.

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

**It assumes the other writer can be wrong.** Pointing Claude Code at a
profile's store is the one place something other than `ccred` writes into a
profile, and every safety gate here guards what *we* write. So `refresh` takes
the credentials as they stand before it spawns anything and puts them back if
what comes out is unusable. That is not defensive decoration: a scheduled run
once decided it was signed out and wrote an empty credential blob over a live
profile.

**A failure is contained to the profile it happened to.** A store that will not
load, a spawn that will not start, a profile whose metadata is corrupt -- none
of them ends the run, the listing or the diagnosis for the others. The commands
you reach for when something is wrong have to survive meeting it.

## Development

```sh
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Inspect a real credentials file without printing any secret:

```sh
cargo run --example check_roundtrip -- ~/.claude/.credentials.json
```

Conventions live in [CLAUDE.md](CLAUDE.md); the threat model and the rules CI
enforces are in [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
