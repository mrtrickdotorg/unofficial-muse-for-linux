#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -m)" != "x86_64" ]]; then
    printf 'This build script currently targets x86_64 (found %s).\n' "$(uname -m)" >&2
    exit 2
fi

LINUXDEPLOY_VERSION="1-alpha-20251107-1"
GTK_PLUGIN_COMMIT="7a3fbc31a9e5075073ff8790f26effbac5f84453"
LINUXDEPLOY_SHA256="c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d"
GTK_PLUGIN_SHA256="b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171"
APPIMAGETOOL_SHA256="e8c1169c4af66c3184d7ee110713d4be04befe671fa358e2c1591ee840553b03"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/unofficial-muse-for-linux/appimage-tools"
LINUXDEPLOY="$CACHE_DIR/linuxdeploy-x86_64.AppImage"
APPIMAGETOOL="$CACHE_DIR/appimagetool-x86_64.AppImage"
GTK_PLUGIN="$CACHE_DIR/linuxdeploy-plugin-gtk"
APPDIR="$ROOT/target/AppDir"

mkdir -p "$CACHE_DIR"
fetch_pinned() {
    local url="$1" destination="$2" expected_sha256="$3"
    if [[ -f "$destination" ]] && printf '%s  %s\n' "$expected_sha256" "$destination" | sha256sum --check --status; then
        return
    fi
    curl --fail --location --retry 3 --output "$destination.tmp" "$url"
    printf '%s  %s\n' "$expected_sha256" "$destination.tmp" | sha256sum --check --status
    mv "$destination.tmp" "$destination"
    chmod 755 "$destination"
}

fetch_pinned \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/$LINUXDEPLOY_VERSION/linuxdeploy-x86_64.AppImage" \
    "$LINUXDEPLOY" "$LINUXDEPLOY_SHA256"
fetch_pinned \
    "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/$GTK_PLUGIN_COMMIT/linuxdeploy-plugin-gtk.sh" \
    "$GTK_PLUGIN" "$GTK_PLUGIN_SHA256"
fetch_pinned \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage" \
    "$APPIMAGETOOL" "$APPIMAGETOOL_SHA256"

cd "$ROOT"
# The updater compares this build's commit with the feed's. CI passes its own
# (`UMFL_BUILD_SHA`); a local build stamps this checkout's HEAD, so a bundle
# built here can answer the real check instead of only ever saying it is
# current.
export UMFL_BUILD_SHA="${UMFL_BUILD_SHA:-$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)}"
if [[ -n "$UMFL_BUILD_SHA" ]]; then
    printf 'Stamped build commit %s\n' "$UMFL_BUILD_SHA"
fi
# The updater orders builds by when they were built: CI passes the run's
# instant (`UMFL_BUILD_EPOCH`, the same value its release notes publish);
# a local build stamps its own build instant, so the ordering the check
# applies is the one the feed describes.
export UMFL_BUILD_EPOCH="${UMFL_BUILD_EPOCH:-$(date -u +%s)}"
printf 'Stamped build epoch %s\n' "$UMFL_BUILD_EPOCH"
# The account gate needs the pinned community bridge, which is installed by
# scripts/setup-account-bridge.sh (CI and the Containerfile do it, and the
# gate finds the default installation on its own). A host without it cannot pass
# the gate: the gate refuses to skip its wire tests and says how to install the
# bridge. Say so when skipping, rather than pretending it ran.
if [[ "${MUSE_BUILD_SKIP_ACCOUNT_TESTS:-}" == "1" ]]; then
    printf 'Skipping the account gate (MUSE_BUILD_SKIP_ACCOUNT_TESTS=1): the pinned bridge is not installed here.\n' >&2
else
    ./scripts/test-account.sh
fi
cargo test --workspace --locked
cargo build --release --locked -p muse
cargo build --release --locked -p muse-reminder-runner
# The shell window is its own project on purpose (GTK 3 must not join
# this workspace's lock, build or tests), so it builds by manifest.
cargo build --release --locked --manifest-path "$ROOT/apps/muse-window/Cargo.toml"

rm -rf -- "$APPDIR"
ACCOUNT_DIR="$APPDIR/usr/share/unofficial-muse-for-linux/account"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/metainfo" "$ACCOUNT_DIR"
install -m 755 "$ROOT/target/release/muse" "$APPDIR/usr/bin/muse"
# The reminder runner ships in the image so AppImage users can extract it
# and install the user service (scripts/install-reminder-service.sh); the
# image itself cannot register a systemd unit.
install -m 755 "$ROOT/target/release/muse-reminder-runner" "$APPDIR/usr/bin/muse-reminder-runner"
# The shell window rides beside the app binary: the app looks for it
# there first, and linuxdeploy deploys its GTK 3 / WebKitGTK 4.1
# library closure alongside the app's own GTK 4 one.
install -m 755 "$ROOT/apps/muse-window/target/release/muse-window" "$APPDIR/usr/bin/muse-window"

