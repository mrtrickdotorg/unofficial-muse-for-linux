# Porting the client to Rust + Tauri + React: analysis and staging plan

**Status: S0 shipped, S1 begun (Nov 20, 2026).** The decision was made to continue toward the
Tauri/React end state. Stage S0 (the palette lift) is in the tree, and S1's first page — Library,
read-only, in the existing WebKitGTK view with real rows — is implemented and verified on this
seat. Everything below the stage list is the analysis that produced the plan.

_Nov 20, 2026. Evidence: the inventory below is read from this tree at `3e652e1`; the Kyth Hub
comparison is read from `kyth/src/kyth-hub-web` (Tauri 2.11, React 18, `gtk 0.18.2` /
`webkit2gtk 2.0.2`); the Tauri engine facts are from Tauri's own docs and lockfile, not from a
build of this app under Tauri._

## The question

Kyth Hub looks better than this client, and it is a Tauri/React app. Should Muse follow it?

The comparison misfires in one specific way: **the Hub's polish comes from its stylesheet, not
from Tauri's engine or from React.** Its look is a 2,068-line `theme.css` with 59 design tokens
(layered navy surfaces, one blue→cyan accent ramp, a three-step text scale, a 12/14/20/999 px
radius scale, one card shadow and one accent glow), applied through class names. The React
components are thin: `HeroCard.tsx` is 61 lines with 8 `className`s and 6 inline styles;
`GaugeCard.tsx` is 81. Nothing about the framework produces that appearance, and the Hub runs on
the *older* Linux stack (GTK 3 + WebKitGTK 4.1).

So the real split is: **port to get flexbox/grid layout and browser devtools** (a developer-speed
argument, not a looks argument), versus **lift the design language into the existing client**
(a looks argument, days not months). This page prices the first and stages both.

## What a port keeps (the cheap half)

The Rust core is already UI-free and would move without changes:

| Stays | Why it is portable |
|---|---|
| `crates/muse-account` | Typed adapter over the Python bridge; no GTK |
| `crates/muse-local-tools` | Broker, grants, approval protocol, policy store; pure Rust + `serde_json` |
| `crates/muse-local-link` | Local-host transport and manifest |
| `crates/muse-tasks`, `crates/muse-reminder-runner` | Durable reminder store + headless runner |
| `crates/muse-calendar`, `crates/muse-mail`, `crates/muse-desktop` | Provider/portal connectors, UI-agnostic |
| `scripts/muse_account_bridge.py`, `muse_account_login.py`, `muse_account_host.py`, `muse_local_host.py` | CLI/stdio sidecars |
| `scripts/test-account.sh`, Python suites | Their subject does not change |
| systemd unit, Flatpak manifest, container AppImage recipe | Mechanical adaptation only |

Of the last 120 commits before this analysis, 29 touched nothing but this core.

## What a port rewrites

`apps/muse` is 14,555 lines; **~11,700 of them are widget construction.**

| File | Lines | Becomes |
|---|---|---|
| `pages.rs` | 6,192 | ~10 React route components (Library, Ideas, Goals, Feed, Search, Create, Account, Local host, Policy, schedule lists) |
| `app.rs` | 2,483 | Shell: rail, side panel, page stack, window sizing, session state |
| `composer.rs` | 616 | Composer card, attachments, send/stop, keyboard handling |
| `chat_view.rs` | 588 | Transcript rows, streaming text, working bubble, option pills |
| `chat.rs` | 539 | Chat page state and navigation |
| `widget.rs` | 334 | Feed widget frames — becomes DOM/`<iframe>`, a simplification |
| `local_host.rs` | 952 | Controller logic stays; cards, grants and log rendering do not |
| `theme.css` | 298 | The look's **spec**; becomes a web stylesheet (Hub's is 2,068 lines, so expect 1.5–2k) |
| other (`main`, `session`, `update`, `login`, `signin`, `voice`, `images`) | ~1,100 | Split: pure logic moves, rendering is rebuilt |

