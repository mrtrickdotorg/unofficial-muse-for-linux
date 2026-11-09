# Unofficial Muse for Linux

A Linux-native companion for the user's existing Muse personal agent. Independent, open source, and **not affiliated with, endorsed by, or supported by Meta**. “Muse” and related marks belong to their respective owners.

## Status

Early development, **not a feature-complete Muse-for-Mac port**. The GTK4 dark desktop has Chats/Files views, session-only read-only folder grants, browsing, bounded text preview, and filename search. Through an explicitly selected **unsupported community adapter** and the user's own Muse session, the account path adds native shared chat/history, prompt dispatch, streamed reply text with live agent status, cancel, reattach to a run already in progress, pending-approval cards with on-press decisions, scheduled-task/run lists with per-schedule deletion, an allowed-sites list with per-grant revocation, and a read-only Library page for account artifacts.

A separate, explicitly started **local host** registers this Linux machine with the account and lets the agent invoke a narrow, approval-gated file manifest (`files.create_text`, `files.read_text`, `files.search`, `files.edit`, `files.rename`, `files.delete` to Trash) inside explicitly granted folders only. Fresh isolated-browser sign-in, live registration, and an agent-to-Linux read round-trip have all been exercised against the real service; an approval-gated create has so far been exercised only by tests, and revocation mid-invocation is unproven live. Connecting an account by itself still gives the agent no Linux access — the local host, its folder grants, and per-action approvals are separate, explicit steps. The agent gets no shell, screen, browser, or unrestricted filesystem access. The human-side local Files view stays read-only and separate from the agent-facing write broker.

Still incomplete: task creation/editing, a durable policy store for approvals (the allowed-sites listing and per-grant revocation are wired; neither a live decision nor a live revocation has been pressed), provider connectors (the service's connector list currently answers 500), Library preview (the service returns no URL for any artifact kind), durable Linux background work, and keyring storage for the session jar. Interjection is accepted while a turn runs but is proven **not** to steer that turn — the service queues it as a later turn. Earlier AppImage launch checks used KDE Wayland through XWayland, not native Wayland. The current ordering and per-phase evidence live in the [Mac parity roadmap](docs/mac-parity-roadmap.md); the row-level state is in [feature parity](docs/feature-parity.md).

The app can launch an isolated, user-controlled official Muse browser login when a compatible Chromium browser is available, or explicitly use an existing local Muse session. It never asks for a password or developer API key for the account path. The separate developer-key Model API crates remain experiments, not Muse account sign-in.

## Account setup

```sh
# Run as your regular user; requires Git and Python 3.10+ with venv.
./scripts/setup-account-bridge.sh
```

The installer pins external community source into `~/.local/share/unofficial-muse-for-linux/account-bridge` (or the XDG data home) and creates its own Python environment. Select the printed `.venv/bin/python` in the app. It refuses existing destinations, never reads credentials, and never runs as root. The AppImage bundles only **our sidecars**, not the external checkout, Meta-derived protocol assets, Python environment, or account data.

See [account integration](docs/account-integration.md) for the source pin, sign-in/session choices, security boundaries, protocol-drift disclosure, and verification limits, and the [Mac parity roadmap](docs/mac-parity-roadmap.md) for the live evidence recorded since. [Account verification](docs/account-verification.md) is the October 18, 2026 batch report — a dated snapshot, not the current state. A session jar is a private plaintext credential, not encrypted storage.

## Development

```sh
./scripts/test-account.sh
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p unofficial-muse-for-linux
```

The workspace separates filesystem policy (`muse-linux-core`), the account subprocess adapter (`muse-account`), the experimental developer API/runtime crates, and the native app. See [building](docs/building.md), [architecture](docs/architecture.md), [feature parity](docs/feature-parity.md), and the [full-port goal](docs/full-port-goal.md).

## License

GPL-3.0-only. See [LICENSE](LICENSE). The separately installed community dependency declares MIT licensing; its source and provenance stay external to this repository and AppImage.
