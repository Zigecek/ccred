# npm packaging

`ccred` is a Rust binary, but it publishes to npm because that is the lowest
friction way to get a CLI onto Windows, and `npx ccred` needs no install at all.

## Layout

- `ccred/` is the user-facing package. It contains only `bin/ccred.js`, which
  resolves and execs the right platform package.
- `@ccred/<platform>-<arch>` packages carry one binary each. They are generated
  in CI from the release artifacts by `scripts/gen-npm.mjs`; they are not
  committed, because a binary in git is a binary nobody reviews.

## Why not a postinstall downloader

cargo-dist can generate an npm installer that fetches the binary during
`npm install`. That is the wrong shape for this tool:

- it fails under `npm ci --ignore-scripts`, which is increasingly the default;
- it cannot install offline or from a cache;
- and npm's provenance attestation would then cover the fetcher, not the binary
  that ends up running.

Per-platform packages resolved by npm itself avoid all three, at the cost of
publishing eight packages instead of one.

## Publishing order

Platform packages first, the root package last. The root's
`optionalDependencies` name exact versions, so publishing it first leaves a
window where it resolves to versions that do not exist yet.

Use npm Trusted Publishing (OIDC from GitHub Actions) rather than a long-lived
token. Note that a package must exist before a trusted publisher can be
configured for it, so the very first publish of each package needs a granular
token that is revoked immediately afterwards.
