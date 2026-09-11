# Changelog

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
