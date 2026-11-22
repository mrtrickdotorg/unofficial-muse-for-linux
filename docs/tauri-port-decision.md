# Porting the client to Rust + Tauri + React: analysis and staging plan

**Status: S0 shipped, S1 gate met, S2 seven pages, S3 slices landed (Nov 21, 2026; its paste and
  decide presses were checked live the same evening).** The decision was made to continue
toward the Tauri/React end state. Stage S0 (the palette lift) is in the tree; S1's reference page —
Library, read-only, in the existing WebKitGTK view with real rows — is implemented and verified on
this seat; S2 has its first page, Goals, on the same contract. Everything below the stage list is
the analysis that produced the plan.

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
  `apps/muse/src/webui.rs` renders it in a `webkit6` view inside its own window. The page is a React
  18 + TypeScript app under `apps/muse/ui` (Vite build, then `scripts/inline.mjs` folds CSS and JS
  into one `built/app.html`, `include_str!`ed — no dev server, bundler or second engine at
  runtime, and the built file is committed so a plain `cargo build` needs no Node).
  Data comes in as a boot payload inlined into the document, so the page paints from what the native
  page last read; `library.snapshot` then reads through `library_snapshot()`.
  **The JS→Rust command channel is built for this page** and is the pattern the rest will copy:
  a `UserContentManager` message handler named `muse` takes `{"id","cmd"}` and answers through
  `window.museReply(id, reply)`; every command is logged with its name. Commands read native state
  only, and a mutating command arrives with the first page that mutates.
  Two findings from the first run, both fixed in the same slice: WebKit's accelerated surface paints
  nothing capturable on a seat without working GL (the page forces
  `HardwareAccelerationPolicy::Never`, which is also the right default for a static local page), and
  a load-finished probe (`JSON.stringify({cards, rows, boot, err, text})` to stderr) is what
  distinguishes a blank document from an empty one.
  Three fixes from comparing the two pages on the same data: the web view's entry point now starts
  insensitive and is enabled only once a listing lands, so the page cannot open claiming the library
  is empty while the native fill is still in flight (a slow listing here took 27 s); the header
  reads the service's own `total_count` ("50 of 127 items") instead of the page count; and the fill
  keeps that total beside the rows for the channel to read.
  Refresh is honest about its name: `library.snapshot` answers a page open from the held rows, and
  `library.refresh` reads the service on a worker thread and answers when that listing lands (a
  listing has taken 27 s here, so the page allows 120 s for it and shows "Refreshing…"). A refusal
  or a timeout reaches the page as a failure banner, never as an empty library.
  S1's gate is met for this page: 50 cards rendered from 50 rows, a snapshot on open, a wire read on
  Refresh, no JS error, side by side with the native page on the same data -- and a DOM suite of six
  tests (`apps/muse/ui/src/Library.test.tsx`, vitest + jsdom) that checks the boot rows and total,
  the not-loaded state, the wire refresh, a refused refresh, tab+search filtering, and an empty
  filter result. The `Web UI` CI job runs them and fails if the committed `built/app.html`
  differs from a fresh build.
