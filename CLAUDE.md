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
the raw value -- keep its call sites confined to `store`, `validate` and
`claude_cli`, and never widen that set casually.

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
- **Take the same lock Claude Code takes** (`<storageDir>/.storage-write`,
  `proper-lockfile` semantics, 15s stale) so the two mutually exclude.
- **Never set `CLAUDE_CODE_OAUTH_TOKEN`** in the environment of a spawned
  `claude`. It triggers a plaintext write which deletes the macOS Keychain
  item.
- **On macOS, shell out to `/usr/bin/security`**, never a native Keychain API.
  The item's ACL trusts `security`; a native read from a scheduled job returns
  `errSecInteractionNotAllowed` and fails silently forever.

## Before committing

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```
