# Project conventions for ccred

## Language: English, everywhere

This is a public package published to crates.io, npm, AUR and Homebrew.
Everything readable by a contributor is written in English:

- code, identifiers, comments and doc comments
- error messages and all CLI output
- test names and assertion messages
- commit messages, README, CHANGELOG, issues and PRs

**This overrides the global rule that commit messages are written in Czech.**
That rule stands for every other project; it does not apply here, because this
repository is read by people who do not speak Czech.

## Source files are ASCII

No typographic characters in source: write `--` not an em dash, `...` not an
ellipsis, `->` not an arrow. Terminals and editors on Windows still mangle
UTF-8 punctuation often enough that it is not worth the risk in a tool people
run from a console.

## Never print a secret

`Secret` deliberately does not implement `Display`, so `println!("{}", token)`
is a compile error. `Debug` is redacted. `Secret::expose()` is the only way to
the raw value, and there are exactly two call sites: `redact.rs`, where the
type lives, and `validate.rs`, which has to look at the bytes to judge them.
CI fails the build if a third appears.

Note that `claude_cli` is not on that list and does not need to be. Refreshing
points Claude Code at a credential directory; this program never handles the
token itself.

Persist error *kinds*, never rendered error messages: a message can echo its
input, and that input can be a token.

## Safety invariants are tests, not intentions

This tool exists because its bash predecessor destroyed a profile. Two bugs did
it, and both have a named regression test. Do not weaken either:

- `v1_regression_blank_and_bogus_tokens_are_rejected` -- `jq -e` treats an
  empty string as truthy, so a logged-out state passed validation. Emptiness is
  checked first and explicitly.
- `assert_safe_replacement` -- an invalid state must never overwrite a valid
  one, and the refresh-token window must never regress.

A third gap is documented by
`window_gate_alone_does_not_catch_a_different_account`: the window check is not
an identity check. Two different accounts can both be valid with advancing
windows. **Callers must verify account identity separately** -- a live
near-miss happened exactly this way.

## Interoperating with Claude Code

We read and write files that belong to Claude Code, so:

- **Patch, never regenerate.** `.claude.json` holds ~68 top-level keys of
  unrelated state. `.credentials.json` has an `organizationUuid` sibling next
  to `claudeAiOauth` that a naive struct silently drops.
- **Round-trips are checked at runtime**, not just in tests: `assert_lossless`
  compares a rewrite against the bytes read and refuses the write if any key
  would disappear.
- **`float_roundtrip` on serde_json is load-bearing.** Without it the parser
  is a unit in the last place off for some inputs, so patching
  `.claude.json` moved `lastCost` from `248.06863250000006` to
  `248.0686325000001` -- a different double, in a running total we never
  touched. Do not drop the feature to save the parse cost; there is nothing
  here where that cost is measurable.
- **Write each file in its own shape.** `.claude.json` is pretty-printed with
  two spaces; `.credentials.json` is one compact line with no trailing
  newline. Both were measured on a live install, and both are what ccred
  writes back. Either shape parses, so this is about leaving someone's files
  looking the way they found them.
- **Take the same lock Claude Code takes** (`<storageDir>/.storage-write`,
  `proper-lockfile` semantics, 15s stale) so the two mutually exclude.
- **Never set `CLAUDE_CODE_OAUTH_TOKEN`** in the environment of a spawned
  `claude`. It triggers a plaintext write which deletes the macOS Keychain
  item.

  Measured again on Windows with 2.1.274: `claude auth status` under that
  variable reports `authMethod: "oauth_token"` and writes no
  `.credentials.json` at all, only a fresh `.claude.json` of machine and
  migration state. So the damage is the Keychain's and the rule stands for
  macOS, where it cannot be checked from here. Claude Desktop does set the
  variable for the sessions it starts, against the user's own
  `CLAUDE_CONFIG_DIR` -- which is why `proc` treats those sessions as the
  Desktop's rather than the store's.
- **On macOS, shell out to `/usr/bin/security`**, never a native Keychain API.
  The item's ACL trusts `security`; a native read from a scheduled job returns
  `errSecInteractionNotAllowed` and fails silently forever.

## Interoperating with Claude Desktop

Everything here was read off a live install and out of the shipped app, not
guessed. When it needs checking again, the app's own bundle is at
`C:\Program Files\WindowsApps\Claude_<version>_x64__<publisher>\app\resources\app.asar`
and the constants are in plain text inside it.

- **The login is the directory, and the token in it is encrypted.** There is
  nothing to copy aside and nothing to put back, so a switch moves the whole
  data directory and `rm --purge` is the only way to delete one.
- **Why the whole directory.** The token is `oauth:tokenCache` in
  `config.json`, encrypted with Electron's `safeStorage`; the key it is
  encrypted with is `os_crypt.encrypted_key` in `Local State`, in the same
  directory, wrapped by DPAPI for the Windows user. Move the pair and the
  login works; copy `config.json` alone and it is a blob nothing can read.
  That is the reason the unit is a directory and not a file, and it is not
  an optimisation waiting to happen.
- **Two places hold local sessions**, both `<dir>/<account>/<organization>/`
  with one small JSON file per session: `claude-code-sessions` and
  `local-agent-mode-sessions`. The sidebar is drawn from both -- in the
  bundle they are scanned side by side -- so anything that shares one has to
  share the other, and the same for the rescue after a sign-out.
