# Mac parity roadmap — priority order

The acceptance contract is [the full-port goal](full-port-goal.md); the row-level state is
[the feature matrix](feature-parity.md). This page decides **ordering**: what to build next, why it
comes before the rest, and what proves it done.

## What "mimic Muse for Mac" actually decomposes into

The Mac client is not one thing. It is two layers, and only one of them is ours to build.

1. **A client for a cloud agent.** The agent, its files, its browser, its memory and its history live
   on the existing Muse Secure VM. The client reads and writes that through the account gateway. Our
   six-operation community adapter (`connect|identity|history|watch|tasks|send`) is a thin slice of
   this layer.
2. **Local reach.** The Mac app holds macOS permissions (Full Disk Access, Automation, Notifications),
   per-app `Off / Read only / Read and interact`, and lets the cloud agent act on the local machine.
   Our equivalent is the account-linked local host: `POST /link-control`, a narrow manifest, and a
   Rust permission broker.

Two consequences drive the whole plan:

- Anything that lives server-side (Sentinel enforcement, the Secure Credentials Store, the Confidential
  VM, usage metering) cannot be mimicked by a client. We can only surface it.
- Every feature below needs a **gateway route we do not control** before it can be built. So the first
  step of each phase is route reconnaissance through the pinned community source. If the route is not
  there, the phase is recorded blocked — never simulated.

**Already built, do not rebuild:** the persistent account host (~0.1 s operations instead of a 3 s
handshake), typed errors with the no-echo rule, private-only session storage, the descriptor-pinned
`openat2` broker, ship-on-every-push AppImage CI. This plan spends none of its budget on those.

## Current blockers

- **Fresh sign-in — proven live.** An isolated-browser sign-in completed against the real service; the
  read-only probe then returned `connected=true threads=4 history_messages=20 schedules=32 runs=25`.
  Expiry and revocation now have their own state in the UI instead of a silent failure.
- **Live registration and first invocation — proven live.** Pressing Start produced a real
  registration acknowledgement (`Registered with Muse`) for a granted scratch folder. `node.list` then
  showed exactly one Linux node, id `linux-f818441e5fe79c3fd56ad17a5d64a6ce` (the value persisted on
  disk), `name='Linux'`, `platform=linux`, `device_family=homehub`, `online=True`, advertising
  `files.create_text`, `files.read_text` and `files.search`. Asking the agent to read
  `read-me.txt` in the granted folder returned its contents, so an agent-to-Linux read round-tripped.
  Neither headline claim is open any more.

Everything below therefore begins with **route reconnaissance**, not with a UI. The pinned community
client carries a static route table (`muse_mcp.client.ROUTES`, 258 entries); a route existing there is
permission to *try* it, never proof that a personal session is entitled to it.

## Route availability

Reconnaissance result, recorded so later phases start from facts rather than guesses. "Table" means the
route is declared in the pinned client; "live" means an authenticated probe has actually answered.
Live today: `identity`, `chat.history`, `sessions.list`, `tasks.list`, `tasks.runs`, `activity.list`
(`days=3`), `skills.list` (68 skills), `subagents.list` (`active=0 recent=20`), `artifacts.list`
(76 artifacts, 50 per page), `spaces.list`, `goals.list`, `model.get` (`auto/auto`) and
`voice.available_voices`. `memory.get` and `fs.list` answer only with a JSON body (`memory_uri`, and a
relative path respectively); `browser_task.get` answered `browser task not found`, which is an idle
task rather than a missing route. `connectors.list`
answers **500 `internal_error`** — a service-side failure, so P7 is blocked on that bug rather than on
our entitlement; retry it before building a connector UI.

