# Architecture

## Separation of concerns

- `muse-linux-core`: permission-scoped, read-only filesystem operations and policy tests; independent of GUI and network access.
- `muse-account`: typed account/history/tasks/send operations with a bounded Python subprocess. Account construction alone does not access credentials or the network; operations are explicit.
- `scripts/muse_account_bridge.py`: our allowlisted JSON sidecar over an explicitly selected external community Python environment. It imports the source checkout; no MCP server or raw API is exposed to the UI.
- `scripts/muse_account_login.py`: explicit user-controlled official Muse browser sign-in using an isolated Chromium-compatible profile and a narrowly scoped app-owned session jar. A compatible browser is an external prerequisite; fresh live sign-in has since been exercised against the real service (see the [roadmap](mac-parity-roadmap.md) P0 gate), while expiry/revocation are surfaced as explicit states.
- `apps/desktop`: native GTK4 dark shell with Chats/Files views, account configuration, native shared history/task views, streamed replies with live status and approval cards, a read-only Library page, and the Local host page (grants, approval cards, activity log). Network and subprocess work must not block GTK's main thread.
- `muse-local-link` + `muse-local-tools`: the account-linked local host — a persistent control connection that registers a narrow command manifest, and the descriptor-pinned `openat2` permission broker that executes approved invocations inside granted roots only. See [local host](local-host.md).
- `muse-meta-api` and `muse-agent-runtime`: the separate experimental developer-key Model API and bounded read-only tool loop. These are not consumer Muse login or a local-host bridge for the account adapter.
- Future local connectors: narrow typed capabilities with grants and auditable results, never an unrestricted shell or whole-home-directory API.

## Muse.ai account link and local-host bridge are core requirements

The goal is the user's existing Muse personal agent plus permissioned Linux capabilities—not a website wrapper, a separate model client, or a Mac relay. Account continuity and local-host integration are separate requirements. Account continuity runs through an **unsupported authenticated community adapter**. Local-host integration is a separate layer on top of it: the explicitly started local host registers this machine under one persisted node identity and dispatches a narrow file manifest through the Rust permission broker — live registration and an agent-to-Linux read have been exercised against the real service (see [local host](local-host.md) and the [roadmap](mac-parity-roadmap.md)). Neither layer is full Mac parity.

```text
GTK -> muse-account -> our JSON sidecar -> selected Python environment
                                       -> external pinned muse-mcp source
                                       -> Muse account/personal-agent gateway

Files view -> muse-linux-core -> selected read-only folder
Local host -> muse-local-link -> muse-local-tools broker -> granted folders only
```

The account interface is an allowlist, not a general route method: `connect`, `identity`, `history`, `watch`, `activity`, `cancel`, `approvals`, `decide`, `permissions`, `revoke`, `tasks`, `artifacts`, and `send`. Internally, task routes are restricted to `tasks.list` (schedules) and `tasks.runs` (executions), and approval reads/decisions plus the grants listing and revocation go through the sentinel service on their own connection. Do not expose a general route method, account-backed remote filesystem, or local command execution. Selected local files are not automatically shared with the account-linked agent — only an explicit local-host folder grant does that, and only inside the granted root.

Prompt dispatch may trigger the personal agent's existing remote capabilities. It is an explicit external action. Send once, then verify through scoped history: prompt persistence is not assistant completion. Uncertain outcomes must not cause automatic retry. Read operations, history scope, identity, and task shapes need real-account checks separate from offline tests.

## Authentication and trust boundaries

App credentials default to `${XDG_CONFIG_HOME:-$HOME/.config}/unofficial-muse-for-linux/account`. Existing `${XDG_CONFIG_HOME:-$HOME/.config}/muse-cli` credentials are used only after an explicit user choice. Account directories must be user-owned/private and jars regular private files; reject symlinks and insecure permissions rather than changing arbitrary paths. The browser helper captures only Muse `hatch_sess` into a mode-0600 jar; passwords and API keys never pass through app forms. This is permission-protected plaintext storage, not a keyring. Disconnect/local deletion is not proof of server-side revocation.

Treat the selected interpreter, source checkout, dependencies, and browser as trusted local code running with the user's privileges; the subprocess is **not a sandbox**. Keep credentials out of argv, logs, crash reports, Rust values, and JSON results. Project typed response projection and fixed errors do not make arbitrary upstream payloads safe to log.

The community gateway protocol is internal/unversioned and may change or stop working. Unsupported status is an explicit compatibility/security disclosure, not a reason to substitute the developer API and claim continuity. The first-party phone/BLE Linux gadget path is a different device-token protocol and is not integrated here. See [account integration](account-integration.md) for source/license provenance, setup, data flow, and limits.

## Linux desktop boundary

Use XDG desktop portals for file selection, screen capture, and remote desktop/input where supported; use accessibility and provider-specific APIs for semantic context. Muse sign-in authorizes account access, not access to the Linux machine. Each future local action needs its own Linux permission, action-specific approval when sensitive, denial behavior, and verified side effects. Missing portals/providers must appear unavailable, not simulated.

AppImage packages the program, not host desktop-session services or a Python/browser runtime. Current launch evidence is XWayland on KDE Wayland; native Wayland needs separate validation.

## Dependency and asset policy

The AppImage contains the Rust application and our two sidecars under `usr/share/unofficial-muse-for-linux/account/`. The community source, `routes.json`, `desc*.bin`, Meta-extracted code/assets, Python venv, and all account state stay external. The installer fetches the reviewed community commit into a new per-user directory, never overwrites an existing tree, never runs as root, and performs no authentication.

Keep this project's GPL-3.0-only license and non-affiliation notice. The external community repository declares MIT licensing and protocol material derived from the web client; preserve its provenance rather than relabeling it as ours. No proprietary app binaries/assets, shared hidden credentials, or credential-bearing distribution are included.
