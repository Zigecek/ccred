# Security

## Reporting

Report a vulnerability privately through GitHub's
[security advisory form](https://github.com/Zigecek/ccred/security/advisories/new)
rather than a public issue. Expect an acknowledgement within a week.

## What this tool touches

`ccred` reads and writes Claude Code's credential files, which contain OAuth
access and refresh tokens for your Anthropic account.

- Profiles live in `~/.ccred/profiles/<name>/`, mode `0600` on Unix.
- On Windows the files inherit the profile directory's ACL, which is what
  Claude Code itself relies on. That inheritance was measured rather than
  assumed: under a default `%USERPROFILE%` a credential file ends up granting
  exactly SYSTEM, `BUILTIN\Administrators` and the owner -- no `Users`, no
  `Everyone` -- which is what `0600` buys on Unix, where root reads it anyway.
  Explicit ACL code is deliberately not written: it would be unsafe code, in a
  credential tool, to reach a state the file is already in.
- `ccred doctor` checks the result instead of trusting either mechanism. On
  Unix it fails loudly, naming the mode, if any credential file it can find is
  readable by anyone else -- a file can arrive from a backup, a `cp -p`, or
  another machine with permissions nobody asked for. On Windows it reports
  that it cannot check rather than implying it did.
- The run log (`~/.ccred/logs/ccred.jsonl`) holds decisions and day counts
  only, and is created `0600`. It never records a rendered error message,
  because a message can echo its input.
- On macOS, Claude Code normally keeps credentials in the Keychain. `ccred`
  does not read the Keychain yet, so on that platform it only sees the
  plaintext fallback file. This is a known gap, not a claim of support.

## Design rules

These are enforced in code and in CI, not merely intended.

- **`Secret` has no `Display` implementation.** `println!("{}", token)` is a
  compile error rather than a runtime leak. `Debug` prints a length and a
  fingerprint.
- **`Secret::expose()` is the only route to a raw token**, and CI fails if it
  is called outside the modules whose job is handling one.
- **Error kinds are persisted, never rendered messages**, because a message can
  echo its input and its input can be a token.
- **A test runs every command down every failure path** and asserts that
  nothing token-shaped reaches stdout, stderr or on-disk metadata.
- **No write replaces valid credentials with invalid ones**, and no write moves
  a refresh window backwards.
- **Account identity is checked separately from validity.** Two different
  accounts can both be valid with advancing windows, so the window rule cannot
  catch a cross-account write.

## What `ccred` does not do

It never contacts an OAuth endpoint. Keeping profiles alive works by running
the real `claude` binary against a profile's own credential store, so the
token exchange happens through Claude Code's code path, lock and client
identity. Re-implementing it would risk single-use refresh token replay
detection, and unrecognised refresh traffic from a server address has been
reported to end in a block that only a manual login clears.

## Scope

Anyone who can already run code as your user can read these files, exactly as
they can read Claude Code's own. `ccred` does not defend against that and does
not claim to; it defends against its own mistakes losing your credentials.