- **S2 — Page migration (2–3 weeks). COMPLETE: Local host is the ninth page (Nov 21, 2026).**
  Remaining read-only and settings pages, one at a time, behind the same GTK shell. Goals copies
  S1's contract: `open_goals_window` renders a boot payload of the roster and the Tracking section
  the native fill last held; `goals.snapshot` answers from those held rows, `goals.refresh` reads
  both listings from the service on one worker thread and updates the cache. A Tracking read that
  fails travels as `tracking_error` with the roster, so the page never reports an unread section as
  an empty one. The page mirrors the native roster: one screenful of Tracking with an in-place
  "Show more", category filters built only from categories in hand, group headers where the group
  changes, the service's completion state on a disabled box, and the same 240-character body
  clamp. Two changes came with it: the reply wait is a 100 ms timer instead of an idle handler
  (an idle poll spins a core for the whole of a slow listing), and the bundle is now shared by both
  pages (`built/app.html`, the boot payload names the page), so S3's pages need no second artifact.
  Tasks is the third page, and the first whose sections are not all one listing: `tasks.snapshot`
  answers the schedules and runs the native fill held plus a re-read of the local reminder store
  (local state, so no wire trip), and `tasks.refresh` reads the service on a worker thread and
  merges the local rows back in on the main loop. Rows carry the native page's own labels
  (`schedule_label`, `run_label`, `reminder_row_text`), so date and status wording cannot drift
  between renderers, and a `loaded` flag separates an account with no schedules from a page that
  has not read them — a distinction an empty list alone cannot make. The Goals slice also got a
  correction: a failed Tracking read used to clear the web cache's rows; it now keeps the rows the
  native page still shows, in the cache and in the reply, and reports the failure beside them.
  Ideas is the fourth page and the first that shares its row code: `ideas.snapshot` and
  `ideas.refresh` follow the same contract, and the roster's group order and body clamp live in
  one `apps/muse/ui/src/listing.ts` used by both Ideas and Goals, so the two cannot drift. Its rows
  carry the service's own icon (the native default when none), and `loaded` again separates an
  empty account from an unread page. The host-side GUI helpers this migration needs for visual
  checks -- screen capture of one window, a click, window close/raise/unminimise, and a launcher
  that waits for a viewable window -- now live in `scripts/x11-shot.py`, `scripts/x11-click.py`,
  `scripts/x11-window.py` and `scripts/muse-gui.sh` instead of a temp directory.
  Threads is the fifth page and the first whose row wording is projected natively rather than
  queried per field: `fill_threads` already computes each row's day group, snippet-and-clock meta
  line, pin order and side-chat split with the helpers the native list uses, so the cache is
  filled by that same paint (`held_threads_from`) and the page cannot word a row differently from
  the list beside it. `threads.snapshot` answers from those rows; `threads.refresh` re-reads the
  account listing -- the expensive connect read, which is why it leaves the main loop -- and the
  reply and cache update together. The page keeps the native shape: pinned rows above the day
  groups, one header per group, unread badges from the listing's own count, side chats under their
  own header, and `loaded` again separating "no conversations" from "not read yet". Seven DOM
  tests (`Threads.test.tsx`) cover the groups, the unread and meta line, both empty states, a wire
  refresh, a refused refresh and a failed read beside held rows.
  Feed is the sixth page, and the first with service-supplied graphics. The native card's
  attachment renders in a locked-down WebKit view (scripts on, navigation ignored, links handed to
  the system browser). The web slice first tried the same shape as an `<iframe srcdoc>` with
  `sandbox="allow-scripts"`; on this build's WebKitGTK the frame never painted — the probe found
  the attribute set and the frame's document empty — and a sandboxed `data:` URL frame came back
  empty too, so the page fell back to plain DOM: each graphic is inserted into its own **shadow
  root**, which keeps the service's `<style>` inside the widget, never executes its `<script>`
  (`innerHTML` does not run scripts), and lets a card-sized element grow to the graphic's natural
  height. A click guard cancels presses on links, so a graphic cannot navigate the window away;
  links inside a graphic are therefore inert here, where the native frame hands them to the
  desktop, because the page has no channel to launch one. A graphic renders live only on the first
  two graphical cards (`widget_available` on this host), the rest keep the service's extracted
  text, exactly as the native paint decides. Heroes are the
  other new shape: a document loaded from `about:blank` cannot read the cache's `file://` images,
  and inlining 34 heroes in the boot payload would carry megabytes, so `feed.hero` answers a
  cached image as a base64 data URI, fetched per card only when the card asks, and cache-only —
  the native fill owns the download. Card wording is projected natively again: `held_feed_from`
  reuses `feed_meta_line`, `relative_time`, `feed_row_cta` and `feed_body_markup` (so the body's
  links arrive resolved, as the GTK label's markup has them), and `fill_feed` fills the cache on
  both the cached and the fresh path, enabling the page's button only once a listing exists. Ten
  DOM tests (`Feed.test.tsx`) cover the prompt and status, the card order and native wording, the
  shadow-root graphic and its fallback text, the body's link markup, the hero data URI, both empty
  states, a wire refresh, a refused refresh and a failed read beside held cards. A card's service
  action reads as a label, not a button: the slice cannot run the action, so it cannot offer a
  control that does nothing. `feed.hero` is cache-only by construction — it goes through
  `fetch_hero_cached`, which refuses every non-http(s) address (a `file:` path can never be read)
  and only calls `is_file()`, so a page open issues no download; the download stays with the
  native fill (`fetch_hero`) — and the lookup and the read now run on a worker thread
  (`Answer::Later`), so a card asking for its hero never puts file I/O on the UI thread.
  The native frame got the same correction the web slice needed: `widget::document` paints a light
  shell (`#ffffff` ground, `#14161a` text, `color-scheme: light`), because the service authors its
  widgets for a white page and their near-black text disappeared into the old `#1e1e1e` shell. Two
  more native faults came out of that check: the frame's measured height was applied with
  `set_min/max_content_height`, which a `Never`-policy scroller ignores for its own allocation (the
  frame laid out at zero and painted nothing), so the fit now sets an explicit `set_size_request`
  and the cage carries a white ground; and a load failure now names its URI and error in the log
  instead of collapsing to a silent blank.
  Search is the seventh page, and the first whose native side is a query rather than a listing. It
  keeps the contract and adds the read that page needs: `search.snapshot` answers the query and rows
  the native search last painted (a boot payload carries both, so the page shows what was searched,
  not just what came back), and `search.run` takes the reader's own words in the request, runs the
  account read on a worker thread and updates the cache the native page shares. An empty query is
  refused without a read, exactly as the native page refuses it, and a failed read reports beside
  whatever rows are held. Rows carry the conversation id the native row uses to open Chat and are
  dropped without one, exactly as the native paint drops them, so the two lists cannot disagree;
  the web row itself is read-only, since this slice has no Chat to open. Because the page
  can read on its own, its Web view button is live from the start rather than waiting for a native
  fill; the page says plainly when no search has run yet. Eight DOM tests (`Search.test.tsx`) cover
  the held query and rows, the not-run state, a search carrying its words, the empty query, a
  refused read, a failed native read, the no-matches sentence, and the sentence for matches the
  service counted but this build could not map to rows — the live account reports `total` 6 and 20
  with zero rows for every query tried, so the page must not read that as "no matches". That shape
  gap is in `muse_account::search`'s mapping, not the page: the native list swallows the same rows. The live check also
  needed a way to type: `scripts/x11-type.py` joins the other host-side helpers (XTEST keys,
  `--clear` for select-all first), since this host has no xdotool.
  Settings is the eighth page, and the first whose native side is held state rather than a listing.
  `settings.snapshot` answers the saved link's interpreter and session folder (read fresh -- one
  small file), the updater's last answer (cached by `paint_updates`, so the page and the Updates
  section cannot tell different stories), and the policy the chat panel last painted, cached by
  `paint_policy` through the panel's own flip table -- so the only rows the page names a flip for
  are the rows a press there would flip. It adds no wire read and no write: the three presses that
  change anything (check, download, restart) and Sign out stay on the native page, and the page
  says so in words rather than offering a control that does nothing, the same rule the Feed card's
  action follows. `link` carries paths only; the session jar never reaches a page. An unlinked
  clone reads as "no saved link" rather than a failure, and an unread policy travels as
  `loaded: false` with the panel's own note ("Link your account first."), so an empty section is
  never mistaken for an empty service. A Settings visit asks the chat controller for the policy
  read when the cache is still empty -- otherwise the page would sit on "not read yet" until
  someone opened Chat, which the live check found on a fresh launch. That answer is pushed rather
  than polled for: `settings_policy_cache` reports whether the projection actually changed,
  `paint_policy` calls `webui::settings_policy_changed()` when it did, and that evaluates
  `window.musePolicyReady && window.musePolicyReady();` in the Settings window, whose page re-asks
  `settings.snapshot`. The window registry holds one weak reference per page, so a closed window
  needs no cleanup and a page without an open window is simply not nudged; a repaint that paints
  the same policy nudges nobody. The page's four-retry timer is gone: an unread policy is a state
  to show, not a reason to guess how long a read will take, and a policy that changes while the
  window is open now reaches it the same way. Ten DOM tests (`Settings.test.tsx`) cover the held
  link and rows, the app's nudge filling an unread policy (with exactly one read on open, so a
  poll would fail the test), the hook being removed when the window closes, a policy read's
  failure note, the staged-update sentence, the updater's own status, the unlinked clone, and a
  refused snapshot. The push was seen live on this seat: a throwaway startup hook opened the page
  while the chat panel's read was still in flight (`policy.loaded=false`), and the Policy section
  then filled with the service's 14 rows -- two flippable, the protocol table, the heading entry --
  with no timer in the page to do it. The live check on this
  seat rendered the service's 14 policy rows (two flippable, the protocol table, the heading entry
  it publishes as a row), both link paths and the checkout update line, with no WebKit console
  errors and no window-level faults.
  Local host is the ninth page, the last of S2's read-only set, and the one whose native side
  already owned a read model: `localhost.snapshot` answers the host controller's own
  `HostSnapshot` -- phase, notice, device id, granted folders, pending cards and the activity log
  -- projected by `held_localhost_from` and re-projected on every native paint. The host polls
  itself every 250 ms and each repaint lands in that cache, so the page follows the pump by
  re-asking its own snapshot: a cache read, not a wire trip, and the reason a card's countdown
  stays honest (the native side recomputes it per paint). A grant's line is `grant_row_text`'s
  wording, a card's body is `preview_content`'s clamp, the deadline is `approval_countdown`'s
  sentence, and the log is newest-first as the native list paints it, so the two renderers cannot
  describe one grant two ways. Nothing here decides anything: granting a folder, start, stop and
  revoke, Allow once, Deny, Clear log and the saved allow-writes policy all stay on the native
  page, and the card says where the decision lives rather than drawing a press that would bypass
  the one-time approval the boundary exists for. The snapshot's card array is named `pending`
  rather than `cards` because the boot payload names the page by shape and `cards` is Feed's key:
  the compiler refused a Feed boot that carried a Local host state, which is the argument for
  one distinct key per page. Six DOM tests (`LocalHost.test.tsx`) cover the held read model, the
  three empty states, a card's exact path and content with no press offered, a move's destination
  and the deciding state, a card that arrives on a later pump, and a refused snapshot; a Rust test
  pins the projection's borrowed wording and the log order. The page was rendered in the preview
  browser with a stand-in channel answering its own read from the boot payload, which caught one
  fault: the controller's ordinary status line was drawn in the error banner, and it is now a
  neutral line beside the phase, with the pink banner kept for a refused read. The live check on
  this seat then opened the real window: a startup hook (a throwaway patch, reverted before the
  commit) showed the native Local host page and opened its Web view, and "Local host · web view"
  (1040x760) painted from the host's held state -- the phase badge "not running", the controller's
  own "Disconnected. No folders shared with Muse." line, and the three empty sections -- with the
  probe reading the page's DOM back as
  `{"grants":0,"cards":0,"log":0,"phase":"not running","device":""}` after one
  `localhost.snapshot` over the channel. The native page beside it carried the same notice and the
  same empty lists, so the two agree on what an idle host looks like. The press itself is the one
  thing this seat would not give: injected input has stopped arriving here (under Xwayland
  `XTestFakeMotionEvent` and key events are ignored, and `XWarpPointer` moves only Xwayland's own
  pointer, so presses land wherever the compositor's pointer sits -- confirmed by a KWin script
  that activated the window and reported it active), so the hook made the two calls the button
  makes rather than a click making them.
  Gate: every page's behaviour has a DOM test that replaces the GTK assertion.