- **A session entry is a file with a `sessionId` in it.** The build calls
  them `local_<id>.json`, and `SidebarStore` keys them by the name the app
  gave so they go back under it, but the name is not what makes one an
  entry. The same directory holds `scheduled-tasks.json` and
  `archived-sessions.idx`, which are the app's own.
- **Chats with Claude are not local.** They belong to the account on
  Anthropic's side and the app reads them from there (its cache is an
  IndexedDB under `IndexedDB/https_claude.ai_0.indexeddb.leveldb`). No
  amount of moving files shares a conversation between two accounts, and
  the README says so in the first sentence of that section, because the
  first person to try it expected otherwise.
- **The sidebar groups are one key**, `dframe-group-scopes` under
  `preferences.epitaxyPrefs` in `claude_desktop_config.json`, whose entries
  are keyed `<account>/<organization>`. Only keys of that shape are read,
  and only that shape is ever written, so a build that keys them some other
  way makes this do nothing rather than write junk.
- **Windows has two layouts and two package families.** The classic
  `%APPDATA%\Claude`, and the MSIX one redirected to
  `%LOCALAPPDATA%\Packages\<family>\LocalCache\Roaming\Claude`. Store and
  sideloaded installs are both MSIX -- `SignatureKind` tells them apart --
  and their families are named differently: `Claude_<hash>` for the
  sideloaded one, `AnthropicPBC.Claude_<hash>` for the Store's, both in the
  app's bundle. Matching a `Claude_` prefix therefore missed every Store
  install; what is matched is the identity before the hash, `Claude` or
  something ending in `.Claude`. The `-3p` build uses `Claude-3p` for the
  same directories. `paths` picks whichever candidate has the more recently
  written `config.json`, and `doctor` names the one it picked. One more
  place: Chromium cannot keep a profile on a UNC path, so a `%APPDATA%` on a
  file share sends the data to `%LOCALAPPDATA%\Claude-Data`.
- **Anything that cannot be done is said, not raised.** A sidebar that could
  not be written, groups with no config to go into, a carry that cannot
  cross a file system: by then the directories have moved, so the switch has
  happened, and an error would send someone looking for a login that is
  where it should be. Every one of those is a warning with the reason in it.

## Saving credentials outranks everything it triggers

`save` registers the refresh schedule on the save that creates a second
profile. That side effect must never be able to fail the save: by the time it
runs the credentials are already stored, and returning an error would trade the
operation that matters for one that does not. Every failure inside
`auto_schedule` becomes a reported `ScheduleSetup::Failed`, never a `?`.

Two rules keep it honest:

- **Decide purely, act separately.** `should_auto_schedule` takes the state as
  an argument, so the policy is tested without a scheduler. A test that read
  the host's real one would assert on whichever machine ran it.
- **The suite must not touch the platform scheduler.** `CCRED_NO_AUTO_SCHEDULE`
  exists for provisioning and is set in `tests/cli.rs` for exactly this reason.

## Output is rendered, never printed inline

Operations return data; `src/ui/render.rs` turns it into text. That split is
what keeps "no output may ever contain a token" auditable in one file, so a
`println!` inside an operation is a bug even when the string is harmless.

Three rules hold in `src/ui`:

- **Six roles, not six colours.** `LABEL`, `VALUE`, `NAME`, `OK`, `WARN` and
  `ERR`, plus `ACCENT` for the active profile and `MUTED` for context.
  Anything that seems to need a new colour needs one of these instead.
- **Measure unstyled, pad outside the styled run.** A column width taken from
  text that already carries escape sequences is wrong by the length of those
  sequences, and padding inside the run puts blanks where `trim_end` cannot
  reach them. `table_columns_line_up_regardless_of_styling` pins this.
- **Two glyph sets, and the Unicode one is opt-in.** A terminal not known to
  handle UTF-8 gets ASCII, because a tool people run when something is already
  broken must not add mojibake to the problem. Source stays ASCII: the Unicode
  set is written as escapes.

Colour itself is `anstream`'s problem, not ours. It strips escapes when stdout
is not a terminal -- which is what keeps systemd, launchd and Task Scheduler
logs clean -- and it honours `NO_COLOR` and `CLICOLOR_FORCE`. Never branch on
colour support by hand.

## No panic in the code that runs unattended

A panic in a scheduled refresh is a slot that reports nothing and a profile
that quietly goes stale -- with no message anywhere a person looks, which is
the failure this program exists to prevent. `scripts/no-panics.sh` refuses
`unwrap`, `expect`, `panic!`, `unreachable!` and `todo!` anywhere before
`#[cfg(test)]`, and CI runs it. Handle the case and say what happened
instead; a proof that lives two checks away is one a later edit can quietly
break.

Tests are exempt, and should be: a test that cannot unwrap says less when it
fails.

## Before committing

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Code behind `#[cfg(unix)]`, `target_os = "linux"` or `"macos"` is not even
compiled on a Windows machine, and the reverse holds on the others. Lint the
other platforms too before pushing -- `cargo clippy` needs only the target's
standard library, not a linker:

```sh
rustup target add x86_64-unknown-linux-gnu x86_64-apple-darwin x86_64-pc-windows-msvc
cargo clippy --target x86_64-unknown-linux-gnu --all-targets --all-features -- -D warnings
cargo clippy --target x86_64-apple-darwin --all-targets --all-features -- -D warnings
```

Shell heredocs and inline scripts mangle backslashes -- escapes such as a
newline, doubled backslashes, Windows paths -- more often than not. Edit Rust
source with an editor, or with a script kept in a file.
