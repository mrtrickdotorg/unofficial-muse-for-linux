# Web UI slice (React)

The pages of the Tauri/React migration so far, rendered by the app's own
WebKitGTK view: **Library** (the reference page), **Goals**, **Tasks**,
**Ideas**, **Threads**, **Feed**, **Search** and **Chat** (S3's first slice:
the transcript the native list holds, followed live). Sources live here; `built/app.html` is the artifact the Rust side includes
with `include_str!`. One bundle serves every window — the boot payload names
the page it opens.

```sh
cd apps/muse/ui
npm install          # once
npm run build        # tsc --noEmit && vite build && inline into built/app.html
```

`scripts/inline.mjs` folds the Vite output into one HTML file because the page
is handed to WebKit with `load_html()` — a `<script src>` or `<link href>`
would resolve against `about:blank` and fail. There is no dev server and no
second browser engine in the bundle.

`built/app.html` is committed so a plain `cargo build` (and the AppImage
container) needs no Node toolchain. Rebuild it whenever `src/` changes; the
`Web UI` CI job rebuilds it and fails if the committed copy differs.

```sh
npm test             # the pages' DOM tests (vitest + jsdom)
```

The page talks to Rust through `window.webkit.messageHandlers.muse`: each call
posts `{"id","cmd"}` and the native side answers with
`window.museReply(id, reply)`. Commands read native state only; a mutating
command arrives with the first page that mutates.

| Command | Cost | Use |
| --- | --- | --- |
| `library.snapshot` | none — the rows the native page holds | opening the page |
| `library.refresh` | one listing read on a worker thread | the Refresh button |
| `goals.snapshot` | none — the roster and Tracking rows the native page holds | opening the page |
| `goals.refresh` | one roster read plus one Tracking read on a worker thread | the Refresh button |
| `tasks.snapshot` | none — the schedules and runs the native page holds, plus a local store read | opening the page |
| `tasks.refresh` | one service read on a worker thread, plus a local store read | the Refresh button |
| `ideas.snapshot` | none — the roster the native page holds | opening the page |
| `ideas.refresh` | one ideas listing read on a worker thread | the Refresh button |
| `chat.snapshot` | none — the transcript the native list holds | opening the page, then every 2 s |

Chat is the first page with no refresh of its own: it follows
`chat.snapshot`, which the native render refills — including the in-place
rewrite of a reply that is still arriving — so the page costs no wire trips
and cannot show a different conversation than the window beside it. It is
read-only on purpose: sending, attachments, approvals and quote staging need
mutating commands, which arrive with the increment that adds the composer.

Refresh is a real read of the service, not a re-read of the cache, and a slow
listing is why it waits: the pages allow 120 s for that call. The native reply
is collected by a 100 ms timer on the main loop, not an idle handler, so a
27 s listing costs a handful of wakeups instead of a core. A reply of
`{"ok":false,"error":…}` reaches the page as a failure banner, and the rows
already on screen stay.

`goals.refresh` answers the roster and the Tracking section together, the way
the native fill reads both under one page ticket. If only Tracking fails, the
roster still arrives and the reply carries `tracking_error` beside the held
Tracking rows, which stay in the cache: a failed read leaves a section stale,
not empty, and the page says so rather than dropping the rows the native page
still shows.

`ideas` rows share `src/listing.ts` with `goals`: the same group order
(category, then kind, then kicker) and the same 240-character body clamp, so
the two rosters cannot drift. `loaded` separates an empty account from an
unread page, and a failed read rides beside the held rows.

`tasks` rows carry the native page's own labels (schedule title plus on/off,
run status plus summary, the reminder line), so the two renderers cannot drift
on wording. `loaded` separates "this account has no schedules" from "this page
has not read them yet", and a failed read is kept beside the rows rather than
replacing them. The local reminders come from their store, not the wire, so
opening the page or refreshing re-reads them on the main loop.
