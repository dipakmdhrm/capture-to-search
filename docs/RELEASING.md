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

## Optional: the apt repository

On top of the GitHub Release, the pipeline can publish a signed apt repository
to GitHub Pages, so Debian and Ubuntu users get updates through `apt upgrade`
instead of downloading a `.deb` each time. The installed package registers this
repository itself, in its `postinst`.

**This is entirely optional.** Until it is set up, the publishing job logs that
it was skipped and everything else - packages, GitHub Release - works normally.
It never blocks a release.

### One-time setup

**1. Create a signing key.** Use a dedicated key, not your personal one. Its
user ID is published in the repository, so use a name and address you are happy
to have public.

```bash
gpg --quick-generate-key "Capture to Search <dipakmdhrm@gmail.com>" rsa4096 sign never
```

gpg prompts for a passphrase. Set one: the exported key file sits on disk for a
moment in the next step, and a passphrase is what protects it if that file
leaks. Note the fingerprint it prints, or find it again with:

```bash
gpg --list-secret-keys --keyid-format=long "Capture to Search"
```

**2. Export the private key.** This is what the secret holds. gpg asks for the
passphrase again.

```bash
gpg --armor --export-secret-keys "Capture to Search" > /tmp/apt-signing-key.asc
```

Sanity-check before uploading - the file should start with a private key header
and be a few kilobytes:

```bash
head -1 /tmp/apt-signing-key.asc     # -----BEGIN PGP PRIVATE KEY BLOCK-----
```

**3. Add the repository secrets, then destroy the file.**

```bash
gh secret set APT_SIGNING_KEY < /tmp/apt-signing-key.asc
gh secret set APT_SIGNING_KEY_PASSPHRASE          # paste the passphrase
shred -u /tmp/apt-signing-key.asc                 # do not leave the key on disk
```

Both live in **Settings > Secrets and variables > Actions**. The private key
never appears in a build log: it is imported into a throwaway keyring on the
runner and used only to sign the repository's `Release` file.

**4. Enable GitHub Pages** on the `gh-pages` branch, at **Settings > Pages**.
The branch does not exist yet; the first release creates it, so either publish a
release first and then enable Pages, or create an empty `gh-pages` branch by
hand.

### Checking the key works before you rely on it

Optional, but it exercises the exact path the runner uses - import into a
throwaway keyring, sign with loopback pinentry, verify as apt would:

```bash
export GNUPGHOME=$(mktemp -d) && chmod 700 "$GNUPGHOME"
printf 'pinentry-mode loopback\n' >> "$GNUPGHOME/gpg.conf"
gpg --batch --import /path/to/apt-signing-key.asc
FPR=$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr/{print $10; exit}')

echo "Origin: test" > Release
gpg --batch --yes --pinentry-mode loopback --passphrase 'YOUR-PASSPHRASE' \
  --local-user "$FPR" -abs -o Release.gpg Release
gpg --armor --export "$FPR" | gpg --dearmor > key.gpg
gpg --no-default-keyring --keyring ./key.gpg --verify Release.gpg Release
```

The last command should print `Good signature`. Unset `GNUPGHOME` afterwards so
you are back on your real keyring.

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
