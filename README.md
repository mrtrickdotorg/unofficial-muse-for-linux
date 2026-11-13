# Unofficial Muse for Linux

**Your Muse, at home on Linux.** 💛

Muse is Meta's personal agent — the one that knows your chats, your feed, your goals, your whole context. It has a lovely Mac app and a home on the web, but Linux never got an invitation. This project is the invitation, written by the community: a native Linux desktop client that lets your Muse live on your Linux machine too, in a window that feels like it belongs there.

Independent, open source, and **not affiliated with, endorsed by, or supported by Meta**. "Muse" and related marks belong to their respective owners. This is a love letter, not a product launch.

## Download

Grab the AppImage, make it executable, and run it — that's the whole install:

**[Download the latest AppImage](https://github.com/mrtrickdotorg/unofficial-muse-for-linux/releases/download/appimage-latest/unofficial-muse-for-linux-x86_64.AppImage)** (x86_64, ~100 MB — there's a whole web engine in there now, for reasons that will make sense in a moment)

Once you're running from the AppImage, updates arrive in the app itself: it checks, verifies the download's size and checksum, swaps itself in place, and relaunches. Your session is never touched.

## What it's like inside

The window follows the same menu as muse.ai, in the same order — **Chat, Search, Library, Ideas, Goals, Create, Feed** — because that's the shape your Muse already has, and a familiar home matters more than a clever one.

- **Chat** is the real thing: your conversations and history, replies streaming in with the agent's live status ("is working…", just like the web), the working bubble while it thinks, and the agent's own option buttons when it offers you a choice. Linking your account asks once; after that, launches reconnect on their own.
- **Feed** reads the way the web feed reads — your Muse's briefings and updates as proper cards in a comfortable reading column, hero images at their natural size, and **widgets rendered live** in a small locked frame: timelines draw themselves, countdowns compute themselves ("in 7 days" means *today* plus seven, figured out when you look at it). Links open in your browser, and **Discuss** drops you into Chat with the post quoted, ready to talk about it. If a widget frame ever can't start, the card quietly falls back to the widget's text — no holes, no errors.
- **Library, Ideas, and Goals** are read-only windows onto the real things, loading on their own when you open them — no refresh buttons to babysit.
- A **side panel** keeps company on the right: who's connected, what your agents did today, your scheduled tasks (deletion included), and recent runs.
- **Search** and **Create** have their seats saved in the menu, but the service doesn't offer those conversations to this client yet, so for now they say so honestly instead of pretending.

## Your agent, carefully

If you want your Muse to be able to *do* things on this machine, there's a separate **local host** you can start, on purpose, that registers the machine with your account. It's deliberately narrow:

- File operations only — create, read, search, edit, rename, delete-to-Trash — and only inside folders you explicitly grant.
- Every sensitive action asks first, with a card, every time. Approvals are **one-time only, by design**; there's no "always allow" switch to fat-finger.
- No shell, no screen, no browser control, no wandering the filesystem. Connecting an account by itself grants the agent *no* access to your machine — the host, the folder grants, and the per-action approvals are each their own explicit step.

Sign-in never asks this app for your password: it opens an isolated browser login you control, or uses an existing local Muse session you point it at. The account connection rides an explicitly selected, unsupported community adapter against your own Muse session — pinned, inspectable, and described plainly in [account integration](docs/account-integration.md). Your session jar is a private plaintext credential; treat it like one.

## Honest edges

This is early, spare-time software, and we'd rather label the gaps than hide them:

- The service's connector list currently answers 500, and Library preview is blocked on the service side (it returns no URL for any artifact), so both are wired-but-waiting.
- A durable, remembered approvals policy belongs on the Muse side and doesn't exist yet — one-time cards are the whole story for now.
- Computer control is intentionally not built.
- Live proofs still owed are tracked in the [Mac parity roadmap](docs/mac-parity-roadmap.md), which is also where the current ordering lives; [feature parity](docs/feature-parity.md) has the row-level state.

## Account setup

```sh
# Run as your regular user; requires Git and Python 3.10+ with venv.
./scripts/setup-account-bridge.sh
```

The installer pins the community adapter into `~/.local/share/unofficial-muse-for-linux/account-bridge` (or your XDG data home) with its own Python environment, and prints the `.venv/bin/python` to select in the app. It refuses existing destinations, never reads credentials, and never runs as root. The AppImage bundles only our sidecars — not the community checkout, protocol assets, a Python environment, or any account data.

## Development

```sh
./scripts/test-account.sh
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p unofficial-muse-for-linux
```

The workspace separates filesystem policy (`muse-linux-core`), the account subprocess adapter (`muse-account`), experimental developer-API/runtime crates (experiments, not account sign-in), and the native GTK4 app. See [building](docs/building.md), [architecture](docs/architecture.md), and the [full-port goal](docs/full-port-goal.md).

## License

GPL-3.0-only. See [LICENSE](LICENSE). The separately installed community dependency declares MIT licensing; its source and provenance stay external to this repository and AppImage.

---

*Built because a personal agent should feel personal on every machine you call home — including the Linux one.*
