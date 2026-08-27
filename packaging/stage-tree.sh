#!/usr/bin/env bash
# Lay out the installed file tree under a target root.
#
# Single source of truth for what a package contains, shared by
# packaging/build-local.sh and the .deb job in
# .github/workflows/build-packages.yml. Duplicating this between the local
# script and CI is how a file ends up in one package and not the other - the
# symbolic tray icon was already missed once that way.
#
#   packaging/stage-tree.sh <target-root> [release-dir]
set -euo pipefail

ROOT="${1:?usage: stage-tree.sh <target-root> [release-dir]}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REL="${2:-$HERE/target/release}"
APP_ID="io.github.dipakmdhrm.CaptureToSearch"

install -Dm0755 "$REL/capture-to-searchd"    "$ROOT/usr/bin/capture-to-searchd"
install -Dm0755 "$REL/capture-to-search-gui" "$ROOT/usr/bin/capture-to-search-gui"

install -Dm0644 "$HERE/data/applications/$APP_ID.desktop" \
  "$ROOT/usr/share/applications/$APP_ID.desktop"
install -Dm0644 "$HERE/data/metainfo/$APP_ID.metainfo.xml" \
  "$ROOT/usr/share/metainfo/$APP_ID.metainfo.xml"

for s in 16 24 32 48 64 128 256 512; do
  install -Dm0644 "$HERE/data/icons/hicolor/${s}x${s}/apps/$APP_ID.png" \
    "$ROOT/usr/share/icons/hicolor/${s}x${s}/apps/$APP_ID.png"
done
# Both SVGs. The symbolic variant is the one every glob written the obvious way
# misses, because it is the only asset not named <appid>.<ext>.
install -Dm0644 "$HERE/data/icons/hicolor/scalable/apps/$APP_ID.svg" \
  "$ROOT/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm0644 "$HERE/data/icons/hicolor/scalable/apps/$APP_ID-symbolic.svg" \
  "$ROOT/usr/share/icons/hicolor/scalable/apps/$APP_ID-symbolic.svg"
