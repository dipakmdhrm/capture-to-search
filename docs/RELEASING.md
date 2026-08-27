# Releasing

Releases are automatic. Merging a pull request to `main` bumps the version,
stamps the changelog, tags, builds every package, and publishes a GitHub
Release. Nothing else is required.

- The bump comes from a `release:*` label on the merged pull request and
  defaults to a patch. Use `release:minor` or `release:major` to change it, and
  `release:skip` to merge without releasing.
- A merge that touches only `.github/` or `*.md` ships nothing user-facing and
  does not release.
- To release by hand instead, push a tag: `git tag v0.2.0 && git push origin v0.2.0`.

## Optional: the apt repository

On top of the GitHub Release, the pipeline can publish a signed apt repository
to GitHub Pages, so Debian and Ubuntu users get updates through `apt upgrade`
instead of downloading a `.deb` each time. The installed package registers this
repository itself, in its `postinst`.

**This is entirely optional.** Until it is set up, the publishing job logs that
it was skipped and everything else - packages, GitHub Release - works normally.
It never blocks a release.

### One-time setup

**1. Create a signing key.** Use a dedicated key, not your personal one.

```bash
gpg --quick-generate-key "Capture to Search <dipakmdhrm@gmail.com>" rsa4096 sign never
gpg --armor --export-secret-keys "Capture to Search" > /tmp/apt-signing-key.asc
```

**2. Add the repository secrets.**

```bash
gh secret set APT_SIGNING_KEY < /tmp/apt-signing-key.asc
gh secret set APT_SIGNING_KEY_PASSPHRASE          # empty if the key has none
shred -u /tmp/apt-signing-key.asc                 # do not leave the key on disk
```

Both live in **Settings > Secrets and variables > Actions**. The private key
never appears in a build log: it is imported into a throwaway keyring on the
runner and used only to sign the repository's `Release` file.

**3. Enable GitHub Pages** on the `gh-pages` branch, at **Settings > Pages**.
The branch does not exist yet; the first release creates it, so either publish a
release first and then enable Pages, or create an empty `gh-pages` branch by
hand.

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

Replace the `APT_SIGNING_KEY` secret and cut a release. Clients that already
have the old key pinned in `/etc/apt/keyrings/capture-to-search.gpg` will report
a signature failure on `apt update` until they reinstall the package, so
announce a rotation rather than doing it quietly.

## Why the repository is signed the way it is

`reprepro` can sign `Release` itself through gpgme, but that fails with an
opaque "General error" on GitHub's runners. The workflow therefore lets
`reprepro` build the repository unsigned and signs `InRelease` and
`Release.gpg` with direct `gpg` calls, feeding the passphrase over loopback
pinentry because the runner is headless. Those two files are what apt clients
actually fetch.