Widget surface to re-express: 183 `Label`, 149 `Box`, 117 `Button`, 51 `ListBox`, 32 `ListBoxRow`,
28 `Window`, 23 `Entry`, 10 `ScrolledWindow`, 9 `CheckButton`, 6 `Popover`, 5 `Stack`, 3
`adw::OverlaySplitView`, plus `Paned`, `FlowBox`, `ComboBoxText`, `TextView`.

**Tests: 98 in-app assertions** (30 in `pages.rs`) drive real GTK widgets. They cannot be
translated; they get re-expressed as DOM tests. Expect a window where layout and interaction
regressions slip past the new net, because the old tests assert what GTK produced.

## Integrations that must be re-homed

Each is GTK- or process-bound today:

| Today | Line | Under Tauri |
|---|---|---|
| `gtk4::FileChooserNative` for folder grants | `pages.rs:728` | Portal dialog (`rfd`/`ashpd`); portal permissions on KDE differ |
| Self-update: download AppImage, verify, swap, relaunch | `update.rs` (388 lines) | Keep as a Rust command, or `tauri-plugin-updater` and lose the relaunch-as-new-instance logic |
| Single instance via Gio application flags | `main.rs:82` | `tauri-plugin-single-instance` |
| `glib::timeout_add_local` pumps in nine modules | app-wide | Events pushed from Rust, or timers; every timing-sensitive behaviour (stream coalescing, page-fill retries, approval expiry) must be re-proven |
| `webkit6::WebView` feed frames with helper staging | `widget.rs` | `<iframe>`; the `/tmp/.webkitgtk-6.0-umfl` staging disappears |
| Image fetch + disk cache | `images.rs` (150) | `#[tauri::command]`, mostly copy-paste |
| Voice: spawn external command | `voice.rs` (194) | Same, as a command |
| Sign-in: spawn Chromium | `signin.rs` (307) | Same |
| AT-SPI names already added to GTK widgets | app-wide | ARIA in the DOM; WebKitGTK's a11y tree is weaker than GTK's |

## Keeping today's look and feel

| Property | Cost |
|---|---|
| Colors, radii, type sizes, shadows, spacing, gradients | Transliterate directly; `theme.css` is the spec |
| Chat shapes (bubbles, floating composer, pills) | Direct |
| Keyboard navigation, focus order, list traversal | Rebuilt by hand; GTK gives these free |
| Text selection, IME, context menus, drag-and-drop | Rebuilt or subtly different |
| Native folder dialog, window decorations, system theme following | Replaced by portal/web equivalents |
| Scroll physics and scrollbar behaviour | Engine-specific; will not match |
| Pixel identity | **Not achievable** — the engine changes (see below) even if the CSS is identical |

"Same look and feel" therefore means same design language and same interaction contract, not same
pixels. The seam that exists today (libadwaita defaults under KDE/Breeze) disappears in one
direction: a web UI looks identical on every desktop, which is part of why the Hub reads as
polished on the same machine.

## Engine reality

- **Tauri 2 on Linux is GTK 3 + WebKitGTK 4.1.** `tauri 2.12.6` depends on `gtk 0.18 (v3_24)` and
  `webkit2gtk 2`; the Hub's own lockfile agrees (`gtk 0.18.2`, `webkit2gtk 2.0.2`), and KythOS
  ships `webkit2gtk4.1` to match.
- The **GTK4 / WebKitGTK 6.0 migration is merged on Tauri's `dev` branch** (tauri#14684) but rides
  Tauri 3, a draft milestone; it is a breaking change and not a released target.
- This client bundles **WebKitGTK 6.0** today (Feed widgets, Library previews). A Tauri 2 port
  therefore either ships a second WebKit generation or forces those views down to 4.1.
- Known Linux webview failure modes hit NVIDIA/Wayland first (blank window, `AcceleratedSurfaceDMABuf`,
  Wayland `Error 71`), which is the seat and the "native Wayland, not XWayland" gate this project
  still owes.

## Effort

Order-of-magnitude, one experienced developer familiar with this code:

