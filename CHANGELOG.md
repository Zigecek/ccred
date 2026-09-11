# Changelog

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
