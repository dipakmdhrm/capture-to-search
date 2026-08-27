#!/usr/bin/env bash
# Remove a per-user install made by ./install.sh.
set -euo pipefail

APP_ID="io.github.dipakmdhrm.CaptureToSearch"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
HICOLOR="$DATA_DIR/icons/hicolor"
APPS_DIR="$DATA_DIR/applications"
METAINFO_DIR="$DATA_DIR/metainfo"
PNG_SIZES=(16 24 32 48 64 128 256 512)

echo "==> Stopping any running daemon and window"
pkill -x capture-to-searchd 2>/dev/null || true
pkill -x capture-to-search-gui 2>/dev/null || true

echo "==> Removing binaries"
rm -f "$BIN_DIR/capture-to-searchd" "$BIN_DIR/capture-to-search-gui"

echo "==> Removing icons, desktop entry, and metainfo"
rm -f "$HICOLOR/scalable/apps/$APP_ID.svg" "$HICOLOR/scalable/apps/$APP_ID-symbolic.svg"
for s in "${PNG_SIZES[@]}"; do
  rm -f "$HICOLOR/${s}x${s}/apps/$APP_ID.png"
done
rm -f "$APPS_DIR/$APP_ID.desktop" "$METAINFO_DIR/$APP_ID.metainfo.xml"

echo "==> Removing the autostart entry"
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/capture-to-search.desktop"

command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$HICOLOR" 2>/dev/null || true
command -v update-desktop-database >/dev/null && update-desktop-database -q "$APPS_DIR" 2>/dev/null || true

echo
echo "Uninstalled. Configuration is left in"
echo "  ${XDG_CONFIG_HOME:-$HOME/.config}/capture-to-search/"
echo "Remove it by hand if you want a clean slate."
