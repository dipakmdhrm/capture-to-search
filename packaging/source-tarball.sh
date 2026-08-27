#!/usr/bin/env bash
# Create the source tarball the rpm and Arch builds unpack and compile.
#
# Single source of truth for what goes in, shared by packaging/build-local.sh
# and .github/workflows/build-packages.yml.
#
#   packaging/source-tarball.sh <version> <output.tar.gz>
set -euo pipefail

VERSION="${1:?usage: source-tarball.sh <version> <output.tar.gz>}"
OUT="${2:?usage: source-tarball.sh <version> <output.tar.gz>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
APP_ID="io.github.dipakmdhrm.CaptureToSearch"
PREFIX="capture-to-search-${VERSION}"

# Absolute, since we cd below.
case "$OUT" in /*) ;; *) OUT="$PWD/$OUT" ;; esac

# The WORKING TREE, not HEAD. A tarball built from HEAD silently omits files
# that have not been committed yet, which surfaces much later as an unrelated
# "cannot stat" from rpmbuild or makepkg. `-co --exclude-standard` takes tracked
# plus untracked files while still honouring .gitignore, so target/ and dist/
# stay out.
( cd "$HERE" && git ls-files -co --exclude-standard -z \
    | tar --null -T - -czf "$OUT" --transform "s,^,${PREFIX}/," )

# Fail here rather than ten minutes into a container build. Every entry below is
# something a package definition installs.
required=(
  "${PREFIX}/Cargo.toml"
  "${PREFIX}/Cargo.lock"
  "${PREFIX}/LICENSE"
  "${PREFIX}/README.md"
  "${PREFIX}/CHANGELOG.md"
  "${PREFIX}/data/applications/${APP_ID}.desktop"
  "${PREFIX}/data/metainfo/${APP_ID}.metainfo.xml"
  "${PREFIX}/data/icons/hicolor/scalable/apps/${APP_ID}-symbolic.svg"
)
listing="$(tar -tzf "$OUT")"
for f in "${required[@]}"; do
  grep -qxF "$f" <<<"$listing" || { echo "source tarball is missing $f" >&2; exit 1; }
done
