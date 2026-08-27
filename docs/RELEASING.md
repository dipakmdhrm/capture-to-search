# Releasing

Releases are automatic. Merging a pull request to `main` bumps the version,
stamps the changelog, tags, builds every package, and publishes a GitHub
Release. Nothing else is required.

- The bump comes from a `release:*` label on the merged pull request and
  defaults to a patch. Use `release:minor` or `release:major` to change it, and
  `release:skip` to merge without releasing.
- The version is computed by `packaging/next-version.sh`, from whichever is
  higher: the newest `v*` tag, or the version `Cargo.toml` already declares.
  Taking the maximum matters in both directions - a tag-only base would release
  `0.0.1` over a manifest already reading `0.1.0`, and a manifest-only base
  would reissue a version that is already tagged. Check what a merge would
  produce before merging:

  ```bash
  ./packaging/next-version.sh patch
  ```
- A merge that touches only `.github/` or `*.md` ships nothing user-facing and
  does not release.
- To release by hand instead, push a tag: `git tag v0.2.0 && git push origin v0.2.0`.

## The apt repository

On top of the GitHub Release, each release publishes a signed apt repository to
GitHub Pages, so Debian and Ubuntu users get updates through `apt upgrade`
instead of downloading a `.deb` each time. The installed package subscribes to
it itself, in its `postinst`.

**Publishing never blocks a release.** The job is gated on `APT_SIGNING_KEY`, so
without that secret it logs that it skipped and the rest of the pipeline runs
unchanged, and a failure inside it does not stop the GitHub Release. Losing an
apt repository for one version is recoverable; losing the packages is not.

### Setup

Already done, and nothing here needs repeating:

- `APT_SIGNING_KEY` and `APT_SIGNING_KEY_PASSPHRASE` hold a dedicated RSA
  4096 signing key that does not expire. An expiring key would silently
  start failing `apt update` on every user's machine the day it lapsed.
- The `gh-pages` branch exists and GitHub Pages serves it at
  <https://dipakmdhrm.github.io/capture-to-search/>. Its `.nojekyll` marker
  keeps Pages from processing the tree, which would otherwise risk rewriting
  or dropping paths an apt client needs.

The only reason to touch any of it again is rotating the key, below.

### What users then do

```bash
sudo apt install ./capture-to-search_<version>_amd64.deb
```

The `postinst` fetches the repository key, writes
`/etc/apt/sources.list.d/capture-to-search.list`, and runs `apt-get update`, so
later releases arrive through `apt upgrade`. Removing the package removes both
files again.

If the repository is unreachable - not published yet, or the machine is offline
- the install still succeeds; the package simply will not auto-update.

### Rotating or revoking the key

Clients pin the key in `/etc/apt/keyrings/capture-to-search.gpg`, so a rotation
makes `apt update` fail for everyone who installed under the old key until they
reinstall the package. Announce it rather than doing it quietly.

```bash
gpg --quick-generate-key "Capture to Search <dipakmdhrm@gmail.com>" rsa4096 sign never
gpg --armor --export-secret-keys "Capture to Search" > /tmp/apt-signing-key.asc
gh secret set APT_SIGNING_KEY < /tmp/apt-signing-key.asc
gh secret set APT_SIGNING_KEY_PASSPHRASE
shred -u /tmp/apt-signing-key.asc
```

Then cut a release, which republishes `key.gpg` and re-signs the repository.
Confirm the export really is the private half before uploading it - the file
must begin with `-----BEGIN PGP PRIVATE KEY BLOCK-----`, and exporting the
public key by mistake fails only later, during a release.

## Why the repository is signed the way it is

`reprepro` can sign `Release` itself through gpgme, but that fails with an
opaque "General error" on GitHub's runners. The workflow therefore lets
`reprepro` build the repository unsigned and signs `InRelease` and
`Release.gpg` with direct `gpg` calls, feeding the passphrase over loopback
pinentry because the runner is headless. Those two files are what apt clients
actually fetch.
