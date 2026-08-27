#!/usr/bin/env bash
# Build distribution packages locally, without CI.
#
#   ./packaging/build-local.sh deb      native (needs dpkg-deb)
#   ./packaging/build-local.sh rpm      in a fedora container (needs docker)
#   ./packaging/build-local.sh arch     in an archlinux container (needs docker)
#   ./packaging/build-local.sh all
#
# Output lands in dist/.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
APP_ID="io.github.dipakmdhrm.CaptureToSearch"
DIST="$HERE/dist"

# Single source of truth for the version: the workspace manifest.
VERSION="$(grep -m1 '^version' "$HERE/Cargo.toml" | cut -d'"' -f2)"
ARCH="$(dpkg --print-architecture 2>/dev/null || echo amd64)"

mkdir -p "$DIST"

# Container builds run as root and leave root-owned files in the bind mount,
# which the invoking user then cannot delete. Hand them back afterwards - and
# from a trap, so a failed build does not strand them either.
reclaim() {
  local dir="$1"
  [ -d "$dir" ] || return 0
  docker run --rm -v "$dir:/reclaim" alpine:latest \
    chown -R "$(id -u):$(id -g)" /reclaim >/dev/null 2>&1 || true
}

build_release() {
  echo "==> cargo build --release --workspace"
  ( cd "$HERE" && cargo build --release --workspace --locked )
}

build_deb() {
  command -v dpkg-deb >/dev/null || { echo "dpkg-deb not found"; return 1; }
  build_release
  local staging
  staging="$(mktemp -d)"
  trap 'rm -rf "$staging"' RETURN

  "$HERE/packaging/stage-tree.sh" "$staging"
  mkdir -p "$staging/DEBIAN"
  for f in postinst prerm postrm; do
    install -m0755 "$HERE/packaging/deb/$f" "$staging/DEBIAN/$f"
  done
  sed -e "s/@VERSION@/${VERSION}/" -e "s/@ARCH@/${ARCH}/" \
    "$HERE/packaging/deb/control.template" > "$staging/DEBIAN/control"

  dpkg-deb --root-owner-group --build "$staging" \
    "$DIST/capture-to-search_${VERSION}_${ARCH}.deb"
  echo "==> $DIST/capture-to-search_${VERSION}_${ARCH}.deb"
}

# Delegates to the shared script so the local build and CI package exactly the
# same set of files.
source_tarball() {
  "$HERE/packaging/source-tarball.sh" "$VERSION" "$1"
}

build_rpm() {
  command -v docker >/dev/null || { echo "docker not found"; return 1; }
  local work="$DIST/rpm-build"
  reclaim "$work"
  rm -rf "$work"; mkdir -p "$work/SOURCES" "$work/SPECS"
  trap 'reclaim "$DIST/rpm-build"' RETURN
  source_tarball "$work/SOURCES/capture-to-search-${VERSION}.tar.gz"
  sed -e "s/@VERSION@/${VERSION}/g" \
      -e "s/@CHANGELOG_DATE@/$(LC_ALL=C date '+%a %b %d %Y')/g" \
      "$HERE/packaging/rpm/capture-to-search.spec" > "$work/SPECS/capture-to-search.spec"

  docker run --rm -v "$work:/work" -w /work fedora:latest bash -c '
    set -e
    dnf install -q -y rpm-build cargo rust gcc gtk4-devel libadwaita-devel pkgconf-pkg-config >/dev/null
    rpmbuild --define "_topdir /work" -bb SPECS/capture-to-search.spec
  '
  find "$work/RPMS" -name '*.rpm' -exec cp {} "$DIST/" \;
  echo "==> $(find "$DIST" -maxdepth 1 -name '*.rpm' | tr '\n' ' ')"
}

build_arch() {
  command -v docker >/dev/null || { echo "docker not found"; return 1; }
  local work="$DIST/arch-build"
  reclaim "$work"
  rm -rf "$work"; mkdir -p "$work"
  trap 'reclaim "$DIST/arch-build"' RETURN
  source_tarball "$work/capture-to-search-${VERSION}.tar.gz"
  sed "s/@VERSION@/${VERSION}/g" "$HERE/packaging/arch/PKGBUILD" > "$work/PKGBUILD"
  cp "$HERE/packaging/arch/capture-to-search.install" "$work/"

  # makepkg refuses to run as root, so build as an unprivileged user.
  docker run --rm -v "$work:/work" archlinux:latest bash -c '
    set -e
    pacman -Syu --noconfirm --needed base-devel cargo gtk4 libadwaita >/dev/null
    useradd -m builder
    cp -r /work /home/builder/build && chown -R builder:builder /home/builder/build
    su builder -c "cd /home/builder/build && makepkg -sf --noconfirm --skipchecksums"
    cp /home/builder/build/*.pkg.tar.zst /work/
  '
  cp "$work"/*.pkg.tar.zst "$DIST/" 2>/dev/null || true
  echo "==> $(find "$DIST" -maxdepth 1 -name '*.pkg.tar.zst' | tr '\n' ' ')"
}

case "${1:-all}" in
  deb)  build_deb ;;
  rpm)  build_rpm ;;
  arch) build_arch ;;
  all)  build_deb; build_rpm; build_arch ;;
  *)    echo "usage: $0 [deb|rpm|arch|all]"; exit 2 ;;
esac