- **S3 — Chat surface (2–3 weeks). STARTED Nov 20, 2026: the transcript is ported, read-only.** This
  slice was pulled ahead of S2's tail (settings, local host): the churn measurement puts 126 of the
  app's ~185 geometry-changing UI commits in chat and shell, so this is the surface that can decide
  S4, while settings (25 commits, 19 geometry) and local host (8, 5) are the quietest in the tree.
  The page renders the native transcript's own three row shapes -- day dividers, bubbles (with the
  service's option pills and attachment chips) and notices -- plus the details panel's account,
  thread and composer status line, all projected from `chat::build_rows`, so the two renderers
  cannot word a turn differently. It is the first page with no refresh command: it follows
  `chat.snapshot`, refilled by every native render *including* the in-place rewrite of a growing
  reply, so the page costs no wire trips, cannot show a different conversation than the window
  beside it, and needs no second copy of the ticket discipline. It opens from the chat's turn menu
  ("Open in web view"), not the header row, so the port costs the chat header no pixel. Five DOM
  tests (`Chat.test.tsx`) cover the three row shapes and the option pills, the unread-thread state,
  the status line, a failed read beside held rows, and -- the reason this page exists -- a reply
  that grows keeping its element: the same DOM node takes the new words, which is the web half of
  the in-place rewrite.

  The page's composer is the slice's one mutating control, and it was built to the rule that made
  the shell worth porting at all: **one dispatcher**. `chat.send` dispatches nothing in `webui.rs`;
  it hands the trimmed draft to a closure the Chat window is opened with, and that closure is the
  controller's own `web_send`, which calls the very `send_text` a press in the app calls -- the
  same `Send` ticket slot, the same receipt closure, the same history reload and quick-poll burst,
  the same quota notice. `send_text` now returns what it did (`Dispatched` or a `Refused` sentence)
  so the page can report a refusal without inventing one, and a refusal keeps the draft. Two
  consequences worth recording: a page opened without the hook has no route to the service at all
  (there is one command entry point, `handle_command_with`, and the tests ask it with `None`), and
  `chat.send` is
  logged on every press like the other wire commands while the polling snapshot is still named
  once per window.

  Stop followed the same rule and needed one honest correction. It is the same control on the same
  circle, and `chat.stop` goes back through the app's own `stop_pressed`, which takes no session
  ticket at all -- the point of stop is that it fires while a send holds `Send`. But the app's send
  is a *dispatch*, about a second long, while the reply streams for many seconds after it: measured
  in the running app, the mirrored `sending` flag was never observed as true, so a page that only
  copied the native card's rule would have shown Stop for a window no reader could hit. Two fixes,
  both recorded here because they are the kind of gap a port hides: the app publishes the flag to
  the cache the moment it dispatches instead of waiting for the next render, and the page keeps
  Stop through the same beat in which it watches fast for the reply. With those, a press sent from
  this window and stopped a second later ended at `status: "Stopped."` -- the service's own
  `cancelled: true`, not an assumption -- and the transcript's last row stayed the reader's turn
  with no reply behind it. **Remaining for S3's gate:** attachments and their verified-send
  receipts, approvals and unread badges; the native composer is still where pictures and
  dictation live.

  Quote staging came last and cost almost nothing, because the pieces already existed: the page's
  Reply takes the turn's first line through the same cap the app applies, the bar above the composer
  is the app's own reply bar in page form, and the quote rides `chat.send` as data that the app
  guards with `staged_quote` -- one definition, shared with the composer's Reply -- before handing
  it to `send_text` as the native quote. No new command, no second dispatch route, and the quote
  clears on the same two events that clear the app's: the send goes, or the reader cancels. The id
  then rides the send as the stream's `in_reply_to`, the shape the shipped 12.1 client uses; the
  excerpt never travels. A live quoted send from the page (Nov 21, 2026) landed a row whose
  `payload.reply_to_message_id` was the quoted turn's id, with the service's own `reply_to_target`
  beside it, so the target is recorded, not inferred.

  Pictures, approvals and unread badges closed S3's gate (Nov 21, 2026), each by the same rule: the
  page gets no second copy of anything. **Attachments** came in as three commands over the app's own
  staging state. `chat.attach` is the `+` press -- it calls `paste_from_clipboard`, the app's own
  clipboard read, so the page never asks for bytes and the app never lends its clipboard to a
  document; `chat.unstage` drops one chip by name; and the chips themselves ride `chat.snapshot` as
  `staged` (name, pixels, bytes -- the numbers the native chip prints), published the moment the
  paste lands, because an async paste arrives after the render that opened the window. The send
  needed one honest change rather than a second path: `web_send` refused an empty draft, so a
  picture alone could not be sent from the page even though the native card allows it. The press is
  now armed on the app's own composer (`Composer::begin_remote`) -- same in-flight flag, same
  cleared quote, same taken chips, the app's typed draft left alone -- and the verified-send receipt
  needed no new surface at all: `stored_mismatch` already writes a send note, and the note is the
  `status` line the page already follows, so a picture the service stored differently is named in
  the page instead of claimed as sent. **Approvals** are projected from the list the native panel
  paints (`held_approvals_from`, reusing `approval_headline` and `decision_label`), carried in the
  same snapshot as an `approvals` view with `loaded` separating "none waiting" from "not read" and
  `note` carrying the panel's link-first sentence; `chat.approvals.refresh` is the panel's own read,
  and `chat.decide` checks the word against the service's five before `run_decision` -- one decide
  path, and a card leaves only when the app's refill no longer carries it. A press the app cannot
  take at all now answers with that reason (the panel slot is still finishing the last press, or the
  window is signed out) instead of an acceptance: `run_decision` reports it, the page prints it
  beside the card that is still waiting, and the card stays -- a dropped press used to answer ok and
  leave the card looking decided. The press names the approval as `approval_id`, never `id`: the
  envelope's `id` is the reply ticket, and a first cut that reused the name for the approval sent its
  answer to a ticket the page never had, leaving the card disabled until the request timed out. The
  page's channel now refuses any payload carrying `id` or `cmd` for that reason, and a DOM test pins
  a card that stays put with the app's sentence beside it after a refused press. **The unread badge** is
  the listing's own count for the open thread, the number the native sidebar badges the row with,
  shown beside the page title because this page has no list to hang it on; zero means no badge,
  never an invented count. Five DOM tests (`Chat.test.tsx`, 24 in all) cover the chip and its drop,
  a picture-only send, a card's decision and the panel's re-read, the unlinked sentence, and the
  badge; the Rust side covers the projection (native headline, verbatim details, five words), the
  snapshot's approvals merge, every new route once through and refused when closed, and the remote
  send's own flag behaviour (145 app tests). Seen live (Nov 21, 2026): the Chat page opened on the
  same 32 rows the native transcript held (boot 32, rows 32, no script error), and a press of
  `Check approvals` in the page's panel reached the app (`chat.approvals.refresh` in the launch log)
  and repainted from "No approvals read yet." to "No pending approvals." once the app's own read
  landed -- the service reports none waiting on this account, and the page says exactly that   rather than an unread section. A picture put on the X clipboard by another process (a GTK3 owner
  offering `image/png`, 1222×901) was then staged by the page's `+`: the log line `chat.attach`, and
  a chip reading `pasted-1795305595283444620.png · 1222 × 901 · 77805 bytes` beside the page's own
  composer -- the same numbers the native chip prints (the 77805 is the staged file the app encoded
  from the texture, not the source PNG's 71128). And a card was decided from the page: a turn sent
  on the page's composer asking the agent to POST to `https://httpbin.org/post` left the panel
  carrying `Host httpbin.org`, `Method POST`, `Path /post`, `Policy tool_exec` with Allow once /
  Allow task / Allow site / Allow always / Deny; the Deny press logged `chat.decide`, and the app's
  refill then read "No pending approvals." with the card gone. That refill is the only statement
  this path makes about the decision -- the service's own sentence for a decide is not surfaced in
  the page, so the card's disappearance is evidence, not a printed claim. The page printed no
  refusal line, so the busy and signed-out sentences remain covered by their tests rather than by
  this pass.
  The composer's five writes got their one-press-one-write rule on the page (Nov 21, 2026), the
  rule the native card already keeps. Each control claims itself for the length of its write and
  ignores a second press while the first is unanswered: the `+` is not offered at all during a send
  (the app refuses a paste then, because the send already took its pictures), a chip's `×` holds
  every chip while a drop is in flight, Stop is one cancel per press and comes back for a later
  press once the app answers -- the native card allows a second stop, so the page does too --
  Check approvals is one read per press, and a decision holds every card until it lands, which is
  the native panel's own slot rule. The presses still reach nothing but the existing `WebRoutes`
  closures: `chat.send`, `chat.stop`, `chat.attach`, `chat.unstage`, `chat.approvals.refresh` and
  `chat.decide` land in `web_send`, `stop_pressed`, `web_attach`, `web_unstage`,
  `refresh_identity_and_approvals` and `run_decision` -- one sender, one clipboard read, one decide
  path, no route to the session slot or the composer. One sentence was corrected beside the
  guards: `web_attach` answered "The clipboard holds no picture to stage." when the real reason was
  a send in flight, and it now names that reason (the page asks before the wire anyway). Six DOM
  tests (`Chat.test.tsx`, 24 in all) pin a second press on `+`, on a chip's `×`, on Stop (twice: one
  write, then a second write after the answer), on Check approvals and on a second card while a
  decision is unanswered, plus that `+` is not offered mid-send; `installChannel` gained an answer
  delay so a suite can watch a control while its write is still in flight.
