#!/usr/bin/env bash
# Compute the next release version.
#
#   packaging/next-version.sh <major|minor|patch> [repo-root]
#
# The base is whichever is higher: the newest `v*` tag, or the version the
# workspace manifest already claims. Tags alone are not enough. This repository
# declared 0.1.0 in Cargo.toml and CHANGELOG.md before any tag existed, so a
# tag-only base would have computed 0.0.1 for the first release - rewriting the
# manifest backwards and stamping a 0.0.1 changelog section above the 0.1.0 one.
#
# Taking the maximum also covers the reverse: someone hand-editing the manifest
# ahead of the tags, which would otherwise produce a version that already exists.
set -euo pipefail

BUMP="${1:?usage: next-version.sh <major|minor|patch> [repo-root]}"
ROOT="${2:-$(cd "$(dirname "$0")/.." && pwd)}"

tag="$(git -C "$ROOT" tag -l 'v*' --sort=-v:refname | head -1)"
tag="${tag#v}"
tag="${tag:-0.0.0}"

manifest="$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' \
  "$ROOT/Cargo.toml" | head -1)"
manifest="${manifest:-0.0.0}"

# sort -V puts the higher semantic version last.
base="$(printf '%s\n%s\n' "$tag" "$manifest" | sort -V | tail -1)"

IFS=. read -r major minor patch <<< "$base"
case "$BUMP" in
  major) major=$((major + 1)); minor=0; patch=0 ;;
  minor) minor=$((minor + 1)); patch=0 ;;
  patch) patch=$((patch + 1)) ;;
  *) echo "unknown bump '$BUMP' (expected major, minor or patch)" >&2; exit 1 ;;
esac

echo "${major}.${minor}.${patch}"
