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
#
# Assembled OUTSIDE the working tree, then moved into place. Writing it directly
# to $OUT races when $OUT is somewhere git would list: `git ls-files` and `tar`
# run concurrently in the pipeline, so if git reaches the output path after tar
# has created it, tar tries to archive the tarball into itself and dies with
# "file changed as we read it". That is exactly what happened in CI, which
# writes into an untracked rpmbuild/SOURCES - and only on one architecture,
# because it is a race.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
( cd "$HERE" && git ls-files -co --exclude-standard -z \
    | tar --null -T - -czf "$STAGE/source.tar.gz" --transform "s,^,${PREFIX}/," )
mkdir -p "$(dirname "$OUT")"
mv "$STAGE/source.tar.gz" "$OUT"

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

# And nothing that should never be in a source tarball: a previous build's
# output, or the tarball itself. Both bloat the archive and the second one is
# the self-inclusion race above.
if grep -qE '(^|/)(rpmbuild|arch-build|target|dist)/' <<<"$listing"; then
  echo "source tarball contains build output:" >&2
  grep -E '(^|/)(rpmbuild|arch-build|target|dist)/' <<<"$listing" | head -5 >&2
  exit 1
fi
if grep -qE '\.(tar\.gz|tgz)$' <<<"$listing"; then
  echo "source tarball contains an archive (is it including itself?):" >&2
  grep -E '\.(tar\.gz|tgz)$' <<<"$listing" | head -5 >&2
  exit 1
fi
