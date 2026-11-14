#!/usr/bin/env bash
# Build the Flatpak bundle for Unofficial Muse for Linux.
#
# The Rust build inside the sandbox is offline by Flatpak's rules,
# so the crates are vendored first (flatpak/vendor/, regenerated
# from Cargo.lock on every run and git-ignored), then flatpak-
# builder builds the manifest and exports a single-file bundle to
# dist/.
#
# Requires: cargo, flatpak-builder, and network access for the
# one-time runtime/SDK download from Flathub.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/flatpak/org.kythos.UnofficialMuseForLinux.json"
BUILD_DIR="$ROOT/flatpak/build-dir"
BUNDLE="$ROOT/dist/unofficial-muse-for-linux-x86_64.flatpak"

cd "$ROOT"

printf 'Vendoring crates from Cargo.lock...\n'
rm -rf "$ROOT/flatpak/vendor"
cargo vendor "$ROOT/flatpak/vendor" >/dev/null

flatpak-builder --force-clean --install-deps-from=flathub \
    "$BUILD_DIR" "$MANIFEST"

mkdir -p "$ROOT/dist"
flatpak-builder --repo="$ROOT/flatpak/repo" --force-clean \
    "$BUILD_DIR" "$MANIFEST"
flatpak build-bundle "$ROOT/flatpak/repo" "$BUNDLE" \
    org.kythos.UnofficialMuseForLinux

printf 'Built: %s\n' "$BUNDLE"