| Phase | Routes in the table | First live probe |
|---|---|---|
| P1 conversation | `chat.stream`, `chat.stream_events`, `chat.subscribe`, `chat.cancel`, `chat.poke`, `chat.history_window`, `chat.sxs.select`, `chat.search`, `chat.mark_seen`, `chat.delete_turn`, `chat.unread_count`, `activity.list/get/subscribe`, `subagents.list/status`, `sessions.get` | `activity.list` (GET), then `chat.history_window` on a known thread |
| P2 approvals | `egress.permissions.*`, `egress.approvals`, `egress.approval.decide`, `egress.approval.history`, `permissions.settings*` | `egress.permissions`, `permissions.settings` (GET only) |
| P3 local reach | none needed — manifest and broker are ours | n/a |
| P4 schedules | `tasks.list/runs/delete/subscribe`, `pin.update`, `pin.reminder_confirm` | `tasks.subscribe` once create/cancel exist |
| P5 artifacts | `artifacts.list/fetch_url/share/remove/pin/subscribe`, `fs.list/read/stat/search/library/export/subscribe/write/rename/delete`, `spaces.*` | `artifacts.list` (GET) |
| P6 memory | `memory.get/search/consolidate/reindex`, `goals.*` | `memory.search` |
| P7 connectors | `connectors.list/get/accounts/permissions/*`, `custom_connectors.*`, `channels.*`, `skills.list/disconnect/set_disabled` | `connectors.list`, `skills.list` |
| P8 browser | `browser_task.get/present/takeover/release/stop/close/heartbeat`, `browser_stream.attach`, `browser_activity.snapshot` | `browser_task.get` on an idle task |
| P9 shell | `node.register`, `node.list`, `nodes.sync`, `node.heartbeat`, `node.unpair`, `nodes.invoke_ack/result`, `client.register_capabilities` | `node.list` |

Two earlier assumptions are corrected by the table: **model choice does exist** (`model.get`/`model.set`),
and **desktop voice may be reachable** (`voice.dictation?sample_rate_hz=`, `voice.available_voices`,
`voice.start_call`). Neither is proven for a personal session, and voice must not be claimed before an
authenticated answer exists.

The local-host registration path is separate: `/link-control` is a headerless control stream, not one of
these 258 HTTP routes. `node.register` is the *other* device path and must not be conflated with it.

## Priority order

### P0 — Prove the foundation (S)

*Gap:* sign-in, revocation and registration are unverified; sessions expire silently; the adapter has
no capability probe, so drift appears as a confusing failure.

*Work:* complete fresh sign-in in the packaged app; session refresh and a clear expired-session state;
move the plaintext jar behind an OS credential store (`oo7`/Secret Service) with migration; ask the
gateway for its version/capabilities once and fail with a specific drift error; extend the read-only
`account-probe` to report which phases below have routes.

*Gate:* packaged app performs a fresh interactive sign-in, survives expiry and revocation with clear
states, and reports live registration for a scratch folder. Every later phase cites this evidence.

*Gate met:* fresh sign-in; expiry and revocation as explicit states; one persisted device identity per
installation (`local-host-node-id`, owner-only, staged-then-renamed); live registration of a scratch
folder; and a real agent-to-Linux `files.read_text` whose contents came back in the reply. P1 is
unblocked. Carried forward: an approval-gated *create* has still never been exercised by the real
agent (only by tests), and revocation mid-invocation is unproven against the live service.

### P1 — The live conversation (M)

*Gap:* the Mac app is explicitly not turn-by-turn — you can interject while it works, it shows
"Is working / Making something / Is updating memory", supports side chats, and an Activity log. We send
one prompt and poll a 45 s watch window. No streaming, no plan/steps, no cancel, no side chats.

*Work:* probe for incremental reply and step events; stream assistant output into the existing
transcript; expose plan/step progress and agent status; cancel and resume; side chats as scoped
threads; persist in-flight state so a restart does not lose the thread.

*Gate:* an answer appears incrementally; interjecting during work is accepted (accepted, not steering —
see below); cancel stops the work and is verified through history; restarting the app reattaches to the
same run.

