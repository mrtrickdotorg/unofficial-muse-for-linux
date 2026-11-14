# Flatpak packaging

`org.kythos.UnofficialMuseForLinux.json` packages the app for
Flatpak against the GNOME 48 runtime (GTK 4 + WebKitGTK 6.0 come
from the runtime, so none of the AppImage's WebKit helper staging
or path patching applies). Build with `scripts/build-flatpak.sh`
(locally) or the dispatch-only **Flatpak** GitHub workflow; both
vendor the crates from `Cargo.lock` first because the sandbox
build is offline.

Layout choices, all deliberate:

- The recorded application id stays `org.kythos.UnofficialMuseForLinux`
  (it is the GTK application id and the metainfo id). The desktop
  file and metainfo are installed under that name; the installed
  copies get their `Icon=` / `<launchable>` rewritten to match,
  the sources are untouched.
- The four account sidecar scripts install to
  `/app/usr/share/unofficial-muse-for-linux/account/` — the same
  `usr/share/...` shape the AppImage uses, because the app finds
  them by walking the executable's ancestors for that path. The
  Python bridge environment is not bundled (same as the AppImage);
  it is created per-user by the account setup flow, inside the
  app's per-user data directory.
- No `GDK_BACKEND` pin: the AppImage pins X11 through its
  linuxdeploy GTK hook; the Flatpak asks for Wayland with an X11
  fallback and lets GTK choose.
- Finish args: `--socket=session-bus` because the Computer
  Control page connects to the session-bus socket path directly
  (the RemoteDesktop portal conversation rides it), and
  `--filesystem=home` because the Local host page and the Files
  view exist to reach folders the user grants — the app gates
  every access behind its own grants, the same trust shape as
  the unsandboxed AppImage. This is a personal bundle, not a
  Flathub submission.

Not yet proven: a bundle built and installed end-to-end on a
real seat (first CI/local build pending), the in-sandbox account
setup flow (the runtime's python3 + venv), and portal device
grants inside the sandbox.
