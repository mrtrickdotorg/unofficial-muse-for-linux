# Building

## Native development and offline gates

The workspace uses edition 2024 and is verified with Rust 1.99; a lower minimum compiler version has not been established. Development also requires GTK4 headers, Bash, and `/usr/bin/python3`. On Debian/Ubuntu install `build-essential`, `pkg-config`, `libgtk-4-dev`, and `python3`; on Fedora install `gcc`, `pkgconf-pkg-config`, `gtk4-devel`, and `python3`.

```sh
# One-time, non-root setup of a pinned external test/runtime dependency.
./scripts/setup-account-bridge.sh
./scripts/test-account.sh
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked
# Requires Xvfb and xauth; TMPDIR must be a writable test scratch directory.
xvfb-run -a cargo test -p unofficial-muse-for-linux native_account_page_consent_failure_and_disconnect_signal_smoke --locked -- --ignored --test-threads=1
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p muse
```

`test-account.sh` syntax-checks all shell scripts under `scripts/`, compiles Python source without writing bytecode, parses the installer's embedded Python via `--help`, and runs all offline unittest cases under the bridge interpreter it finds by itself -- the installation `setup-account-bridge.sh` writes, or the one `MUSE_ACCOUNT_TEST_PYTHON` names -- after checking that installation's commit against the installer's pin. No variable has to be exported for the default install; a missing, stale or unusable bridge fails the gate with the exact command to run instead of skipping the wire tests. The gate fails if any test is skipped. Its scratch directory comes from `XDG_RUNTIME_DIR` or `HOME` (verified private: owned by the runner, no group/other bits) rather than `/tmp`, because the sign-in tests prove their config directory's whole path is private and a host whose `/tmp` belongs to `nobody`, or a checkout inside a group-writable tree, would otherwise fail those cases for reasons unrelated to the code. Account/login tests use controlled fakes; local-host wire tests use the pinned external runtime's real protobuf classes with fake gateway IO. No account credentials, live service, or actual installed browser are used. Installing the dependency requires network access; running these tests does not.

CI installs the same pinned external runtime for tests and retains formatting, locked workspace build/test, explicit native GUI smoke tests, and warning-denying Clippy checks. Unit tests do not establish live browser sign-in, service compatibility, packaged GUI behavior, or actual device registration.

## Driving the real GTK window locally

CI runs the display-dependent GUI tests under `xvfb-run`. A workstation without
GTK4 development packages can use the user-level toolbox prefix instead, which
needs no root and installs nothing:

```sh
. "$HOME/.local/share/muse-toolbox/env.sh"   # GTK4 pkg-config, linker, TMPDIR
cargo build -p muse --locked
```

The `#[ignore]`d tests then drive the real widgets in-process: they construct the
same `AccountPage` CI smokes, click and type through the widget API, and assert on
what the widgets say. That needs a display but no compositor, accessibility bus or
input-injection tool, and an existing Xwayland session supplies one:

```sh
TMPDIR="$HOME/.cache/muse-tmp" cargo test -p unofficial-muse-for-linux \
  native_account_page_consent_failure_and_disconnect_signal_smoke \
  --locked -- --ignored --test-threads=1
```

This is the supported way to reproduce UI behaviour locally: assertions run
against real GTK widgets rather than a model of them. It does not produce pixels.
The pinned `gsk4` exposes no `Snapshot` wrapper, so rendering a window to a file
would need raw FFI, and the host has no capture tool (`spectacle`, `import`,
`grim`) — visual review still means looking at the packaged AppImage or adding one
of those tools. A Broadway session is not a substitute: `broadwayd` binds an
abstract socket the GTK client does not look for, and the browser side is a canvas
with no widgets to address.

## Optional runtime account dependency

The native account path additionally requires an external source checkout and per-user Python 3.10+ environment. Install as a regular user, outside the AppImage/container:

```sh
./scripts/setup-account-bridge.sh
```

Choose the printed absolute `.venv/bin/python` in the app. Setup pins the community source, installs its dependencies, and verifies offline imports/data loading. It does not read or create an authenticated session. Existing destinations are refused, not updated in place. See [account integration](account-integration.md) for exact pin, paths, ownership checks, sign-in/session choice, license/provenance, and protocol drift. Python dependency versions are recorded after resolution, not fully hash-locked; account runtime setup is not a reproducible build of the Rust artifact.

## Pinned x86_64 AppImage build

