#!/usr/bin/env bash
set -euo pipefail

# Repack the AppImage on a host that cannot run the bundler's deploy step.
#
# scripts/build-appimage.sh is still the real recipe: inside the builder image
# (Containerfile.appimage) linuxdeploy collects the GTK 4 / WebKitGTK 6.0
# library closure out of the container's own packages, patch-webkit-path.py
# moves WebKit's absolute helper path to the bundle's staging path, and
# patchelf gives the helpers their RUNPATH. A rootless workstation has no
# system GTK 4 runtime to deploy and no container to build one in, so this
# script takes the runtime tree from a released AppImage -- the same artifact
# the developer environment already runs against -- and replaces exactly what
# this checkout owns: both binaries, the project sidecars, the icons, the
# desktop entry and the metadata. The packaging step is the same appimagetool
# invocation build-appimage.sh ends with. It is a development fallback, not a
# substitute for the container build CI publishes.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -m)" != "x86_64" ]]; then
    printf 'This script currently targets x86_64 (found %s).\n' "$(uname -m)" >&2
    exit 2
fi

APPIMAGETOOL_SHA256="e8c1169c4af66c3184d7ee110713d4be04befe671fa358e2c1591ee840553b03"
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/unofficial-muse-for-linux/appimage-tools"
APPIMAGETOOL="$CACHE_DIR/appimagetool-x86_64.AppImage"
BASELINE="${MUSE_APPIMAGE_BASELINE:-${MUSE_APPIMAGE:-$HOME/.cache/muse-appimage/squashfs-root}}"
APPDIR="$ROOT/target/AppDir-repack"

if [[ ! -d "$BASELINE/usr/lib" ]]; then
    printf 'No runtime baseline at %s.\n' "$BASELINE" >&2
    printf 'Extract a released AppImage first:\n' >&2
    printf '  ./unofficial-muse-for-linux-x86_64.AppImage --appimage-extract   # into squashfs-root\n' >&2
    printf 'or set MUSE_APPIMAGE_BASELINE to an extracted tree.\n' >&2
    exit 1
fi

mkdir -p "$CACHE_DIR"
if [[ ! -x "$APPIMAGETOOL" ]]; then
    curl --fail --location --retry 3 --output "$APPIMAGETOOL.tmp" "$APPIMAGETOOL_URL"
    printf '%s  %s\n' "$APPIMAGETOOL_SHA256" "$APPIMAGETOOL.tmp" | sha256sum --check --status
    mv "$APPIMAGETOOL.tmp" "$APPIMAGETOOL"
    chmod 755 "$APPIMAGETOOL"
elif ! printf '%s  %s\n' "$APPIMAGETOOL_SHA256" "$APPIMAGETOOL" | sha256sum --check --status; then
    printf 'The cached appimagetool does not match its pinned checksum: %s\n' "$APPIMAGETOOL" >&2
    exit 1
fi

cd "$ROOT"
# The updater compares this build's commit with the feed's. CI passes its own
# (`UMFL_BUILD_SHA`); a repack stamps HEAD, so the bundle's check answers
# against the real feed instead of claiming to be current without an identity.
export UMFL_BUILD_SHA="${UMFL_BUILD_SHA:-$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)}"
if [[ -n "$UMFL_BUILD_SHA" ]]; then
    printf 'Stamped build commit %s\n' "$UMFL_BUILD_SHA"
fi
# A repack is a new build of its commit, so it stamps its own instant:
# the updater orders builds by when they were built, and this bundle's
# place in that order is the repack's moment, not the commit's.
export UMFL_BUILD_EPOCH="${UMFL_BUILD_EPOCH:-$(date -u +%s)}"
printf 'Stamped build epoch %s\n' "$UMFL_BUILD_EPOCH"
cargo test --workspace --locked
cargo build --release --locked -p muse
cargo build --release --locked -p muse-reminder-runner
cargo build --release --locked --manifest-path apps/muse-window/Cargo.toml

rm -rf -- "$APPDIR"
cp -a "$BASELINE" "$APPDIR"

ACCOUNT_DIR="$APPDIR/usr/share/unofficial-muse-for-linux/account"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/metainfo" "$ACCOUNT_DIR"
install -m 755 "$ROOT/target/release/muse" "$APPDIR/usr/bin/muse"
install -m 755 "$ROOT/target/release/muse-reminder-runner" "$APPDIR/usr/bin/muse-reminder-runner"
# The window is GTK 3 + WebKitGTK 4.1 and runs on the host's own pair, so it is
# installed beside the app rather than deployed into the GTK 4 tree.
install -m 755 "$ROOT/apps/muse-window/target/release/muse-window" "$APPDIR/usr/bin/muse-window"
install -m 644 "$ROOT/scripts/muse_account_bridge.py" "$ACCOUNT_DIR/muse_account_bridge.py"
install -m 644 "$ROOT/scripts/muse_account_login.py" "$ACCOUNT_DIR/muse_account_login.py"
install -m 644 "$ROOT/scripts/muse_local_host.py" "$ACCOUNT_DIR/muse_local_host.py"
install -m 644 "$ROOT/scripts/muse_account_host.py" "$ACCOUNT_DIR/muse_account_host.py"
install -m 644 "$ROOT/resources/unofficial-muse-for-linux.appdata.xml" \
    "$APPDIR/usr/share/metainfo/unofficial-muse-for-linux.appdata.xml"
install -m 644 "$ROOT/resources/unofficial-muse-for-linux.desktop" \
    "$APPDIR/usr/share/applications/unofficial-muse-for-linux.desktop"
for size in 16 32 48 64 128 256 512; do
    install -Dm644 "$ROOT/resources/icons/${size}x${size}.png" \
        "$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps/unofficial-muse-for-linux.png"
done

# linuxdeploy's deploy step is what gives the app its `$ORIGIN/../lib` RUNPATH
# in the container build; a plain cargo binary has none, and the bundle's GTK 4
# and WebKitGTK 6.0 libraries are not on the host. The window is left alone on
# purpose: its libraries are the host's GTK 3 and WebKitGTK 4.1, and a RUNPATH
# into this tree would hand it the GTK 4 generation.
for binary in muse muse-reminder-runner; do
    patchelf --set-rpath '$ORIGIN/../lib' "$APPDIR/usr/bin/$binary"
done

# An extracted tree keeps the modes it was unpacked with (0700 for the
# directories a squashfs image carries), and appimagetool refuses to package a
# tree other users cannot read. The bundle is meant to be run by whoever has the
# file, so every directory gets its read/execute bits back.
chmod -R a+rX "$APPDIR"

ARTIFACT="$ROOT/target/unofficial-muse-for-linux-x86_64.AppImage"
rm -f -- "$ARTIFACT" "$ROOT"/target/*.AppImage
export APPIMAGE_EXTRACT_AND_RUN=1
ARCH=x86_64 "$APPIMAGETOOL" "$APPDIR" "$ARTIFACT"
install -Dm755 "$ARTIFACT" "$ROOT/dist/unofficial-muse-for-linux-x86_64.AppImage"
printf 'Built: %s\n' "$ROOT/dist/unofficial-muse-for-linux-x86_64.AppImage"
