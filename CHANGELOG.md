# Changelog

## 0.2.7

### `ccred log`

A scheduled run left nothing anyone could read. systemd captures stdout in the
journal and launchd can be pointed at a file, but Windows Task Scheduler
discards it entirely -- so on the platform where a job is least visible there
was no record at all. When a refresh quietly emptied a profile, the only
reason it could be explained was that someone happened to be watching at the
time.

Each run now appends a line: what every profile was decided to be, and how its
refresh window moved. Decisions and numbers only, never the detail text --
that is built from rendered error messages, and the rule here is to persist
error *kinds*, because a message can echo its input and that input can be a
token. The file rotates at a megabyte and keeps three generations.

### The sandbox was not a sandbox

Writing that found `log_dir` was derived from `LOCALAPPDATA` or
`XDG_STATE_HOME`, read from the process environment rather than from the
`Paths` it belongs to. Neither a test sandbox nor `CCRED_HOME` relocated it,
which means this project's own test suite had been appending its runs to the
real user's log file. Logs live under the ccred home now, with everything else
this tool owns, and a test pins that nothing is written outside the home it
was given.

## 0.2.6

### A failed probe says which kind of failure it was

"Did not move the refresh window" covered two situations that call for
opposite reactions: the probe was too weak and did nothing, or it did exchange
tokens and the server refused. Only the first is worth trying a harder rung
for; only the second means the account is in trouble. Comparing the store
revision separates them, which is what that method was written for and never
used for.

### Documented

- The README claimed nothing was published anywhere. The Scoop bucket is live
  and verified, and every release carries a `.deb` for amd64 and arm64; both
  now come first, with the registries named as waiting on account setup rather
  than implied to be missing.
- Why the scheduler passes `--if-older-than 48` and not the 72 the design
  sketch suggested: the shortest real gap between runs is 72 hours, so 72
  would sit exactly on the interval and could swallow a run it was meant to
  allow.

### Tested

Every source file now has tests. The last three to get them were the exit-code
mapping, the load contract, and the schedule spec -- respectively the thing
schedulers read, the thing that stopped the shell version destroying a
profile, and the two properties that decide whether a registered job can start
at all.

## 0.2.5

### A profile with a live access token is no longer spawned for

Measured on a live machine: a profile with twenty-one days of refresh window
and six hours left on its access token ran all three probes, moved nothing,
and was recorded as a failure with a backoff.

The refresh window only extends when Claude Code performs a token exchange,
and it only does that when the access token needs renewing. A profile whose
access token is still valid therefore cannot be refreshed at all -- the run
was judging a correct no-op by a yardstick that did not apply. It now waits
instead, which is a short wait: an access token lives about eight hours
against a schedule measured in days.

Without this, every recently saved profile collected spurious failures.

### Fixed

- `trust_dir` is gone rather than wired up. Connecting it would have meant
  marking a directory trusted in the user's own configuration on their behalf,
  to avoid a prompt that does not appear.

### Tested

- The exit-code mapping. A scheduler reads nothing else, so those numbers are
  a published contract, and the distinction this tool cares most about --
  "this machine is not set up" versus "your credentials are in danger" --
  could until now have been swapped without anything noticing.
- The journal recovery arms that decide whether an interrupted switch rolls
  back or is merely litter. Two of the five phases had never executed.
- The switch paths that degrade instead of failing: a profile deleted from
  under the pointer, and one saved before account details were captured.
- The helpers in `src/ui/render.rs`, the file the conventions name as the one
  place to audit for "no output may ever contain a token", which had none.
- The command tree, through clap's own `debug_assert`, which is the
  difference between finding a bad flag here and finding it when a user runs
  that command.

## 0.2.4

### `ccred restore <name>`

A copy of each profile's credentials has been written after every accepted
save since the beginning, and nothing ever read it. That stopped being
academic when a spawned Claude Code signed itself out and emptied a profile on
a live machine: the copy was the only thing that survived, and getting it back
meant editing files over SSH.

`restore` puts it back, and `doctor` now says which unusable profiles have a
copy worth restoring rather than sending everyone to `claude auth login`.

### A timeout that did not time out

The first test ever written for the probe runner found it. Killing the child
does not necessarily close the pipe -- if that child spawned one of its own,
the grandchild still holds the write end and the reader thread stays blocked
on it. Waiting for that thread then undid the deadline that had just fired: a
30-second process survived a 400-millisecond timeout. On a scheduled run that
is a job that never ends.