The container uses Rust 1.99 on Ubuntu 24.04 (the base is that release on purpose: the bundle carries its GTK, and the AppImage's glibc floor is 2.39) with GTK 4 and GTK 3 packaging dependencies — the state layer is GTK 4 + WebKitGTK 6.0, the shell window is GTK 3 + WebKitGTK 4.1 — Python and a pinned test-only community runtime for the offline account/host gate. The test runtime is installed as an unprivileged build user under `/opt/muse-test` and is **not copied into the AppImage**. The linuxdeploy builder and GTK plugin use pinned version/commit downloads checked by SHA-256 before execution. The base image tag and apt packages are not digest/package-version locked; this is not a bit-for-bit reproducibility claim.

```sh
podman build -f Containerfile.appimage -t muse-linux-appimage-builder .
mkdir -p "$HOME/.cache/muse-linux-cargo"
podman run --rm \
  -e CARGO_HOME=/cargo-cache \
  -v "$PWD:/workspace:Z" \
  -v "$HOME/.cache/muse-linux-cargo:/cargo-cache:Z" \
  -w /workspace \
  muse-linux-appimage-builder \
  ./scripts/build-appimage.sh
```

CI runs this same build on every push to any branch or tag (`.github/workflows/appimage.yml`). For `main` it publishes the result to the rolling [`appimage-latest` release](../../releases/tag/appimage-latest), so the newest artifact is downloadable without a local container build; other refs keep their build as a workflow artifact only. Each commit gets its own concurrency group, so a burst of pushes can never cancel or replace a build, and every run uploads its AppImage as a workflow artifact named with the commit SHA — every pushed commit keeps its own downloadable build. Inside the container the tests need `TMPDIR` set to a scratch root with no world-writable ancestors (CI uses `/root/appimage-scratch`), because `/tmp` is rejected by the persistence checks.

The build runs `test-account.sh` and locked workspace Rust tests before creating `dist/unofficial-muse-for-linux-x86_64.AppImage`. `MUSE_BUILD_SKIP_ACCOUNT_TESTS=1` skips only that account gate, for a host without the pinned bridge; the workspace tests still run.

It bundles GTK/runtime support through linuxdeploy and **only our** `muse_account_bridge.py`, `muse_account_login.py`, `muse_account_host.py` and `muse_local_host.py` under `usr/share/unofficial-muse-for-linux/account/`. It also ships the shell window (`apps/muse-window`, built as its own project) as `usr/bin/muse-window` beside `usr/bin/muse`, the icon set at 16/32/48/64/128/256/512 in the hicolor tree, the desktop entry and the AppStream metadata. The window binary is installed *before* linuxdeploy's deploy, so that step deploys its GTK 3 / WebKitGTK 4.1 library closure into the same AppDir, and the WebKitGTK 4.1 helper tree rides in `usr/lib/webkit2gtk-4.1` with the same treatment as the 6.0 set — RUNPATH on the helpers and the injected bundle, the library's compiled-in helper path patched to the staging path the app links on launch — while `patch-webkit-path.py` refuses to package unless both generations were patched. `web_shell::clean_library_path` still strips the bundle's own library entries from the child's `LD_LIBRARY_PATH`, so the two generations never meet in one process. A host without that pair still runs: the app falls back to its native GTK 4 window. It does not bundle the external community checkout, Meta-derived route/descriptors, Python environment, browser, or any credentials. The host must supply the selected Python environment and, for fresh sign-in, a compatible Chromium browser. Host session services, portals, accessibility, display server, and graphics stack are not bundled.

## Building the AppImage on a workstation

Without the container, the same script builds on a host that has both GTK generations' development packages, WebKit's helper processes (WebKitGTK 6.0, for the state layer), `patchelf`, Python 3 and network access for the pinned linuxdeploy/appimagetool downloads:

```sh
scripts/render-icons.py resources/unofficial-muse-for-linux.svg /tmp/muse-icons 16 32 48 64 128 256 512
MUSE_BUILD_SKIP_ACCOUNT_TESTS=1 ./scripts/build-appimage.sh
# -> dist/unofficial-muse-for-linux-x86_64.AppImage
```

Run it with the environment the app itself is built in (the GTK 4 `PKG_CONFIG_PATH`); the script bakes absolute library paths and fails loudly when WebKit's helpers are missing. The icon set under `resources/icons/` and the window's own icons come from `scripts/render-icons.py`, which renders the app SVG through gdk-pixbuf (the app's own image stack) because this tree does not depend on rsvg-convert or ImageMagick: The developer build is a different artifact: `cargo run -p muse` uses `target/debug/muse` and `apps/muse-window/target/debug/muse-window`, and has a checkout, the `target` directories and the window's own dev path behind it. The packaged app finds `muse-window` beside its executable, so no `target` path is involved.

On a host without FUSE the built image still runs (`APPIMAGE_EXTRACT_AND_RUN=1 ./dist/unofficial-muse-for-linux-x86_64.AppImage`).

A rootless host (no container, and no system GTK 4 runtime to deploy) cannot run linuxdeploy's deploy step at all. There, `scripts/repack-appimage.sh` builds the same release binaries and repackages them over a released AppImage's runtime tree (`MUSE_APPIMAGE_BASELINE`, defaulting to `~/.cache/muse-appimage/squashfs-root`), replacing the binaries, sidecars, icons, desktop entry and metadata and packaging with the same appimagetool step. It is a development fallback for looking at the current tree as a bundle; the container build is the published artifact. Both this script and `scripts/build-appimage.sh` stamp the commit into `UMFL_BUILD_SHA` (defaulting to `git rev-parse HEAD` and printed as `Stamped build commit <sha>`) so the packaged update check compares itself against the feed instead of reading up to date from a missing sha, and it now also reads `HTTPS_PROXY`/`ALL_PROXY` the way the rest of the app's traffic does. With that stamp the Settings card names what the feed holds rather than just that something newer exists: the Nov 23, 2026 bundle answered `A newer build is available: a6ed625 (79.4 MB).` against the live feed.

## Release verification boundaries

Inspect the newly built artifact rather than treating source edits or an older AppImage as release evidence. Confirm all three project sidecars are present and no `routes.json`, `desc*.bin`, community source, venv, or saved account credentials are present. Re-run GTK folder-grant and account/history/task/local-host UI checks on that exact artifact. Record dates with timezone and distinguish offline tests, backend checks with an existing session, fresh interactive browser authentication, and actual Linux local-tool dispatch.

Earlier AppImage checks launched on KythOS KDE Wayland through XWayland and exercised the dark UI/navigation and read-only file chooser/list/preview. The GTK packaging hook currently selects X11; that evidence is **not native Wayland support**. It does not verify this batch's new packaged account UI, a fresh login, or Mac-style device pairing. See [the batch verification report](account-verification.md) for the new artifact, real-account backend evidence, and session rejection during the GUI check. Fresh authentication and device pairing still need explicit checks. Account authentication does not turn read-only local files into agent tools; the developer-key adapter remains a separate experiment.
