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

## A note on the Windows one-liner

cargo-dist suggests `powershell -ExecutionPolicy Bypass -c "irm <url> | iex"`.
That fails on **Windows PowerShell 5.1** with a misleading pair of errors about
an empty string and an unterminated block comment.

It is not a download problem and not a permissions problem. Measured: the
string reaching the pipeline is one object of the correct length whose content
is byte-identical to the file on disk, the script parses cleanly under 5.1, and
the same script run from a file installs perfectly. PowerShell 7 also handles
the piped form. Something about executing this particular script as a piped
string under 5.1 breaks it, and the exact mechanism is not pinned down.

So the documented Windows command downloads first and then runs. Slightly
longer, works on both.

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