- **S4 — Shell decision. STARTED Nov 22, 2026: the shell is a web page.** Either keep the GTK
  chrome (rail, menus, dialogs, a11y) permanently, or replace it with a Tauri window. This is the
  only stage that forces a WebKit decision (4.1 now, or Tauri 3 later).

  **The shell, ported (Nov 22, 2026).** `Shell.tsx` is the native window's three columns with no
  native window around them: the rail, the page list and one page's content, in a document of its
  own. It is opened from the native drawer's `Shell` button and asks nothing new of the pages, each
  of which already asks its own `*.snapshot` over the same channel, so the shell cannot show a page
  anything but the state the app holds -- there is no shell-side copy to go stale. The two lists it
  draws come from Rust: `pages::shell_pages()` is built from `pages::PAGES` and `pages::title()`, so
  a page cannot appear in the rail and go missing from the page list; the drawer also prints the
  account and the build stamp the native drawer carries. The rail's icons are the app's own SVGs
  (`apps/muse/assets/icons`, read with Vite's `?raw`), not a second icon set: while both rails
  exist they look the same. A page with no web component is still listed and named as such ("This
  page has no web view yet") rather than hidden, which is the honest state for anything the port
  has not reached. Four DOM tests pin the rail and drawer counts to the app's list, the first page
  opening, a press mounting the ported component and asking *its* snapshot (and unmounting the
  page it replaced), the mark moving in both lists, and the missing-page wording. Live on this
  seat: the shell window opened from the running app, `{"rail":10,"drawer":10,"current":"Chat",
  "title":"Chat"}` from its own probe, the chat page drawn from the app's held state
  (`/tmp/muse-shell.png`, `/tmp/muse-shell2.png`).

  What the shell still lacks to *replace* the native window, in the order it has to happen: the
  native-only writes (Settings policy flips, Local host grants and start/stop) have no channel
  route, so those pages are read-only here and say so; the drawer's History rows, the search and
  new-chat entry points, the dialogs (updates, sign-in, first run) and the keyboard map are still
  GTK widgets with no page; and the window itself (menu, focus, a11y pane) is GTK's. Removing GTK
  is the last step of that list, not the first: deleting `pages.rs`'s builders today would delete
  the reference the port is checked against on live data, and the 153 Rust tests that assert the
  native rows are still the only assertion of most page behaviour. The Tauri spike below prices the
  window swap that has to land before a single line of GTK can go.

  **Tauri 2 spike (Nov 21, 2026, throwaway, outside the tree).** Three source files on tauri 2.12.7 /
  wry 0.57 / tao 0.37 loaded the unmodified `apps/muse/ui/built/app.html` with the same
  `/*__BOOT__*/` substitution `document()` does, served over `tauri://localhost`, and it rendered:
  the app's own `CHAT_PROBE` came back through the channel as
  `{"rows":2,"boot":2,"channel":true,"dpr":1,"err":"","ua":"... AppleWebKit/605.1.15 ..."}` with no
  script error, under `libwebkit2gtk-4.1.so.0.21.10` (WebKitGTK 2.52.6, GTK 3.24.41, libsoup 3.4.4)
  with its own `WebKitWebProcess`/`WebKitNetworkProcess`; the Xwayland window was also captured as a
  picture of that page -- rows, composer, approvals panel, and the two status lines only a reply can
  paint. Timings: window built at 94-115 ms and the first page command at 688-735 ms on Xwayland,
  94 ms and 1164 ms on Wayland. **Two channel mappings worked.** Page to native: an
  `initialization_script` rebuilds `window.webkit.messageHandlers.muse` on
  `__TAURI_INTERNALS__.invoke("muse_command", { request })`, and because it runs before the page's
  module the page's own 2 s `chat.snapshot` poll arrives through it. Native to page: both resolving
  the invoke's return in the shim and the app's own `window.museReply(id, reply)` through
  `WebviewWindow::eval` with `postMessage` fire-and-forget, which the spike's second build used
  alone -- so the page's ticket protocol survives unchanged. That matters because `Answer::Later`
  polls the GTK main loop over `Rc<Chat>` state that is not `Send`: an async command is not a
  drop-in, an eval from the main thread is. Boot: the substitution was then moved into a custom URI
  handler (`muse://localhost/chat`), so one template is served per request with its own payload --
  two windows in one run got `served_to: "chat"` at 1133 ms and `served_to: "library"` at 1173 ms
  from 186,991- and 186,447-byte documents of the same inlined bundle, and both rendered from it
  (the chat window's two rows, the Library window's `1 of 1 items` card), which also showed the
  page's own mount re-read replacing boot data exactly as it does under the app. The navigation lock
  maps to `on_navigation`. Risks: wry 0.57 has no
  `HardwareAccelerationPolicy` equivalent -- the setting the GTK view uses to stay capturable on a
  GL-less seat -- leaving only the `WEBKIT_DISABLE_*` environment variables (this seat captured fine
  without it); a Tauri shell would run GTK3/WebKitGTK 4.1 beside the GTK4/6.0 app, two renderers for
  one page; and the page gains an origin (`tauri://localhost`) plus `__TAURI_INTERNALS__`, while
  app-defined commands are not gated by Tauri's capabilities, so one command named `muse` routed
  into `handle_command_with` stays the only gate.

  **Wayland presentation is verified.** KWin's own `ScreenShot2` refuses an
  unprivileged caller (`Error.NoAuthorized`), and neither `grim` nor `spectacle` is installed, so
  the capture went through `org.freedesktop.portal.Screenshot` with `interactive: false` -- the GTK
  portal backend is the only one this seat registers -- and it returned a full-screen PNG with no
  prompt: the wry window on `kwin_wayland`, page drawn (title, both rows, composer, approvals
  panel), at `window built at 88 ms (gdk backend: wayland)`, 1107 ms to serve the document and
  1158 ms to its first command. It is the desktop through the portal rather than a per-window API,
  and the portal writes into `~/Pictures`; the file was moved to `/tmp` for this check.

  **S4 decision table.** The page work is common to all three -- S2/S3 port pages into WebKit
  views whoever owns the window -- so only the shell, its integrations and its rework are priced.
  Estimates are the doc's above; the numbers are this seat's spike.

  | | Stay on GTK 4 / WebKitGTK 6.0 | Tauri 2 now (GTK 3 / WebKitGTK 4.1) | Wait for Tauri 3 |
  |---|---|---|---|
  | **Cost** | No shell work. S2/S3's page work is the shared cost; S5 packaging and the parity-bug tail remain. | The doc's ~9-11 weeks, of which 5-7 (read-only 3-4, chat 2-3) staying also needs; the Tauri-only delta is ~4-4.5 (shell 1-1.5, integrations 1, tests/a11y/packaging/Wayland 2). | Nil until it ships, then the same ~4-4.5 delta and a second S4; a draft, breaking milestone with no date. |
  | **Engine split** | One engine: the bundled WebKitGTK 6.0 the Feed frames and Library previews already use. | Two generations while migrating; at the end either a second WebKit in the bundle or the Feed frames forced down to 4.1 (the doc's risk 3). | One engine, GTK4 + 6.0, in one process -- the only option that removes the split and keeps GTK4. |
  | **a11y** | AT-SPI on real widgets; the 98 GTK assertions stay authoritative until each page's DOM test replaces them, and P9 is unchanged. | ARIA on 4.1's tree, "weaker than GTK's", chrome a11y from GTK 3; the doc's risk "(4) the P9 accessibility gate getting harder". | ARIA on the 6.0 tree with no second engine; the DOM cost matches Tauri 2, the chrome is no worse than today. |
  | **Wayland risk** | Works on this seat today; S5 still owes the "native Wayland, not XWayland" gate either way. | The doc's blank window / `AcceleratedSurfaceDMABuf` / Wayland `Error 71` hit NVIDIA and Wayland first. Spike: window built 94-115 ms, 1164 ms to first command against 688-735 ms on Xwayland, layout proven and **presentation verified** by a portal capture (KWin's own `ScreenShot2` refuses with `Error.NoAuthorized`; no `grim`/`spectacle`), and wry has no `HardwareAccelerationPolicy` guard. | Same class as Tauri 2 minus the 4.1 gap; nothing measured on it yet. |

  Reading the table: staying is the only option whose cost is already being paid and the only one
  with no engine split. Tauri 2 is the only one available now, and its price is a second WebKit
  generation and a harder a11y gate rather than a shorter project. The table moves only if S1-S3
  demonstrably cut layout churn (the measurement the decision below waits for), or if Tauri 3 ships
  a released GTK4/6.0 target that can be measured on this seat.

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