| Phase | Estimate |
|---|---|
| Tauri shell, window, routing, state/IPC plumbing | 1–1.5 weeks |
| Read-only and settings pages (Library, Ideas, Goals, Feed, Search, Create, Tasks, Policy, Account, Local host) | 3–4 weeks |
| Chat: transcript, streaming, composer, attachments, side panel | 2–3 weeks (hardest, most bespoke) |
| Integration re-home (chooser, update, instance, images, voice, sign-in, widgets) | 1 week |
| DOM tests, a11y, packaging rework, Wayland gate | 2 weeks |
| **Total, before the parity-bug tail** | **~9–11 weeks** |

Risks, in order: (1) regressions in the newest, least-exercised surfaces — streaming transcript,
approval cards, local-host console; (2) losing the GTK widget tests that currently encode layout
guarantees; (3) a second WebKit in the bundle and a lower glibc baseline if Tauri 2's 4.1 is
adopted; (4) the P9 accessibility gate getting harder, not easier.

## Staging plan

Each stage is independently useful and leaves the app shippable. Stop at any gate that disappoints.

- **S0 — Lift the design language (days). SHIPPED Nov 20, 2026.** Kyth Hub's token set is in
  `apps/muse/src/theme.css` as `@define-color` (`hub_ground`, three surfaces, hairline pair, the
  `#36a3ff → #55e5c1` ramp, three-step ink, status colors) and styles the rail, the page sidebar and
  the chat shell; pages keep following the system theme. Gate met: Patrick reviewed it on his seat
  and asked for the glows removed, which is in. No architecture change, no lost tests.
- **S1 — Reference page in HTML (1–2 weeks). STARTED Nov 20, 2026.** Library is the reference page:
  `apps/muse/src/webui.rs` renders it in a `webkit6` view inside its own window, from
  `apps/muse/ui/{library.html,library.js,hub.css}` (`include_str!`ed, so no dev server, bundler or
  second engine), over the rows the native page last read (`library_snapshot()`). Read-only: the
  JS→Rust command channel is not built yet, and arrives with the first page that mutates.
  Two findings from the first run, both fixed in the same slice: WebKit's accelerated surface paints
  nothing capturable on a seat without working GL (the page forces
  `HardwareAccelerationPolicy::Never`, which is also the right default for a static local page), and
  a load-finished probe (`JSON.stringify({cards, rows, err, text})` to stderr) is what distinguishes
  a blank document from an empty one. Gate met for this page: 50 cards rendered from 50 rows, no JS
  error, side by side with the native page on the same data. Still to do for S1's full gate: the
  same page as React (today it is plain ES modules), and a DOM test suite.
- **S2 — Page migration (2–3 weeks).** Remaining read-only and settings pages, one at a time, behind
  the same GTK shell. Gate: every page's behaviour has a DOM test that replaces the GTK assertion.
- **S3 — Chat surface (2–3 weeks).** Transcript, composer, attachments, side panel. Last, because it
  is where a webview's feel differs most from GTK's. Gate: streaming, cancellation, approvals, quote
  staging and unread badges behave identically under the same manual script.
- **S4 — Shell decision.** Either keep the GTK chrome (rail, menus, dialogs, a11y) permanently, or
  replace it with a Tauri window. This is the only stage that forces a WebKit decision (4.1 now, or
  Tauri 3 later).
- **S5 — Packaging and gates.** AppImage recipe, Flatpak manifest, CI, native-Wayland and
  screen-reader checks re-run against the new build.

S0–S3 are the same work under either end state (GTK shell or Tauri shell), which is why they are
staged before S4 rather than after it.

## Decision recorded here

Not started, and not recommended as one step. The immediate step is **S0**, because it is the part
that addresses the actual complaint (the look) with no rewrite, and it produces the stylesheet every
later stage depends on. S1 is the cheapest experiment that can justify or kill the rest.

Revisit the full port if, after S1–S2, the web pages demonstrably cut layout churn (the last 120
commits contain 45 layout/metric fixes and 91 UI-touching commits) — that is the measurement that
would settle it, rather than another app's appearance.