*Cancel (2026-11-04).* `chat.cancel` is `POST /chat/cancel` with `noiseOnly`, and the pinned CLI never
calls it, so the bridge reaches the route directly instead of waiting for a library method. Probed
against the live service with nothing running it answered `{"cancelled": false, "message_id": null}` —
the service states whether anything was actually stopped, so the window reports that answer ("nothing
was running to stop") rather than treating a press as a stop.

*Mid-run shape (live, 2026-11-04).* While a turn produces, history already carries the assistant row with
**no status**: seq `72546`, role `assistant`, status absent, text "1. In August". A terminal status
(`completed` / `interrupted` / `failed` / `cancelled`) replaces it later. So "in flight" is exactly
`role == assistant && status not terminal`, and treating an absent status as finished would be wrong in the
opposite direction — the window's finish-check now pins both directions. Two probes then sent a second
message mid-turn: the service accepted it (`sent: true`), but in both the first reply had already reached a
terminal status when the second landed, so **steering a live turn stays unproven** at that point (it is settled
below, and not in the Mac app's favour), and one acceptance came
back `confirmed: false` — accepted but unconfirmed inside the 8 s window, which the window reports as
uncertain rather than as delivered. History does not stream either: the partial row sat at 12 characters and its
text did not change across ~50 s of reads, then jumped to the full 21,340 characters at completion. The
incremental channel is the **live stream**: a 20 s listen during a run returned `delta.text_append` events
carrying raw fragments (`"anti"`, `"-Sullan"`, `" government"`) with monotonic `seq`, alongside
`agent.status: is responding` and `task.status: running`. So an answer can appear incrementally, but only by
joining deltas — and repeated listens overlap, so de-duplication by `seq` is mandatory. That is what the
window's live draft does: join new fragments, never apply one twice, and drop the draft the moment the
transcript carries the finished reply.

*Gate item closed (live, 2026-11-04).* A prompt asking for 120 numbered lines was dispatched and
confirmed (`message_id 1f7a5d61…`), then `chat.cancel` was sent 4 s later and answered
`{"cancelled": true, "message_id": "2c84f68b…"}`. History then showed the reply at `interrupted` with
one item written ("1. One is the loneliest number…") and nothing further for 95 s — the run stopped
and the transcript says so. Two facts worth keeping: the id in the cancel answer is *not* the prompt's
id, so it identifies the interrupted reply rather than the request, and a cancelled turn closes as
`interrupted`, not `completed` — a finish-check that knew only `completed` would report a stopped turn
as permanently in progress. Reattach-after-restart is now evidence-driven rather than remembered. A reply row with no terminal status means
the service is still producing, so a window that opens mid-run opens a bounded watch (ten minutes) and listens
on `activity` in alternation with the history poll, streaming fragments into the draft and closing the watch
when the turn ends. Nothing is persisted locally, so a stale row cannot invent a run: the strip only speaks when
the live stream reports a working status, and a finished status opens no watch. The cost is that the transcript
still shows the partial reply, so the draft starts where the window did rather than where the answer did.
*Interjection (2026-11-06).* The composer no longer shares the read slot: a dispatch owns `sending`, so a
background refresh — or the turn the agent is still producing — cannot refuse the next message with "a request is
already running". That refusal was never about the running turn; it was `issue()` rejecting any operation while a
read held the one `busy` slot. One unresolved dispatch at a time is still enforced (two in flight could duplicate an
uncertain send), an uncertain receipt still blocks until acknowledged, and switching conversations still refuses
while a dispatch is unresolved. Proven by unit test: a send issued while a history read holds `busy` now returns a
ticket. The live half of the gate is the service's answer, not the composer's, so it is recorded separately below.

*Interjection does not steer (live, 2026-11-06).* The gate item is settled, and the answer is not the one
the Mac app advertises. The decisive run asked for the numbers 1–1200 spelled out; at 12.6 s the live stream
had emitted 194 `delta.text_append` fragments (5,776 characters) and stood at item **262 of 1200**, so the turn
was demonstrably mid-generation with most of its output still unwritten. The interjection — "STOP. Abandon the
number list right now … reply with only the word ORCHID" — was accepted at that moment (`sent: true`) and its
receipt came back `confirmed: false`: persisted, but not visible in history inside the 8 s confirmation window,
which is the uncertain case the window already reports as uncertain rather than as delivered. The running reply
did not change course. The stream continued through item 861 at 32 s and 1137 at 41.6 s, and history closed it
`completed` at 33,917 characters, ending "1200. one thousand two hundred". The interjection was answered
separately afterwards: a new assistant row of six characters, "ORCHID", `completed`, with its own user row
between the two. An earlier run shows the same shape from the other side — a 300-item list interjected at item
281 still finished at 300, with the ORCHID answer arriving as a later turn. So the service accepts a message
during work and processes it *after* the turn in flight; it does not redirect that turn, and the reply that
finishes is the one the earlier instruction asked for. The window's behaviour is therefore honest as built
(accept the message, show it in the transcript) but must never promise a redirect, and the composer being open
is not evidence that the agent saw the message. Steering, in the sense the Mac app implies, is a model-side
capability this service does not expose; the interjection is a queue, and the UI should say so if it labels one.

*Reattach proven live (2026-11-04).* Two probes drove a real run through the bridge. A history read 22 s into the
run returned exactly the shape the window keys on — `72826 assistant status=None`, 50 characters written — so a
window opening at that moment sees a reply still being produced. A listen *started fresh* at +24 s (nothing
carried over from before) returned 200 events: 196 `delta.text_append` carrying 3,959 characters, plus
`agent.status: is responding` and `task.status: running`. A window that attaches mid-run is therefore told work is
in progress and gets text, which is what makes the draft appear without the window that sent the message. The
fragments are forward-only from the attach point, not a replay: at +13 s of a second run the transcript row read
`'1. Ur'` while the first fragment was `' city of classical Greece'` (seq 80694, range to 80889), so the draft
continues from where the window attached instead of restating what the transcript already shows. Cancelling that
run closed the row at `interrupted` with 5,852 characters, and the cancel answer's id (`24011a78…`) again
differed from the send's (`64bda101…`), which is the interrupted reply rather than the prompt.

*Recon (live, 2026-11-04).* `chat.subscribe` answers for a personal session and delivers
`agent.status` (`activity_code`/`activity_text`, `session_id`, `is_thread`), `task.status`
(`status: completed`, `active_subagent_count`, `task_id`), `approvals.snapshot` and
`model_lock.updated`, each with a monotonically increasing `seq`. The pinned CLI records in its own
comments that **threaded replies never arrive as live events**, that live chat events use `delta.*`
shapes rather than `message.*`, and that only history carries the `reply_to` discriminator. So the
design is fixed rather than guessed: reply text stays on `chat.history` polling (exactly what `watch`
already does) and the live stream feeds **status, deltas and approvals**. Consequence for honesty:
incremental *text* cannot be claimed as token-by-token until a `delta.*` event is actually observed
during a run, and the status surface must not be derived from wall-clock guesses while `chat.subscribe`
is available.

*Verified through the product path.* The shipped `activity` operation, invoked as the bridge's own
JSON request, returned `model_lock.updated`, `agent.status` (`status: "Needs approval"`),
`task.status` (`completed`) and `approvals.snapshot` in one 6 s listen, and a 31 s listen was rejected
with `invalid_request` before any network use. The chat status line is still unobserved in a running
build, which is a display question rather than a transport one.

### P2 — The approval and policy engine (L)

*Gap:* the Mac app has five outcomes per request (Allow once / this task / this site / always / deny),
read-vs-write action classes, `Ask for some actions` vs `Always ask`, per-connector overrides, an
allowed-websites list, and enforcement outside the model. We have per-invocation Allow-once/Deny only.

*Work:* an action model (class × target × scope) shared by every capability; a durable policy store
with per-scope rules and expiry; enforcement at the broker, not in the prompt; the five-outcome UI;
an Activity log of decoded decisions, denials and outcomes; undo where the action is reversible;
global stop.

*Gate:* "always allow" persists across restarts and is revocable; deny leaves no side effect; a
decision detail that changes after the request invalidates the approval; the log answers "what did it
do to my machine" without secrets.

*Why here:* every later capability inherits this. Building computer use or connectors first means
rebuilding their consent paths afterwards.

*Recon (live).* `approvals.snapshot` already delivers `pending_approvals` and `recent_approvals` rows
carrying `approval_id`, `decision` (`allow_always` observed), `always_scope` (`destination_domain`),
`attention_mode` and `display.detail_rows`. That is the Mac five-outcome model's own vocabulary, so our
policy store should mirror those field names instead of inventing parallel ones, and the deciding route
is `egress.approval.decide`. Local broker approvals remain ours; only the *shape* is borrowed.

*Reading is reachable; deciding is a separate, unauthorised step.* A live `approvals.snapshot` carried
**7 pending approvals**, each an egress `CONNECT` request with `Host` / `Grants` / `Method` / `Path` /
`Policy` detail rows, while `recent_approvals` showed `allow_always`. The routes are bound to the
**sentinel** service, and the two gateways in the pinned source differ exactly there:
`muse_mcp/client.py`, which this bridge imports, has no service selection at all, so
`egress.approvals` fails with `DecodeError … 'hatch.noise.ServiceResponse'`; `muse.py`'s gateway routes
by `SERVICE_IDS = {"daemon": 0, "sentinel": 1, "vault": 2, "authd": 3}` and answered `egress.approvals`
read-only against the same saved session — **once**, then raised the identical `DecodeError` on a repeat.
The cause is in the reader, not the route: `_read_frame` parses every frame as `ServiceResponse`, so a
sentinel notification frame aborts the read. P2's prerequisite is therefore frame-type discrimination on
the sentinel connection, not another route probe; until that exists the read path is intermittent and
must not be presented as reliable. `egress.approval.decide` is `POST /approvals/{approval_id}/decide` on
the same service — a write that must go through an explicit user decision and P2's policy model, never a
convenience retry and never from a background refresh.

*Read path fixed and verified (2026-11-04).* The bridge now opens the sentinel service itself, through
`sentinel_gateway`: it imports the pinned `muse.py` gateway, which routes `egress.approvals` to service 1, and
replaces **only** the frame reader so a notification payload becomes an empty `ServiceFrame`. The parent request
loop already ignores frames whose `stream_id` is not the one it sent, so no request logic is duplicated and the
fix cannot drift from the pinned implementation. The new read-only `approvals` operation projects
`pending_approvals` into `approval_id`, `kind`, `decision`, `always_scope`, `attention_mode`, `title`, `summary`
and `detail_rows` (label/value pairs), keeping the service's own five-outcome vocabulary instead of inventing a
parallel one; `recent_approvals` is deliberately not exposed, and a row with no id fails closed as
`protocol_error` because it could not be decided safely. Live: `{"op": "approvals"}` answered `ok: true` in 4 s
with `pending: 0` — the first clean sentinel read through our own code, where the earlier attempts died with
`DecodeError`. An empty list is the present state, not proof that the route reports rows when they exist; the
projection's shape still rests on the earlier 7-pending snapshot. Suite: 92 tests, including the
notification-frame regression, the sentinel-route/close tests and the fail-closed id test.

*Large replies arrive in Noise chunks, not notifications (live, 2026-11-06).* The P2 read path's remaining
failure — `egress.permissions` answering with what `call_json` reported as `non-JSON response`, its quoted text
starting `{"ok":true,"result":{"hitl_debug_tools_enabled":…` — is a transport-frame bug, and it corrects the
diagnosis above. That body was never a notification problem. A live trace showed one reply arriving as **eleven**
wire frames, and the ones the reader could not parse were pieces of *one* `ServiceResponse` split across
`NoiseTransportFrame` chunks sharing a `chunk_id`: `egress.permissions` sent groups of three and four ~65,489-byte
chunks beside single-frame messages, `egress.approvals` sent two. The pinned `_read_frame` parses every frame as a
whole `ServiceResponse`, so a mid-frame chunk raised `Wire format was corrupt`; the sentinel guard then skipped it
as though it were a notification, which dropped exactly the bytes between chunk boundaries. The damage surfaced
late — `json.loads` failed ~450 bytes into the following chunk, so the error quoted a perfectly valid-looking
prefix and pointed at the payload instead of the reader. Reassembling by `chunk_index` before parsing yields
`egress.permissions` **496,150** bytes where the old reader assembled 145,814, and `egress.approvals` 80,620
against 73,654; both parse. Live after the fix: `egress.permissions` ok (`auto_allow` 5 flags, 130 connectors),
`egress.approvals` ok (10 recent), and `egress.approval.history` answers **50 items** where it previously
answered `{}`. Suite: 96 tests, three of them new — reassembly, index-not-arrival ordering, and two chunk groups
that must not mix.

*Allowed sites and revocation are reachable; not yet exposed.* The listing surface is `GET /permissions/active`
(`egress.permissions.active`): 50 `items` carrying `permission_id`, `kind` (`transient_network_grant`), `lifetime`,
`mode`, `policy_subject`, `expires_at_ms` and **`revocable: true`**, with `display.summary_title` for the row. The
durable "this site was allowed always" record is `egress.approval.history`'s `always_allow_registrable_domain`,
beside `host`, `port`, `decision` and `always_scope` — that is where the school domains in the history came from.
Revocation is `POST /permissions/revoke` (`egress.permissions.revoke`), with per-connector reset at
`DELETE /permissions/connectors/{connector}` and settings at `PATCH /permissions/settings`. None of these are wired
into the bridge or the window yet, and they should not be until P2's policy model exists: revocation is a write,
so it inherits `decide`'s posture — one explicit press, no background call, no retry, and the service's answer
reported rather than assumed. The permissions payload is ~496 KB, so any listing must project narrowly; the window
must never hold the raw body.

*Deciding is now implemented at the bridge.* `decide` takes an `approval_id` and one of the five published
outcome words (`allow_once`, `allow_task`, `allow_site`, `allow_always`, `deny`), refuses every other value
before any request is made, and sends exactly one `POST /approvals/{approval_id}/decide` on its own sentinel
connection. The answer reports that the service **accepted** the decision, plus the service's own word for it —
never that the approved action ran, because a 2xx is not an effect — and a decision is never retried. What is
human half is now wired too. *Check approvals* reads the pending list on demand — reading is never consent, so
an empty answer says "the service reports no approvals are waiting" and changes nothing. The list is also read
once when the status line first reports the agent is waiting (`Needs approval`), keyed to that exact status text,
so the alternating live reads cannot turn an unanswered request into a poll loop and a status that merely
describes work never fetches a list. Each request becomes a
card carrying the service's own title, summary, kind, offered outcome, remembered scope and every detail row
verbatim; summarising those rows would hide the action being approved. The card offers **Allow once** and
**Deny**, plus **Allow always** only when the service published an `always_scope`, so "always" can never mean a
scope this app invented. One press takes the exclusive slot, disables every card, sends one `decide`, and then
re-reads the list: a decision only leaves the card after the service accepts it, and the notice reports the
service's word ("accepted the decision (deny)") rather than claiming the action ran. A failed decision keeps the
request on screen and says to re-read instead of pressing twice. Window tests cover the leave-on-accept rule,
the keep-on-failure rule, the accepted-is-not-allowed wording, and the empty-read notice. No live decision has
been pressed: no approval was pending at the time, and pressing one is an irreversible write for the person
whose account it is.

### P3 — Reach the capabilities we already have (M)

*Gap (2026-11-06):* the registered manifest now advertises six file commands. `files.edit`,
`files.rename` and `files.delete` exist behind approvals with read-back verification — an edit is
approved as the complete resulting text and refuses a file that changed since the request, a rename
uses `RENAME_NOREPLACE`, and a delete is a Trash move that never unlinks. Still unreachable: screen
capture, input/app control and notification send (capabilities we run locally that the agent cannot
ask for), cross-root `files.move`, overwrite, and per-root `read`/`read+interact` modes.

*Work:* expose screen capture and the RemoteDesktop input path through the manifest; decide the
notification channel; per-root modes instead of grant-or-nothing.

*Screen capture is blocked by the result channel, not by the portal.* A decoded application frame is
capped at 256 KiB and a result is JSON, so a PNG cannot travel to the agent; the desktop's own capture
path returns a GTK-side `Screenshot` for display. Pointer/keyboard actions are small JSON and stay
feasible. Do not ship a capture command that returns a path the agent cannot read as an image: that
would report a capability the agent cannot use.

*Gate:* the real agent reads a granted file, then edits and moves one only after approval, with intent
and outcome both in the Activity log; revoke stops it mid-task; denied operations change nothing.

*Why here:* highest value per unit of work in the whole plan — the capabilities exist and are tested
for human use; only the dispatch surface is missing.

### P4 — Scheduled tasks and durable background work (L)

*Gap:* Mac has one-time, recurring and location reminders, delivered as chat messages, with approvals
managed separately. We have read-only backend lists plus a local reminder store, and our host dies with
the app (parent-death + 300 s idle), so nothing survives closing the window.

*Work:* create/edit/cancel/enable from Linux; a user-level service (`systemd --user` unit) that owns
the host independent of the GUI; a local scheduler with timezone handling, catch-up on resume and
at-most-once dispatch; desktop notifications via `org.freedesktop.Notifications`; location reminders
degraded to time-only and documented as such.

*Gate:* a task created in the app runs while the window is closed, notifies, survives reboot without
duplicating, can be stopped, and leaves an audit trail.

### P5 — Artifacts, Library and Upcoming (M)

*Gap:* the Mac client has Library (saved artifacts), System Files (the VM filesystem) and Upcoming.
We have none — no list, no preview, no download, no share gate. Distinct from local Files.

*Library listing is reachable and now projected (live, 2026-11-06).* `artifacts.list` answered with
`total_count: 76`, a 50-edge page and `page_info.has_next_page: true`. Each edge is `{cursor, node}`;
the node carries `artifact_ref` (`file:workspace/your_files/…`), `display_name`, `type`, `format`,
`status`, `placement`, `updated_at_ms`, `is_pinned`, `open_target.path` and a `capabilities` block —
the rows first inspected reported `can_open`/`can_pin`/`can_delete`/`can_rename` true and
`can_share: false`, but a full first page (50 rows) carries `can_share: true` on one row, so sharing
is a per-artifact answer and not a global switch. The
bridge op keeps those fields plus `has_more` and drops the cursor: a pagination token is not
something this client needs to hold, and every unlisted node field stays upstream. No preview,
open, download or share exists yet, and sharing stays the service's own `false` rather than a
switch this client could flip.

The Library page reads exactly that: one explicit press, one read, pinned-first so a chosen pin is
not buried by recency, and a notice that repeats the service's total beside the page size instead of
implying 50 is everything. It has no open, preview, download, rename, pin or share action, and it
cannot start a read without a connected account.

*Work:* probe workspace/artifact routes; list and preview the supported types (PDF, DOCX, TXT, MD,
XLSX, CSV, PNG/JPEG/GIF/WebP), open-in-app, upload, download; sharing off until explicitly approved per
artifact; Upcoming as a merged view of schedules and pending approvals.

*Gate:* an artifact the agent made opens natively; sharing is blocked and audited until approved;
Upcoming shows the next scheduled run and the pending approval without polling storms.

### P6 — Memory (S)

*Gap:* memory is a real, inspectable surface on Mac (file-backed, `MEMORY.md`-style, plus a "forget"
path). We expose nothing, so a user cannot see or correct what the agent believes.

*Work:* read the memory surface, search it, edit and delete entries, export; show the "updating memory"
status in P1's status area.

*Gate:* the user can list, correct and delete remembered facts, and the deletion is verified through a
fresh read rather than assumed.

### P7 — Connector framework (L)

*Gap:* Mac connectors are authorized in-app, are read-only or read-write per connector, can push events,
and can be authored for unlisted services. Ours are three hard-coded provider integrations with no
scope control and no registry.

*Work:* a connector interface with OAuth in the credential store, declared scopes, read-only default,
write behind P2, proactive event delivery, read-back verification, and custom-connector authoring
(MCP-shaped) with explicit unreviewed-code warnings.

*Gate:* one real provider end to end: read, drafted write, approved write, verified read-back, revoked
cleanly; a denied write mutates nothing.

### P8 — Browser parity (M)

*Gap:* cloud-side browsing exists for the agent and not for us; on the Mac you can take control or stop
a task and allow specific sites. Our extension is user-gesture only (list/open/close), no content
access, no form filling.

*Work:* surface the agent's browser session with take-control and stop; an allowed-websites policy in
P2; extension-mediated agent actions with per-action approval; form filling gated on submit.

*Gate:* a harmless real action completes in a real page; a site outside the allowlist is refused; a
submit-shaped action waits for approval; stop halts further actions.

### P9 — Product shell and distribution (M)

*Gap:* notifications, usage meter and limit warnings, personalization (name/tone), privacy and training
toggles, reset, self-update, tray, i18n, and an accessible, native-Wayland-verified build.

*Work:* in that order. Usage and personalization are server-side values — surface them, do not invent
them. Add AT-SPI to our own widgets (we use it as a client; our UI should be usable by it), a Flatpak
alongside the AppImage, and a real native-Wayland check rather than XWayland-only evidence.

*Gate:* the app is usable with a screen reader, updates itself, warns before the usage cap, and passes
its scenario checks on both native Wayland and XWayland.

### P10 — Not ours to mimic

Record these as boundaries instead of backlog: Sentinel (server-side, outside the model),
Secure Credentials Store, Confidential VM, usage metering internals, phone calls, the iOS/Android
companion apps. **Model choice** does not appear to exist as a Muse feature at all, and **desktop
voice** is unconfirmed for the Mac build — both need evidence before they earn a phase.

## Ordering rules

1. **Existence proof before breadth.** P0 exists because nothing below it can be called done while
   sign-in and registration are unverified.
2. **Policy before capability.** P2 precedes P3–P8; consent bolted on later is consent bypassed.
3. **Reuse before new surfaces.** P3 exposes built capabilities instead of adding more.
4. **The loop before the shelf.** P1 (conversation) and P4 (tasks) change daily use; P5/P6 are
   completeness.
5. **One route probe per phase, first.** A missing gateway route is a documented block, not a mock,
   and not a reason to substitute the developer API.

## Next three actions

1. Complete a fresh packaged sign-in and record the exact states (success, expired, revoked).
2. Register the local host against the real service and invoke `files.read_text` on a scratch file —
   the first live agent-to-Linux call.
3. Extend `account-probe` to report route availability for streaming, artifacts and memory, so P1, P5
   and P6 are planned against facts.
