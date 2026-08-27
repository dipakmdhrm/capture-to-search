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

stage_shared() {
  # Lay out the parts every format installs identically.
  local root="$1"
  install -Dm0755 "$HERE/target/release/capture-to-searchd" "$root/usr/bin/capture-to-searchd"
  install -Dm0755 "$HERE/target/release/capture-to-search-gui" "$root/usr/bin/capture-to-search-gui"
  install -Dm0644 "$HERE/data/applications/$APP_ID.desktop" \
    "$root/usr/share/applications/$APP_ID.desktop"
  install -Dm0644 "$HERE/data/metainfo/$APP_ID.metainfo.xml" \
    "$root/usr/share/metainfo/$APP_ID.metainfo.xml"
  for s in 16 24 32 48 64 128 256 512; do
    install -Dm0644 "$HERE/data/icons/hicolor/${s}x${s}/apps/$APP_ID.png" \
      "$root/usr/share/icons/hicolor/${s}x${s}/apps/$APP_ID.png"
  done
  install -Dm0644 "$HERE/data/icons/hicolor/scalable/apps/$APP_ID.svg" \
    "$root/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
  install -Dm0644 "$HERE/data/icons/hicolor/scalable/apps/$APP_ID-symbolic.svg" \
    "$root/usr/share/icons/hicolor/scalable/apps/$APP_ID-symbolic.svg"
}

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

  stage_shared "$staging"
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

# Build a source tarball named the way rpm and makepkg expect.
#
# Archives the WORKING TREE, not HEAD. A local build script that silently
# packaged the last commit would omit files you just added - which is exactly
# how a missing data/metainfo file turned into a confusing mid-build
# "cannot stat" from rpmbuild. `git ls-files -co --exclude-standard` gives
# tracked plus untracked files while still honouring .gitignore, so target/ and
# dist/ stay out.
source_tarball() {
  local out="$1"
  local prefix="capture-to-search-${VERSION}"
  ( cd "$HERE" && git ls-files -co --exclude-standard -z \
      | tar --null -T - -czf "$out" --transform "s,^,${prefix}/," )

  # Fail loudly here rather than 10 minutes into a container build.
  local required=(
    "${prefix}/Cargo.toml"
    "${prefix}/data/applications/${APP_ID}.desktop"
    "${prefix}/data/metainfo/${APP_ID}.metainfo.xml"
    "${prefix}/LICENSE"
  )
  local listing
  listing="$(tar -tzf "$out")"
  for f in "${required[@]}"; do
    grep -qxF "$f" <<<"$listing" || {
      echo "source tarball is missing $f" >&2
      return 1
    }
  done
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
