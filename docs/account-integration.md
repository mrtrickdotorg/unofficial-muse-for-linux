# Account integration

## Scope

The native desktop account path uses the user's existing Muse personal agent through an **unsupported community adapter**, not the separate developer-key Model API. This project and adapter are not affiliated with or supported by Meta. The user explicitly chooses whether to install and use that dependency.

The account batches add typed account connection, shared native chat/history, prompt dispatch, streamed replies with live status, cancel, pending-approval reads and on-press decisions, read-only scheduled-task/run lists, and a read-only Library list. `tasks.list` returns schedules; `tasks.runs` returns individual executions. Fetching those lists does not implement a Linux scheduler or task editing. Account linkage **by itself** is not device registration or a Linux local-tool dispatch channel: that is the separate, explicitly started [local host](local-host.md), which has since registered live and round-tripped an agent file read. The agent gains no shell, screen, browser, or filesystem access from connecting an account alone.

## Install the external dependency

Requirements: Git, Python 3.10+ with `venv`/`ensurepip`, and network access to GitHub and the Python package index. Some distributions package `venv` separately. Use an ordinary user account; the script refuses root. No `sudo`, piped shell, upstream installer, credentials argument, or browser automation is used during setup.

From this repository:

```sh
./scripts/setup-account-bridge.sh
# Optional: choose a compatible installed Python or a NEW install directory.
./scripts/setup-account-bridge.sh --python /absolute/path/to/python3 \
  --prefix "$HOME/.local/share/unofficial-muse-for-linux/account-bridge-new"
```

Default installation: `${XDG_DATA_HOME:-$HOME/.local/share}/unofficial-muse-for-linux/account-bridge`.

- Source: `https://github.com/kleprevost/muse-mcp.git`
- Reviewed source pin: `965cc2f197543a8762d3d7067038da6de962730a`
- Layout: `source/` checkout, `.venv/`, `installation.json`, and `installed-dependencies.txt`.
- Select the resulting absolute `.venv/bin/python` in the app. That interpreter imports the pinned source through a path-only `.pth` file, keeping `muse.py` beside its descriptors and route data.
- An existing destination—including a symlink or an earlier installation—is refused. Use a new path for an update, inspect it, then explicitly select its interpreter. Failures retain the new partial installation for inspection; nothing existing is deleted.
- Installation ancestors must not be symlinks or group/other-writable; directories inside the account's home must be owned by that user. The destination must be a new directory under that home. No permission repair is performed on arbitrary existing directories.

The source commit is pinned and checked after fetch. Our browser sign-in helper uses only Python's standard library. The community package's Python dependencies follow the source pin's version ranges, not a hash-locked transitive dependency set; resolved versions are recorded for diagnosis. A Git commit pin is not publisher signature verification. Setup verifies offline imports, descriptors, route loading, and transport availability; it does not authenticate or contact the user's Muse agent.

## Sign-in and session choice

The app-specific default account directory is `${XDG_CONFIG_HOME:-$HOME/.config}/unofficial-muse-for-linux/account`. Merely starting the app does not opt into the community CLI's existing credentials.

The explicit browser sign-in helper uses a **new isolated Chromium-compatible browser profile** and opens the official Muse login page. The user controls sign-in in that browser; the application does not ask for a password or API key. Only the scoped Muse `hatch_sess` cookie is captured into the app's account directory, with a private directory and mode-0600 session jar. No normal browser profile is scraped or reused. Without a compatible browser, sign-in is unavailable and must be reported as such. Fresh interactive browser authentication was not demonstrated in this batch's overnight validation — it has since been completed against the real service, as recorded in the [roadmap](mac-parity-roadmap.md) P0 gate; helper unit tests alone would not have been proof of a real sign-in.

**Use my existing Muse login** is a separate, explicit opt-in to `${XDG_CONFIG_HOME:-$HOME/.config}/muse-cli`. It does not copy those credentials into the repository or AppImage, and it does not register a device. Keep the session directory private (0700) and session jar private (0600); symlinks and insecure ownership/permissions are rejected rather than silently fixed. Do not paste session cookies into a terminal command, issue, chat, or log. The two paths cannot share one folder: the sign-in helper refuses a folder named `muse-cli`, so pressing **Sign in with Muse** while that folder is selected switches to the app's own folder and voids consent instead of attempting a doomed sign-in.

A session jar is a reusable **plaintext credential protected by filesystem permissions**, not encrypted credential storage. Protect backups and the local account; never include account directories in bug reports. Removing a local jar or disconnecting the UI does not prove server-side session revocation. Use the official account/session controls to revoke access. OS credential-store integration and end-to-end revocation validation remain work to do.

## Runtime boundary

