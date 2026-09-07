# Publishing

The release pipeline is wired and runs on a `v*` tag. Four things still need a
human with account access, because none of them can be created through an API.

## 1. Homebrew tap token

The release workflow pushes a formula into `Zigecek/homebrew-ccred`, which the
default `GITHUB_TOKEN` cannot reach because it is a different repository.

Create a fine-grained personal access token with **Contents: read and write**
on `Zigecek/homebrew-ccred` only, then:

```sh
gh secret set HOMEBREW_TAP_TOKEN --repo Zigecek/ccred
```

## 2. crates.io trusted publishing

Publishing uses OIDC rather than a stored token, so there is no long-lived
credential in the repository to leak.

On <https://crates.io/settings/tokens> → Trusted Publishing, add a publisher:

| Field | Value |
|---|---|
| Repository owner | `Zigecek` |
| Repository name | `ccred` |
| Workflow filename | `release.yml` |
| Environment | `crates-io` |

**Bootstrap:** a trusted publisher can only be configured for a crate that
exists. If crates.io refuses because `ccred` is unpublished, do one manual
`cargo publish` with a scoped API token, revoke that token immediately, then
configure trusted publishing for every release after it.

## 3. npm trusted publishing

Eight packages are published: `ccred` and seven `@ccred/<platform>` ones. Each
needs its own trusted publisher, pointing at `publish-npm.yml` and the `npm`
environment.

Reserve the `@ccred` organisation on npm first, so the scope cannot be taken.

The same bootstrap applies: a package must exist before a trusted publisher can
be attached to it. Plan one token-based publish per package, then switch.

## 4. AUR (optional)

Only needed for `yay -S ccred-bin`. Register an AUR account, add an SSH key,
then set three secrets:

```sh
gh secret set AUR_SSH_PRIVATE_KEY --repo Zigecek/ccred
gh secret set AUR_USERNAME --repo Zigecek/ccred
gh secret set AUR_EMAIL --repo Zigecek/ccred
```

The job skips itself while these are unset, so releases do not fail without it.

## Cutting a release

```sh
# bump the version in Cargo.toml, commit
git tag -a v0.2.0 -m "ccred 0.2.0"
git push origin v0.2.0
```

Before tagging, it is worth running `dist plan` locally. It validates the
configuration and prints exactly which artifacts will be produced, which is a
great deal cheaper than finding a mistake after seven cross-compilations. It is
how the `.tar.xz` versus `.tar.gz` mismatch in the packaging jobs was caught.

## Environments

`crates-io` and `npm` exist as GitHub environments. They currently have no
protection rules. Adding a required reviewer to each is worth doing: it puts a
deliberate pause in front of anything that ships a tool which handles other
people's credentials.
