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

### Claude Desktop

The Desktop logs in on its own, and what it stores is not a file this tool
can read: the token in `config.json` is encrypted with Electron's
`safeStorage`, whose key lives in `Local State` in the same directory,
wrapped by DPAPI for the Windows user or by the login keychain elsewhere.

- **The token is never read, decrypted or copied.** A switch moves the whole
  data directory and moves it back; that is the only operation on a Desktop
  login. A parked one is therefore usable only by the user who parked it, on
  the machine they parked it on, and copying `config.json` on its own would
  produce a blob nothing can open.
- **Deleting a parked login cannot be undone**, for the same reason: there is
  no copy and no way to make one. `ccred rm <name>-desktop` refuses without
  `--purge`, and `ccred uninstall --purge` counts them separately in the plan
  it shows before asking.
- **Three things are written inside the Desktop's directory, and nothing
  else**: the session entries of its own session lists, with only the fields
  that belong to the session and never the ones bound to an account; the
  index of archived sessions, patched rather than rewritten; and one key of
  `claude_desktop_config.json`, merged into. Each is written with the Desktop
  closed, which is checked through Electron's single-instance lock -- reading
  is safe while it runs, writing is not.
- **Chats with Claude are not touched, because they are not there.** They
  belong to the account on Anthropic's side and the app reads them from
  there. Nothing local moves a conversation between two accounts.

## Design rules

These are enforced in code and in CI, not merely intended.

- **`Secret` has no `Display` implementation.** `println!("{}", token)` is a
  compile error rather than a runtime leak. `Debug` prints a length and a
  fingerprint.
- **`Secret::expose()` is the only route to a raw token**, and CI fails if it
  is called outside the modules whose job is handling one.
- **Error kinds are persisted, never rendered messages**, because a message can
  echo its input and its input can be a token.
- **Somebody else's file is patched, never regenerated.** Both applications'
  files hold state this tool does not model -- `.claude.json` has around
  seventy top-level keys -- and a rewrite from a struct drops whatever the
  struct does not know. Every write compares against the bytes that were
  read and refuses if a key would disappear.
- **A test runs every command, including the ways each one fails,** and
  asserts that nothing token-shaped reaches stdout or stderr -- nor any file
  written beside the credentials: profile metadata, the account blob, the
  active pointer, the switch journal or the run log.
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