### Fixed

- A profile whose credentials a probe cleared is reported as needing a login,
  not as broken. They are readable again by then; what is gone is the server's
  willingness to refresh them.
- A missing `claude` is a configuration problem (exit 8), not an unsafe state
  (exit 7), and it no longer aborts the whole run -- the active profile's
  mirror does not use the binary at all. The message names `--claude-path`
  rather than a configuration file nothing reads.

## 0.2.3

**Upgrade from 0.2.1 or 0.2.2.** Both shipped a check that reported healthy
profiles as broken.

### The environment oracle is gone

0.2.1 added a check meant to prove the spawned `claude` had really used the
profile's credential store, by comparing the account it reported against the
profile's own. It reads that account from the cached `oauthAccount` in the
shared `.claude.json` -- which we deliberately leave shared -- so it names the
live account regardless of which store was read. On a real machine it declared
an untouched profile "ignored the credential directory".

No oracle is needed. Measured against claude 2.1.236: pointing
`CLAUDE_SECURESTORAGE_CONFIG_DIR` at empty credentials makes it report
`loggedIn: false`, while without the variable it is logged in. The variable is
honoured, so the existing window-movement test is the whole proof -- had
another store been read, the profile's own file could not have changed.

### A spent refresh token is now reported as needing a login

When a refresh token has already been used, Claude Code prints:

    Failed to authenticate: OAuth session expired and could not be refreshed

None of the phrases this looked for appear in it, so the profile was reported
as broken rather than as needing a login -- which is the one thing the user
can actually act on.

### Added

- `ccred refresh --force` tries every profile now, ignoring the backoff, the
  window threshold and the run-level rate limit. Those are right for a timer
  and wrong for someone who has just fixed what was broken and wants to watch
  it work. It does not clear `needs_login`: a human is genuinely required
  there, and spawning a process that cannot succeed only wastes a rate limit.

## 0.2.2

### A spawned `claude` can no longer empty a profile

This is written from an incident, not a review. A scheduled refresh on a
live machine spawned `claude` against a profile's own credential store; the
binary decided it was signed out and wrote an empty credential blob straight
over it. Every safety gate in this tool guards what *we* write -- that write
was not ours, so nothing stopped it. The profile survived only because a
last-known-good copy happened to be sitting beside it.

`refresh` now takes the store as it stands before spawning anything, and
after each probe checks whether it went from usable to unusable. If it did,
the good copy goes back, `needs_login` is latched and the run says so.
Restoring a token that later turns out to be dead costs nothing, because
`validate` reports it on the next run; not restoring costs the account.

### Fixed

- `save` and the refresh mirror take the credential store lock. Only `switch`
  did, and those two are what a timer runs -- so they are the likeliest to
  collide with a live session refreshing its token, and a read landing
  mid-write copies half of one token pair and half of the next.
- The refresh rate limit was armed by any run that finished, including one
  where every profile came back broken, turning a transient failure into two
  days of silence. Only a clean run suppresses the next one now.
- `publish-npm` upgraded npm in place, leaving the running npm without part
  of its own dependency tree. It takes npm 11 from Node 24 instead.
- The packaging trigger required the whole release run to succeed, but that
  run also carries the registry publish jobs, which fail while the accounts
  are unconfigured. It gates on the release existing instead.

## 0.2.1

### Refreshing could not have worked

`env_pairs_for` set `CLAUDE_CONFIG_DIR` alongside
`CLAUDE_SECURESTORAGE_CONFIG_DIR`. Only the second was wanted: it relocates
the credential store and leaves `.claude.json`, `projects/`, `sessions/` and
MCP config shared. The first moves the whole configuration, so every spawned
probe ran against an empty directory -- no config, no trust state for the
working directory, no MCP. A non-interactive run in that state has nothing to
refresh and may stop on a trust prompt instead.

The oracle that proved the environment took effect went with it.
`projectsDirectory` follows `CLAUDE_CONFIG_DIR`, so it can no longer answer
that question. The account Claude Code reports having authenticated as is
direct evidence instead.

### `switch` could destroy an account outright

The outgoing mirror is deliberately skipped when the live account is not the
one the pointer names -- which is exactly the case where those credentials are
stored in no profile. The next step overwrote them. Someone logged into an
account they had not saved lost it by running the command meant to organise
their accounts. Those credentials are now copied aside first, and the report
says where.

### Fixed