# WebKit's helper processes are not part of the binary's library
# closure, so linuxdeploy cannot find them on its own — copy the
# directory into the AppDir explicitly, and fail the build rather
# than ship frames that can never render. (How the engine finds
# them at runtime is the staging story in patch-webkit-path.py:
# its compiled-in path is absolute, so the bundle's library is
# patched to a staging path the app symlinks on launch.)
webkit_helpers=""
for candidate in /usr/lib/*/webkitgtk-6.0; do
    if [[ -d "$candidate" ]]; then
        webkit_helpers="$candidate"
        break
    fi
done
if [[ -z "$webkit_helpers" ]]; then
    printf 'WebKit helper processes not found (is libwebkitgtk-6.0-dev installed?).\n' >&2
    exit 1
fi
mkdir -p "$APPDIR/usr/lib"
cp -r "$webkit_helpers" "$APPDIR/usr/lib/"

# The shell window's helpers are the same story for the other WebKit
# generation: copy the directory in explicitly, and fail the build
# rather than ship a window whose first page can never render. (Its
# library is patched to the 4.1 staging path by the same
# patch-webkit-path.py run, and the app links that path on launch.)
webkit41_helpers=""
for candidate in /usr/lib/*/webkit2gtk-4.1; do
    if [[ -d "$candidate" ]]; then
        webkit41_helpers="$candidate"
        break
    fi
done
if [[ -z "$webkit41_helpers" ]]; then
    printf 'WebKit 4.1 helper processes not found (is libwebkit2gtk-4.1-dev installed?).\n' >&2
    exit 1
fi
cp -r "$webkit41_helpers" "$APPDIR/usr/lib/"
install -m 644 "$ROOT/resources/unofficial-muse-for-linux.appdata.xml" "$APPDIR/usr/share/metainfo/unofficial-muse-for-linux.appdata.xml"
# The icon set the bundle carries: a hicolor tree at every size a desktop asks
# for. linuxdeploy also takes the SVG for the AppImage's own icon; the PNGs are
# what a menu falls back to on hosts that do not scale SVG app icons.
for size in 16 32 48 64 128 256 512; do
    install -Dm644 "$ROOT/resources/icons/${size}x${size}.png" \
        "$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps/unofficial-muse-for-linux.png"
done
# Bundle only this project's typed sidecars, never the external community
# checkout, Meta-derived protocol assets, Python environment, or account state.
install -m 644 "$ROOT/scripts/muse_account_bridge.py" "$ACCOUNT_DIR/muse_account_bridge.py"
install -m 644 "$ROOT/scripts/muse_account_login.py" "$ACCOUNT_DIR/muse_account_login.py"
install -m 644 "$ROOT/scripts/muse_local_host.py" "$ACCOUNT_DIR/muse_local_host.py"
install -m 644 "$ROOT/scripts/muse_account_host.py" "$ACCOUNT_DIR/muse_account_host.py"

shopt -s nullglob
for previous in "$ROOT"/target/*.AppImage; do
    rm -f -- "$previous"
done
cd "$ROOT/target"
export APPIMAGE_EXTRACT_AND_RUN=1
export DEPLOY_GTK_VERSION=4
export LINUXDEPLOY
export PATH="$CACHE_DIR:$PATH"
# Deploy only, for now: the WebKit library must be patched before
# packaging (below), and a second linuxdeploy run would redeploy a
# pristine copy over the patch — so appimagetool does the packaging.
"$LINUXDEPLOY" \
    --appdir "$APPDIR" \
    --desktop-file "$ROOT/resources/unofficial-muse-for-linux.desktop" \
    --icon-file "$ROOT/resources/unofficial-muse-for-linux.svg" \
    --plugin gtk

python3 "$ROOT/scripts/patch-webkit-path.py" "$APPDIR/usr/lib"

# WebKit launches its helper processes with a scrubbed
# environment — LD_LIBRARY_PATH does not survive — so the helpers
# must find the bundled libraries through their own RUNPATH:
# $ORIGIN/.. from usr/lib/webkitgtk-6.0 (and ../.. for the
# injected bundle). Without this the helpers die on launch with
# "libwebkitgtk-6.0.so.4: cannot open shared object file" and the
# first WebView traps the app.
for helper in WebKitWebProcess WebKitNetworkProcess WebKitGPUProcess; do
    helper_path="$APPDIR/usr/lib/webkitgtk-6.0/$helper"
    if [[ ! -f "$helper_path" ]]; then
        printf 'WebKit helper missing from the AppDir: %s\n' "$helper" >&2
        exit 1
    fi
    patchelf --set-rpath '$ORIGIN/..' "$helper_path"
done
patchelf --set-rpath '$ORIGIN/../..' \
    "$APPDIR/usr/lib/webkitgtk-6.0/injected-bundle/libwebkitgtkinjectedbundle.so"

# The shell window's helpers need the same RUNPATH treatment: its
# WebKit launches them with the same scrubbed environment.
for helper in WebKitWebProcess WebKitNetworkProcess WebKitGPUProcess; do
    helper_path="$APPDIR/usr/lib/webkit2gtk-4.1/$helper"
    if [[ ! -f "$helper_path" ]]; then
        printf 'WebKit 4.1 helper missing from the AppDir: %s\n' "$helper" >&2
        exit 1
    fi
    patchelf --set-rpath '$ORIGIN/..' "$helper_path"
done
patchelf --set-rpath '$ORIGIN/../..' \
    "$APPDIR/usr/lib/webkit2gtk-4.1/injected-bundle/libwebkit2gtkinjectedbundle.so"

ARCH=x86_64 "$APPIMAGETOOL" "$APPDIR" "$ROOT/target/unofficial-muse-for-linux-x86_64.AppImage"

artifact=""
for candidate in "$ROOT"/target/*.AppImage; do
    if [[ -f "$candidate" ]]; then
        artifact="$candidate"
        break
    fi
done
if [[ -z "$artifact" ]]; then
    printf 'linuxdeploy completed without producing an AppImage.\n' >&2
    exit 1
fi

install -Dm755 "$artifact" "$ROOT/dist/unofficial-muse-for-linux-x86_64.AppImage"
printf 'Built: %s\n' "$ROOT/dist/unofficial-muse-for-linux-x86_64.AppImage"
