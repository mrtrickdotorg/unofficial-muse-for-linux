# Web UI slice (React)

The pages of the Tauri/React migration so far: **Library** (the reference
page), **Goals**, **Tasks**, **Ideas**, **Threads**, **Feed**, **Search**,
**Chat** (S3: the transcript the native list holds, followed live, plus a
composer that sends) and **Shell** (the rail, the page list, History and one
page at a time — the app's own window). Sources live here; `built/app.html` is
the artifact the Rust side includes with `include_str!`. One bundle serves
every window — the boot payload names the page it opens.

The shell page is what the app opens: `apps/muse-window` is a Tauri 2 window,
and the app spawns it and answers its commands over pipes (`apps/muse/src/web_shell.rs`).
Its boot payload is the app's first paint, and the account line and History
are filled by the app's own reads after it, so the window hands the drawer the
later answer through `window.museShellBoot` — the same nudge Settings uses for
an unread policy, and the reason the page needs no timer of its own.

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
| `chat.send` | one message, through the app's own send path (the `Send` slot) | the Send button, or Enter |
| `chat.stop` | one cancel, outside every session slot (so it fires while a send holds `Send`) | the Stop button |

Chat is the first page with no refresh of its own: it follows
`chat.snapshot`, which the native render refills — including the in-place
rewrite of a reply that is still arriving — so the page costs no wire trips
and cannot show a different conversation than the native window beside it.

Its composer is the slice's one mutating control, and `chat.send` dispatches
nothing itself. The command hands the trimmed draft back to the app's
controller through the closure the Chat window is opened with, so a press here
takes the same `Send` ticket slot, the same receipt handling and the same
history reload as a press in the app: there is exactly one sender, and a send
from this window cannot race one from that one. The answer says only whether
the app took the press — a held slot or a signed-out window comes back as the
app's own sentence and the draft stays — while what the service said arrives
the way everything else does, in the transcript and the status line.

Stop is the other half of that, and it is built the same way: it takes Send's
place on the same circle while a send is in flight, and `chat.stop` goes back
through the app's own `stop_pressed`, which takes no session ticket at all —
that is the point of stop, since it has to fire while a send holds the `Send`
slot. The app's send is a dispatch that ends in about a second, so the page
keeps Stop a beat longer, through the same window in which it watches fast for
the reply; otherwise a reader could never reach it. The answer says only that
the cancel went out, and the app writes the service's own verdict on its status
line: "Stopped." when something was running, "Nothing was running." when the
press was a little late.

Reply stages a quote on the page — the turn's first line, trimmed, with the ×
that drops it — and the quote rides `chat.send` as `{id, excerpt}`. The app
guards it with the same `staged_quote` the composer's own Reply uses and passes
it into `send_text`, so a quote from the page is the native quote and not a
second notion of one; the page clears it on a send that goes and on a cancel,
which are the two things that clear the app's. The id then rides the send as the
stream's `in_reply_to`, so the service can record the target; the excerpt stays
in the bar. Pictures, approvals and dictation remain in the app window.

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
