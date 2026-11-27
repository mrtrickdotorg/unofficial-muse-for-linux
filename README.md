# Unofficial Muse for Linux

**Your Muse, at home on Linux.** 💛

Muse is Meta's personal agent — the one that knows your chats, your feed, your goals, your whole context. It has a lovely Mac app and a home on the web, but Linux never got an invitation. This project is the invitation, written by the community: a native Linux desktop client that lets your Muse live on your Linux machine too, in a window that feels like it belongs there.

Independent, open source, and **not affiliated with, endorsed by, or supported by Meta**. "Muse" and related marks belong to their respective owners. This is a love letter, not a product launch.

## Download

Grab the AppImage, make it executable, and run it — that's the whole install:

**[Download the latest AppImage](https://github.com/mrtrickdotorg/unofficial-muse-for-linux/releases/download/appimage-latest/unofficial-muse-for-linux-x86_64.AppImage)** (x86_64, ~86 MB — there's a whole web engine in there now, for reasons that will make sense in a moment)

```sh
chmod +x unofficial-muse-for-linux-x86_64.AppImage
./unofficial-muse-for-linux-x86_64.AppImage
```

That one file is the whole app: the state layer, its shell window, the icon set at every desktop size, the desktop entry and the app metadata. Once you're running from the AppImage, updates arrive in the app itself: it checks, verifies the download's size and checksum, swaps the file it was launched from in place, and relaunches. Your session is never touched.

The shell window is a second process, linked against GTK 3 and WebKitGTK 4.1, because the state layer's GTK 4 and WebKitGTK 6.0 cannot share a process with them. The AppImage carries that pair beside its own GTK 4 stack — the window binary, the WebKitGTK 4.1 helper tree and its library closure — and points that engine at the bundle before the window starts. Where the window cannot come up, the app opens its own GTK 4 window instead — the same pages in the fallback renderer — so the bundle still starts anywhere the GTK 4 side does.

## What it's like inside

The window opens covering your primary display the first time, and from then on remembers its size, position and whether it was maximized -- close it wherever you like and the next launch puts it back there.

The window follows the same menu as muse.ai, in the same order — **Chat, Search, Library, Ideas, Goals, Create, Feed** — because that's the shape your Muse already has, and a familiar home matters more than a clever one.

- **Chat** is the real thing: your conversations and history, replies streaming in with the agent's live status ("is working…", just like the web), the working bubble while it thinks, and the agent's own option buttons when it offers you a choice. The conversation is the only part that scrolls: the message box sits at the window's foot at every window size, a resize keeps the newest turn in view instead of dropping you mid-bubble, and a narrow window gives up the side panel and some drawer width before it ever squeezes the message box. Linking your account asks once; after that, launches reconnect on their own — and if that first connect ever fails, the next listing beat signs the window in instead of leaving it at "Sign in".
- **Threads** lists every conversation the account holds, day by day, pinned ones first — and each row carries the presses the mac app's own row menu has: **Pin** (or Unpin), **Rename**, **Archive** and **Delete**. Every press is the app's own `session_op` against your Muse, and the page paints the listing it reads back afterwards, so a change the service refuses leaves the row exactly as it was instead of pretending. Delete asks twice — the row turns into **Confirm delete** (or **Cancel**) and only the second press removes the conversation, which the service keeps no Trash for. A conversation with replies you haven't read carries your Muse's own unread count, and it moves while you watch: both drawer and page re-read the listing every eight seconds, so a message arriving on a conversation you don't have open badges its row, opening it marks it read, and the row's **Mark read** press clears a badge without opening anything.
- **Feed** reads the way the web feed reads — your Muse's briefings and updates as proper cards in a comfortable reading column, hero images at their natural size, and **widgets rendered live** in a small locked frame: timelines draw themselves, countdowns compute themselves ("in 7 days" means *today* plus seven, figured out when you look at it). Links open in your browser, and **Discuss** drops you into Chat with the post quoted, ready to talk about it. If a widget frame ever can't start, the card quietly falls back to the widget's text — no holes, no errors.
- **Library, Ideas, and Goals** are read-only windows onto the real things, loading on their own when you open them — no refresh buttons to babysit.
- A **side panel** keeps company on the right: who's connected, what your agents did today, your scheduled tasks (deletion included), and recent runs.
- **Search** reads your conversations through the service's own search route: type words, get the matches with the line each one matched, and press a result to open that conversation in Chat — both the native list and the shell window's Search page. **Create** keeps its seat in the menu and says so honestly, because the service offers this client no create surface yet.
- **Choosing a folder** (the session folder on sign-in, a grant on Local host) happens on a sheet inside the window, and the sheet can make the folder it stands in: type a name, press Create, and the new folder is made private (0700), opened, and ready for "Use this folder". The chat that follows is that folder's: the first message of a new chat starts the session, and the new conversation appears in History at once, marked as the open one.

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
# The app's window is the Tauri/React shell when it can be: it is its own project
# (GTK 3 + WebKitGTK 4.1), so build it once before running the app.
(cd apps/muse-window && cargo build --locked)
cargo run -p muse                  # opens the shell window on a linked clone
MUSE_SHELL=gtk cargo run -p muse    # the native GTK window

# The distributable: both binaries, icons, desktop entry and metadata, in one
# AppImage. `MUSE_BUILD_SKIP_ACCOUNT_TESTS=1` skips only the gate that needs the
# pinned account bridge (scripts/setup-account-bridge.sh); CI runs it.
MUSE_BUILD_SKIP_ACCOUNT_TESTS=1 ./scripts/build-appimage.sh
# -> dist/unofficial-muse-for-linux-x86_64.AppImage
```

A dev build is not the distributable. `cargo run -p muse` finds the window in
`apps/muse-window/target/debug` and serves the React bundle from the file the window
embeds; the AppImage carries release builds of both binaries in `usr/bin` and has no
checkout, no dev server and no `target` directory behind it. The packaged app looks for
`muse-window` beside its own executable, so the two stay a pair wherever the bundle is
unpacked.

The window's `tauri.conf.json` carries its own bundle metadata (identifier
`org.kythos.MuseWindow`, product name `Muse Shell Window`, the icon set at 32/64/128/256
and a hidden desktop entry), and `cd apps/muse-window && npx @tauri-apps/cli@2 build
--bundles deb` produces that component as a `.deb`. It is a component, not the app: it
is spawned by the app and has nothing to show alone, which is why its desktop entry is
`NoDisplay=true` and the installable app is the AppImage.

The app's window is `apps/muse-window`: a Tauri 2 window over the React pages in `apps/muse/ui`,
which the app spawns as a child process and answers over its own pipes -- the two cannot share a
process, because the app is GTK 4 + WebKitGTK 6.0 and the window is GTK 3 + 4.1. The native GTK
shell stays compiled as the fallback (no window binary, no saved link, or `MUSE_SHELL=gtk`) and as
the reference the pages are checked against.

The workspace separates filesystem policy (`muse-linux-core`), the account subprocess adapter
(`muse-account`), experimental developer-API/runtime crates (experiments, not account sign-in), and
the native GTK4 app. See [building](docs/building.md), [architecture](docs/architecture.md), and the [full-port goal](docs/full-port-goal.md).

## License

GPL-3.0-only. See [LICENSE](LICENSE). The separately installed community dependency declares MIT licensing; its source and provenance stay external to this repository and AppImage.

---

*Built because a personal agent should feel personal on every machine you call home — including the Linux one.*