```text
Native GTK UI
  -> muse-account (typed Rust operations; bounded subprocess)
    -> our muse_account_bridge.py (one JSON request/result over stdio)
      -> selected per-user Python + external pinned community source
        -> authenticated Muse personal-agent gateway

Explicit sign-in action
  -> our muse_account_login.py
    -> isolated, user-controlled official Muse browser login
    -> app-owned scoped session jar
```

No MCP server or listening local service is required. The sidecar allowlist is `connect`, `identity`, `history`, `watch`, `activity`, `cancel`, `approvals`, `decide`, `tasks`, `artifacts`, and `send`; it does not expose the community adapter's arbitrary raw-route escape hatch. Internally, task reads are restricted to `tasks.list` and `tasks.runs`, and approval reads/decisions are bound to the sentinel service rather than the shared gateway. Neither sidecar exposes a general local shell or remote file actions.

Rust receives typed account/history/task data, not cookies or upstream exception text. Account directory paths may be passed to the sidecar; session values are not put in process arguments, logs, error messages, or JSON replies. Treat retrieved agent messages as untrusted text. Sending a prompt is an external action and may instruct the existing agent to use its already configured capabilities; it is not a harmless local echo.

Send performs one dispatch and checks scoped history for a newer matching user message. A confirmed result means **prompt persistence**, not completion of the agent's work. A timeout or uncertain result must not be auto-retried: the original dispatch may already have taken effect. Refresh history first. Local file permissions remain a separate, read-only feature; this account path does not implicitly upload a selected folder or connect the experimental local tool loop to the personal agent.

When `muse_account_host.py` is installed beside the bridge, account reads and sends go through a long-lived helper instead of one process per action. It holds two gateway connections (one for waits, one for everything else, because a single Noise connection tolerates only one frame consumer), serves only the same allowlisted operations over a private Unix socket, re-reads the session on every request, drops its connections when the pinned cookie content changes, refuses any request that tries to name a different session directory, and exits when orphaned or idle. The one-shot bridge stays in the tree and is used automatically when no host is running, so a stale install or a broken helper degrades to the previous behaviour rather than blocking a send.

## Provenance, licensing, and drift

The external [community repository at the pinned commit](https://github.com/kleprevost/muse-mcp/tree/965cc2f197543a8762d3d7067038da6de962730a) declares MIT licensing. Its README attributes `muse.py`, `cli.py`, `routes.json`, descriptors, and protocol notes to `nikships/muse-cli`; its source describes routes/framing as derived from the Muse web client. Its gateway protocol is internal and unversioned, not an official supported API. Service updates, dependency changes, session expiry, VM availability, or account differences can break the adapter without notice. The pin is a compatibility baseline, not a service guarantee.

Our GPL-3.0-only app bundles **only our two Python sidecars** at `usr/share/unofficial-muse-for-linux/account/`. It does not bundle the community checkout, `routes.json`, `desc*.bin`, Meta-extracted code/assets, Python environment, cookies, or account data. The setup step obtains the external source into the user's local directory, preserving its source/license there; this separation does not assert ownership of upstream protocol-derived material. Users must consider the service's terms and their own account authorization before choosing the adapter.

The separate first-party Linux gadget SDK uses phone/BLE pairing and a device token, not personal-client login. It is not used by this account adapter and is not a replacement for shared chat/history/task fetching. Adding it later would require a distinct design and dispatch/permissions proof; no broad `system.run` capability is enabled here.

## Verification and remaining work

See [executed batch verification](account-verification.md) for the October 18 batch's real-account round-trip and the subsequent HTTP-401/session limitation during its packaged GUI check — a dated snapshot. Later live evidence (fresh sign-in, local-host registration and invocation, streaming/cancel/approvals) is recorded in the [Mac parity roadmap](mac-parity-roadmap.md) and [local host](local-host.md).

```sh
./scripts/test-account.sh
cargo test --workspace --locked
```

The canonical account gate uses `/usr/bin/python3` standard-library unittest discovery over `scripts/tests/test_*.py`, fake imports, and no credentials or network. It also syntax-checks shell/Python scripts. Build and CI invoke it in addition to the existing Rust checks. See [building](building.md) for the complete gates.

Keep evidence categories separate: offline tests, source-install/import checks, authenticated backend probes, GUI account/history/task checks, fresh browser login, and Mac-style local-host dispatch. Passing one does not establish the others. Integrated release evidence must record the exact artifact, local date/timezone, account scope, and observed result without including names, IDs, message text, or credentials.

Remaining parity includes a live-pressed approval decision and revocation during an in-flight invocation, a durable approval policy store, task creation/editing, resume, browser/app/screen connectors beyond the approval-gated file manifest, and durable Linux background work. Fresh sign-in, local-host registration/dispatch, streaming, approval reads/decisions, cancel, and approval-gated local file writes are implemented and — where the roadmap says so — proven live. The feature matrix is in [feature-parity.md](feature-parity.md).
