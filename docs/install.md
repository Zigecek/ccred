# Installing

Nothing is published yet. This describes the intended channels and, more
usefully, the reasoning behind which ones are worth maintaining.

## Channels

| Channel | Command | Notes |
|---|---|---|
| npm | `npm i -g ccred`, or `npx ccred` | Prebuilt binary, no download at install time |
| crates.io | `cargo install ccred` | Builds from source |
| GitHub Releases | shell / PowerShell installer | What every other channel points at |
| Homebrew | `brew install Zigecek/ccred/ccred` | macOS and Linux |
| Scoop | `scoop install ccred` | Windows; no gatekeeper, self-updating manifest |
| apt | `sudo apt install ./ccred_<ver>_amd64.deb` | A real `.deb` from the release page |
| AUR | `yay -S ccred-bin` | Prebuilt; a source variant can follow |

## A note on the Windows command line

The Windows one-liner is `powershell -ExecutionPolicy Bypass -c "irm <url> |
iex"`. That is character for character the shape uv documents. Both parts look
worse than they are, so both are explained here.

### `-ExecutionPolicy Bypass` does not bypass anything

Execution policy never applied to this command. It governs *script files*;
`irm <url> | iex` is an individual command operating on a string, and even
`Restricted` does not stop it. Microsoft says so directly: the policy "isn't a
security boundary, it's defense in depth ... users can easily bypass a policy
by typing the script contents at the command line".

The flag is present because cargo-dist's installer template checks its own
execution policy in `Initialize-Environment` and throws unless it is
`Unrestricted`, `RemoteSigned` or `Bypass`. On a default Windows client
`powershell.exe` reports `Restricted`, so without the flag the script
downloads, starts, and then kills itself around line 40. The flag satisfies a
check the script performs on itself, and nothing else. That check is wrong for
the piped code path; it is cargo-dist#833 upstream, still open.

`pwsh` 7.x stores its policy separately and does not need the flag at all.

### `-NoProfile` was a measurement error, and has been removed

An earlier revision of this file defended `-NoProfile` on empirical grounds --
three runs each way, deterministic, with a zero-byte user profile named as the
trigger. That measurement was invalid and everything built on it was wrong.

`Trojan:Win32/Commando.A!ml` is a cloud-backed verdict on the command line: the
download-cradle pattern, MITRE T1059.001, matched by a published Sigma rule.
The verdict is not stable over time. On one machine an identical command line
produced a detection six times out of six, and about fifteen minutes later was
clean three times out of three, with nothing changed locally. Any A/B short
enough to run by hand measures cloud state rather than the flag under test.

`-NoProfile` also appears in publicly documented `Commando.A!ml` detections, so
it cannot be protective, and cargo-dist never emitted it -- it was added here
by hand. Removing it matches what uv, rustup and starship actually ship.

### The detection and the install failure are the same event

When Defender blocks the command, the connection is terminated mid-download.
`irm` yields nothing, `iex` receives an empty string, and PowerShell reports an
empty-string error followed by an unterminated block comment. That error was
diagnosed here first as a PowerShell 5.1 incompatibility and then as a corrupt
user profile. Both were wrong. There is one cause and it is the block; the
install failure is a symptom of the detection, not a separate bug.

### What actually helps

Nothing in the flags. What helps is not needing the cradle:

- `npm i -g ccred` -- no policy, no cradle, no SmartScreen prompt
- `scoop install ccred` -- the same, and the manifest is already written
- `winget install` -- once submitted, and only as a zip/portable manifest:
  SmartScreen prompts come from `ShellExecuteEx`, which winget does not use to
  launch a shimmed binary
- a manual download verified with its `.sha256` and `gh attestation verify`

## Code signing

Not worth buying yet, and the reason is not frugality. Signing's value is that
reputation accumulates across releases under one publisher identity; unsigned
binaries restart from zero every release. With zero releases and no download
volume, a certificate buys a publisher name inside a warning dialog.

An EV certificate specifically is not worth it: it stopped conferring instant
SmartScreen reputation in 2024, and Microsoft now says paying the premium for
that reason is no longer justified.

When it is time, in order: SignPath Foundation (free for OSS, but it requires
an already-released, actively maintained project), then Certum's open-source
cloud certificate at EUR 49. Azure Artifact Signing is ruled out -- individual
developers are limited to the USA and Canada.

GitHub attestations are already enabled in `dist-workspace.toml`. They are
worth having, but they do not affect SmartScreen or Defender at all; Fulcio is
not in the Microsoft Trusted Root Program and sigstore does not produce
Authenticode signatures.

## Deliberately not packaged

**Snap and Flatpak.** Both sandbox the filesystem, and this tool's entire job
is reading `~/.claude`. That is a categorical incompatibility, not a packaging
inconvenience, and no amount of portal configuration makes it a good fit.

**Launchpad PPA.** Requires full Debian source packaging and vendoring every
crate, because Launchpad builders have no network access. Ubuntu only.
Disproportionate for a tool this size.

**Chocolatey.** Heaviest moderation of the Windows options, aimed at
enterprise IT rather than people who live in a terminal. Scoop and winget
cover the same users with far less friction.

## Order of work

1. GitHub Releases via cargo-dist. Everything else is an adapter over it.
2. crates.io.
3. npm -- the best acquisition route, and what makes Windows painless.
4. Homebrew tap, Scoop bucket, `.deb` via nfpm.
5. AUR, then winget.

## A note on the `.deb`

It carries no maintainer scripts, on purpose. A `postinst` must not install the
systemd user timer: the package installs as root, the timer belongs to one
user's session, and `~/.claude` may not exist yet. Scheduling stays an explicit
`ccred schedule install`, run by the person who wants it.