- `scopes` round-trips faithfully. With `skip_serializing_if` on an empty
  `Vec`, an input of `"scopes": []` re-serialised to nothing, the lossless
  guard correctly called that a dropped key, and every command touching such a
  file failed with exit 7.
- The release job's `--target` is gone. Supplying `target_commitish` makes
  GitHub demand `workflows: write` on top of `contents: write` whenever the
  released commit range touches `.github/workflows`, and `GITHUB_TOKEN` cannot
  hold that scope. This is the 403 that made every release be assembled by
  hand.
- `packaging` now chains off the release workflow. It triggered on
  `release: published`, which never fires for a release created with
  `GITHUB_TOKEN`, so no `.deb` or AUR package had ever been built.
- `publish-npm` pinned Node 22.14 and then installed `npm@latest`, which
  requires 22.22.2. It could never reach the registry.
- Both called publish workflows download release assets but were granted no
  `contents` permission.
- The Homebrew job is gated on its token existing, so an unconfigured tap no
  longer fails every release.
- The nfpm download was verified against a filename that was never written to
  disk, and the checksum grep also matched the `.sbom.json` line.
- Every action in `release.yml` is pinned to a commit, including
  `actions/attest`, which mints this tool's provenance.
- The Scoop manifest carried a placeholder hash and an `extract_dir` for a
  directory the archive does not contain.

## 0.2.0

### The refresh schedule registers itself

The save that creates a **second** profile now registers the background
refresh, because that is the moment an idle account starts expiring
unattended and the only moment someone is certainly watching. It never fires
on a re-save, so a schedule that was deliberately removed does not come back,
and `CCRED_NO_AUTO_SCHEDULE=1` opts out for provisioning. Registration can
never fail a save: the credentials are stored by then, and losing that result
because a scheduler refused would trade the operation that matters for one
that does not.

### Three ways scheduling was broken on Windows

**It could not be registered at all without administrator rights.**
`schtasks` answers `Access is denied` for `<LogonType>S4U</LogonType>` and,
independently, for any `<BootTrigger>`. Ordinary task creation needs no
elevation, so the feature was out of reach only because we insisted on the
better definition. Registration now asks for that first and falls back to
`InteractiveToken` without a boot trigger, which any user may create. What is
lost is the signed-out case, not reliability -- `StartWhenAvailable` still
catches up a missed run -- and `schedule status` says so.

**`schedule status` reported "installed, but disabled" on every machine.** The
task state was read from any `/V` row whose *value* was `Disabled`, and a
healthy task has two: `Idle Time` and `Delete Task If Not Rescheduled`. The
state now comes from `<Enabled>` inside `<Settings>` of the registered XML,
which is unambiguous and not localised.

**`doctor` never checked the schedule at all.** Every other finding reports on
a profile as it stands today, so a machine with nothing refreshing its idle
profiles passed cleanly and then lost an account a fortnight later.

### Output is rendered rather than printed

Every command was a column of unaligned `println!`. There is now a
presentation layer: aligned tables, meters for the remaining refresh window,
colour that says how worried to be, and a styled help screen.

Colour is `anstream`'s job. It strips escapes when stdout is not a terminal,
which keeps systemd, launchd and Task Scheduler logs free of them, and it
honours `NO_COLOR` and `CLICOLOR_FORCE`. Unicode drawing characters are
opt-in: a terminal not known to handle UTF-8 gets ASCII, because a tool people
run when something is already broken must not add mojibake to the problem.
`CCRED_UNICODE=1` or `0` overrules the guess.

Reports also carry more than they used to: when a profile was last synced, the
plan it is on, and the access token's remaining life in hours -- it lives about
eight hours, so counting it in days always read as "0 days left". `--json`
keeps the raw API values.

### Fixed

- `packaging.yml` contained a GitHub expression with no right-hand operand.
  The YAML parsed, so nothing local caught it, but GitHub refused to load the
  workflow. No `.deb` or AUR artifact had ever been produced by CI.
- The Windows install command no longer passes `-NoProfile`. It was added on
  the strength of an A/B that does not hold up: `Trojan:Win32/Commando.A!ml`
  is a cloud-backed verdict on the download-cradle command line and is not
  stable over time. See `docs/install.md`.

## 0.1.0

First release. Saves each Claude Code account as a named profile, switches the
active one, and keeps idle profiles from expiring by running the real `claude`
binary against each profile's own credential store. It never contacts an OAuth
endpoint itself.
