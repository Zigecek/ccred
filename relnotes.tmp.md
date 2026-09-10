First release.

`ccred` saves each Claude Code account as a named profile, switches the active
one, and keeps idle profiles from expiring by running the real `claude` binary
against each profile's own credential store. It never contacts an OAuth
endpoint itself.

**Unofficial and independent.** Not affiliated with, endorsed by, or sponsored
by Anthropic PBC.

## Install

```sh
# Linux and macOS
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Zigecek/ccred/releases/download/v0.1.0/ccred-installer.sh | sh
```

```powershell
# Windows
powershell -NoProfile -ExecutionPolicy Bypass -c "irm https://github.com/Zigecek/ccred/releases/download/v0.1.0/ccred-installer.ps1 | iex"
```

`-NoProfile` matters: without it the install can fail on a machine whose
PowerShell profile interferes, with errors that look like a corrupt download.

Or download the archive for your platform below. Every archive has a `.sha256`
checksum beside it.

## Status

Linux and Windows are tested. macOS builds and its unit tests run in CI, but
the tool has never been exercised on real macOS hardware, and the Keychain
backend is not implemented -- on that platform only the plaintext fallback
file is visible. Treat macOS as unverified.

Registry publishing (crates.io, npm, Homebrew, AUR) is configured but not yet
live; see `docs/publishing.md`.
