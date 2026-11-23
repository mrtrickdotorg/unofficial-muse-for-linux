import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";
import { call, channelAvailable } from "./api";
import type {
  ApprovalCard,
  ApprovalChoice,
  ChatRow as Row,
  ChatSnapshot,
  TurnMenu,
} from "./types";

// How often the page re-asks for the transcript. The app owns the reads --
// its own polling and its streaming rewrite -- so this window costs no wire
// trips of its own: it follows the rows the native list last built.
const POLL_MS = 2000;

// The draft is the one thing a reader edits keystroke by keystroke, and every
// write of it repaints the native window's own entry. So the page coalesces
// typing into one command per pause, and one more mid-burst so a long sentence
// still reaches the native window while it is being typed.
const DRAFT_DEBOUNCE_MS = 200;
const DRAFT_BATCH_MS = 600;

/// The quote the page stages with Reply: the row's first line, trimmed. The
/// app owns the stored quote and caps it there; this bar ellipsises, so the
/// page needs no second copy of the limit.
export function quoteExcerpt(text: string): string {
  return text.split("\n")[0].trim();
}

/// A message turn: the reader's words in the blue pill, Muse's in the grey
/// bubble, with the attachments and option pills the native row carries.
function Turn({ row, onReply }: { row: Row; onReply: (row: Row) => void }) {
  const mine = row.role === "user";
  return (
    <div
      className={`chat-row ${mine ? "user" : "assistant"}`}
      data-testid={`turn-${row.id}`}
      data-role={row.role}
    >
      {!mine && (
        <div className="chat-head">
          <span className="chat-avatar" aria-hidden="true">
            ✦
          </span>
          <span className="chat-who">Muse</span>
          {row.sent_at && <span className="chat-time">{row.sent_at}</span>}
        </div>
      )}
      {row.reply_to && (
        <div className="chat-reply-to" title={row.reply_to}>
          Replying to: {row.reply_to}
        </div>
      )}
      <div className={`chat-bubble ${mine ? "user" : "assistant"}`}>
        {row.text.split(/\n{2,}/).map((paragraph, index) => (
          <p key={index}>{paragraph}</p>
        ))}
      </div>
      {row.attachments.length > 0 && (
        <div className="chat-files">
          {row.attachments.map((file) => (
            <span className="chat-file" key={file}>
              {file}
            </span>
          ))}
        </div>
      )}
      <div className="chat-row-actions">
        <button
          className="chat-reply"
          type="button"
          onClick={() => onReply(row)}
          aria-label={`Reply to ${mine ? "your" : "Muse's"} message`}
        >
          Reply
        </button>
      </div>
      {row.options.length > 0 && (
        <div className="chat-options">
          {row.options.map((option) => (
            <span
              className={`chat-option${row.selected === option ? " chosen" : ""}`}
              key={option}
            >
              {row.selected === option ? "✓ " : ""}
              {option}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

// `pollMs` is a seam for the tests: a page under test should not have to
// wait two seconds per look to prove that a growing reply reaches the DOM.
export function Chat({ pollMs = POLL_MS }: { pollMs?: number }) {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<ChatSnapshot | null>(
    boot && "account" in boot && "rows" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);

  // A cache read on the native side: the transcript it holds right now. The
  // page opens on the boot copy and then keeps asking, which is how a reply
  // that grows while it arrives reaches this window.
  const loadSnapshot = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    try {
      setData(await call<ChatSnapshot>("chat.snapshot"));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }, []);

  useEffect(() => {
    void loadSnapshot();
    const timer = setInterval(() => void loadSnapshot(), pollMs);
    return () => clearInterval(timer);
  }, [loadSnapshot, pollMs]);


  // One draft, not two: the box shows the app's own composer text, a keystroke
  // here writes it back through `chat.draft`, and a dictation's words arrive as
  // the same draft on the next snapshot. `lastSynced` is the value both
  // surfaces agree on, so an echo of this page's own typing is not mistaken for
  // the native window changing the draft underneath the reader.
  const [draft, setDraft] = useState("");
  // The last draft both surfaces agreed on: what the app answered, never what
  // this page has typed but not yet sent.
  const lastSynced = useRef("");
  const pendingDraft = useRef<string | null>(null);
  const flushTimer = useRef<number | null>(null);
  const burstStarted = useRef(0);

  const cancelFlush = () => {
    if (flushTimer.current !== null) {
      window.clearTimeout(flushTimer.current);
      flushTimer.current = null;
    }
    pendingDraft.current = null;
    burstStarted.current = 0;
  };

  // Send whatever the reader typed since the last write, once. `lastSynced`
  // moves here rather than on each keystroke: until the app has the words, it
  // has not agreed to them.
  const flushDraft = useCallback(() => {
    if (flushTimer.current !== null) {
      window.clearTimeout(flushTimer.current);
      flushTimer.current = null;
    }
    const text = pendingDraft.current;
    pendingDraft.current = null;
    burstStarted.current = 0;
    if (text === null) return;
    lastSynced.current = text;
    void call<{ ok: boolean }>("chat.draft", 15_000, { text }).catch((failure: unknown) =>
      setNote(sentence(failure)),
    );
  }, []);

  const typeDraft = (text: string) => {
    setDraft(text);
    pendingDraft.current = text;
    const now = Date.now();
    if (burstStarted.current === 0) burstStarted.current = now;
    if (flushTimer.current !== null) window.clearTimeout(flushTimer.current);
    // A burst flushes on its pause; one that never pauses flushes a beat later
    // so the native window is never far behind the box.
    const waited = now - burstStarted.current;
    flushTimer.current = window.setTimeout(
      flushDraft,
      waited >= DRAFT_BATCH_MS ? 0 : DRAFT_DEBOUNCE_MS,
    );
  };
  const [pending, setPending] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  // One decision at a time: the app takes the page slot per press, and every
  // card is disabled while one is in flight so a second press cannot race it.
  const [deciding, setDeciding] = useState<string | null>(null);
  // A press the app could not take -- a held panel slot, a signed-out window --
  // says so beside the cards, because the card is still there and still means
  // "waiting on you".
  const [approvalNote, setApprovalNote] = useState<string | null>(null);
  const sentence = (failure: unknown) =>
    failure instanceof Error ? failure.message : String(failure);
  // The pictures staged in the app's own composer. This page can ask for a
  // paste and drop one, but the bytes and the send stay with the app.
  const staged = data?.staged ?? [];
  const approvals = data?.approvals ?? { cards: [], loaded: false };
  const sendable = (draft.trim().length > 0 || staged.length > 0) && !pending;
  // A send is in flight for about a second, so the two-second poll would
  // usually miss it -- and Stop is only shown while the app says a send is in
  // flight. Watch fast for a beat after a dispatch, the way the app's own send
  // does, so a reader can actually reach Stop.
  const [watching, setWatching] = useState(false);
  // The page's Reply: which turn the next send answers. Reader-side context
  // only -- the app's send is a plain send -- but it rides the same command and
  // is cleared by the same two things that clear the app's own quote: a send
  // that goes, and a cancel.
  const [quote, setQuote] = useState<{ id: string; excerpt: string } | null>(null);

  const send = async (event?: FormEvent) => {
    event?.preventDefault();
    const text = draft.trim();
    // A picture alone sends, exactly as it does from the app's own card.
    if (pending || (text.length === 0 && staged.length === 0)) return;
    setPending(true);
    setNote(null);
    try {
      await call<{ ok: boolean }>("chat.send", 30_000, {
        text,
        ...(quote ? { quote } : {}),
      });
      // The app took it: the shared draft clears on both surfaces, exactly as
      // a press in that window clears it, and so does the quote -- one Reply
      // answers one send. The transcript and the status line carry the outcome.
      cancelFlush();
      setDraft("");
      lastSynced.current = "";
      setQuote(null);
      void loadSnapshot();
      setWatching(true);
    } catch (failure) {
      // The app's own sentence (a held slot, a signed-out window), and the
      // draft stays: nothing typed is lost to a refusal.
      setNote(sentence(failure));
    } finally {
      setPending(false);
    }
  };

  // One write at a time per control, the way the native card works: a press
  // claims the control, a second press while that write is unanswered is not a
  // second write, and the control comes back when the app's answer lands.
  const [attaching, setAttaching] = useState(false);
  const [dropping, setDropping] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [checking, setChecking] = useState(false);
  // The turn menu's Delete, on the app's own arm-then-confirm rule: the page
  // prints the label the app computed and posts one press per answer, so a
  // page showing "Confirm delete turn" means the app is really armed.
  const [deleting, setDeleting] = useState(false);
  const [turnNote, setTurnNote] = useState<string | null>(null);
  // The turn menu, where the native "…" pill keeps it: a popover beside the
  // thread title, so the one destructive press is not a stray link under some
  // other section's heading.
  const [menuOpen, setMenuOpen] = useState(false);

  // The `+` press: the app's clipboard read, the same one its own button
  // makes. The chip it stages comes back in the next snapshot. The app refuses
  // a paste while a send is in flight, so this press is not offered then -- one
  // press, one paste, and never a write the app would drop.
  const attach = async () => {
    if (attaching || pending || data?.sending) return;
    setAttaching(true);
    setNote(null);
    try {
      await call<{ ok: boolean }>("chat.attach", 30_000);
      void loadSnapshot();
    } catch (failure) {
      setNote(sentence(failure));
    } finally {
      setAttaching(false);
    }
  };

  const unstage = async (name: string) => {
    if (dropping !== null) return;
    setDropping(name);
    setNote(null);
    try {
      await call<{ ok: boolean }>("chat.unstage", 30_000, { name });
      void loadSnapshot();
    } catch (failure) {
      setNote(sentence(failure));
    } finally {
      setDropping(null);
    }
  };

  const deleteTurn = async () => {
    if (deleting) return;
    setDeleting(true);
    setTurnNote(null);
    try {
      await call<{ ok: boolean }>("chat.delete", 30_000);
      // The label and the rows both come from the app: the armed press names
      // itself there, and the turn only disappears once the app's own re-read
      // no longer carries it.
      void loadSnapshot();
    } catch (failure) {
      // The app's own sentence (a held send slot, a signed-out window). The
      // turn stays: nothing is deleted on this page's say-so.
      setTurnNote(sentence(failure));
    } finally {
      setDeleting(false);
    }
  };

  // The mic: the app's own toggle, so a press here and a press in the native
  // window are the same take, and its words land in the draft both surfaces show.
  const [dictatingPress, setDictatingPress] = useState(false);
  const dictate = async () => {
    if (dictatingPress) return;
    setDictatingPress(true);
    setNote(null);
    try {
      await call<{ ok: boolean }>("chat.dictate", 30_000);
      // A take opens or closes; either way the app's own sentence lands in the
      // status line and the mic's state comes back in the next snapshot.
      void loadSnapshot();
    } catch (failure) {
      setNote(sentence(failure));
    } finally {
      setDictatingPress(false);
    }
  };

  // The panel's "Check approvals", on the app's own read.
  const refreshApprovals = async () => {
    if (checking) return;
    setChecking(true);
    setApprovalNote(null);
    try {
      await call<{ ok: boolean }>("chat.approvals.refresh", 30_000);
      void loadSnapshot();
    } catch (failure) {
      setApprovalNote(sentence(failure));
    } finally {
      setChecking(false);
    }
  };

  // One press, one decision, and the card leaves only when the app's re-read
  // no longer carries it -- never on this page's say-so.
  const decide = async (card: ApprovalCard, choice: ApprovalChoice) => {
    setDeciding(card.id);
    setApprovalNote(null);
    try {
      // `approval_id`, not `id`: `id` is the channel's own reply ticket, and
      // overwriting it would leave this press waiting for an answer nobody
      // sends.
      await call<{ ok: boolean }>("chat.decide", 30_000, {
        approval_id: card.id,
        decision: choice.word,
      });
      void loadSnapshot();
    } catch (failure) {
      // The app's own sentence (a held panel slot, a signed-out window). The
      // card stays: nothing was decided, so nothing may leave the panel.
      setApprovalNote(sentence(failure));
    } finally {
      setDeciding(null);
    }
  };

  // Stop is the app's stop, not the page's: it goes back through `chat.stop`
  // and the app's own cancel path, which takes no session ticket precisely so
  // it can fire while a send holds the `Send` slot. What the service said it
  // did comes back in the status line, and what actually landed in the
  // transcript.
  const stop = async () => {
    // A double press is one cancel, not two: the second press while the first
    // is unanswered is dropped. Once the app answers, a later press is a new
    // stop -- the native card allows a second press, and so does this one.
    if (stopping) return;
    setStopping(true);
    setNote(null);
    try {
      await call<{ ok: boolean }>("chat.stop", 30_000);
    } catch (failure) {
      setNote(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setStopping(false);
    }
  };

  // The window can change the draft without this page: native typing, a
  // dictation's words, another page proposing one. The snapshot is the news,
  // and it wins over typing this page has not sent yet -- writing those stale
  // words afterwards would clobber the dictation. A pending flush is therefore
  // dropped, not delayed.
  const sharedDraft = data?.composer?.draft;
  useEffect(() => {
    if (sharedDraft === undefined || sharedDraft === lastSynced.current) return;
    cancelFlush();
    lastSynced.current = sharedDraft;
    setDraft(sharedDraft);
  }, [sharedDraft]);

  // Leaving the page or the box must not leave the native window behind: a blur
  // and an unmount both write what is still pending.
  useEffect(() => flushDraft, [flushDraft]);

  useEffect(() => {
    if (!watching) return;
    const fast = setInterval(() => void loadSnapshot(), 300);
    const done = setTimeout(() => setWatching(false), 8000);
    return () => {
      clearInterval(fast);
      clearTimeout(done);
    };
  }, [watching, loadSnapshot]);

  const stageQuote = (row: Row) => {
    const excerpt = quoteExcerpt(row.text);
    if (!excerpt) return;
    setQuote({ id: row.id, excerpt });
  };

  const draftKey = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void send();
    }
  };

  const rows = useMemo(() => data?.rows ?? [], [data]);
  const loaded = data?.loaded ?? false;
  const turn: TurnMenu | undefined = data?.turn;
  const dictating = data?.composer?.dictating ?? false;

  // The native probe reads this: how many transcript rows the page holds.
  (window as unknown as { __MUSE_ROWS__?: number }).__MUSE_ROWS__ = rows.length;

  return (
    <div className="page chat-page">
      <div className="top">
        <h1 className="title">{data?.title || "Chat"}</h1>
        {data?.unread ? (
          <span className="chat-unread" aria-label={`${data.unread} unread`}>
            {data.unread}
          </span>
        ) : null}
        {turn && (
          <div className="chat-turn-menu">
            {/* A plain disclosure button, not `aria-haspopup="menu"`: WebKit
                maps that to a combo box with only a `select` action, which is
                the wrong thing to announce and the wrong thing to press. */}
            <button
              className="chat-turn-pill"
              type="button"
              aria-label="Turn options"
              aria-expanded={menuOpen}
              aria-controls="chat-turn-menu"
              onClick={() => setMenuOpen((open) => !open)}
            >
              …
            </button>
            {/* A labelled group of buttons, not `role="menu"`: a menu role
                promises arrow-key navigation and focus handling this popover
                does not implement, and WebKit gives a menu item an unnamed
                action that no assistive press can address by name. */}
            {menuOpen && turn && (
              <div
                className="chat-turn-pop"
                id="chat-turn-menu"
                role="group"
                aria-label="Turn options"
              >
                <button
                  className={`chat-turn-delete${turn.armed ? " armed" : ""}`}
                  type="button"
                  disabled={deleting}
                  onClick={() => void deleteTurn()}
                >
                  {turn.delete_label}
                </button>
                {turn.armed && (
                  <p className="chat-turn-hint">
                    This deletes the latest turn and cannot be undone.
                  </p>
                )}
                {turnNote && <p className="chat-approval-error">{turnNote}</p>}
              </div>
            )}
          </div>
        )}
        <span className="count">{data?.account ?? ""}</span>
        <span className="chat-live">following the app</span>
      </div>

      {error && <div className="banner">Command failed: {error}</div>}
      {data?.error && <div className="banner">The last read failed: {data.error}</div>}

      <div className="chat-shell">
        <div className="chat-transcript">
          {!loaded ? (
            <div className="empty">
              The native Chat page has not read a thread yet, so there is
              nothing to mirror. Open a conversation in the app — this window
              is not claiming the thread is empty.
            </div>
          ) : rows.length === 0 ? (
            <div className="empty">No messages yet.</div>
          ) : (
            rows.map((row, index) =>
              row.kind === "day" ? (
                <div className="chat-day" key={`day-${index}`}>
                  {row.day}
                </div>
              ) : row.kind === "notice" ? (
                <div className="chat-notice" key={`notice-${index}`}>
                  {row.text}
                </div>
              ) : (
                // Keyed by the service's own id: a reply that grows stays
                // the same element, so its words change under the reader
                // instead of a second turn replacing the first.
                <Turn key={row.id} row={row} onReply={stageQuote} />
              ),
            )
          )}

          {quote && (
            <div className="chat-quote">
              <span className="chat-quote-text">Replying to: {quote.excerpt}</span>
              <button
                className="chat-quote-cancel"
                type="button"
                onClick={() => setQuote(null)}
                aria-label="Cancel reply"
              >
                ×
              </button>
            </div>
          )}
          {staged.length > 0 && (
            <div className="chat-staged" aria-label="Pictures staged for the next message">
              {staged.map((chip) => (
                <span className="chat-chip" key={chip.name}>
                  <span className="chat-chip-meta">
                    <span className="chat-chip-name">{chip.name}</span>
                    <span className="chat-chip-facts">
                      {chip.width} × {chip.height} · {chip.bytes} bytes
                    </span>
                  </span>
                  <button
                    className="chat-chip-remove"
                    type="button"
                    disabled={dropping !== null}
                    onClick={() => void unstage(chip.name)}
                    aria-label={`Drop ${chip.name}`}
                  >
                    ×
                  </button>
                </span>
              ))}
            </div>
          )}
          <form className="chat-composer" onSubmit={send}>
            <button
              className="chat-attach"
              type="button"
              disabled={attaching || pending || Boolean(data?.sending)}
              onClick={() => void attach()}
              aria-label="Attach a picture from the clipboard"
            >
              +
            </button>
            {/* The mic is the app's own toggle, and its state is the app's:
                a take also stops itself after thirty seconds, which only the
                window knows. */}
            <button
              className={`chat-mic${dictating ? " listening" : ""}`}
              type="button"
              disabled={dictatingPress}
              onClick={() => void dictate()}
              aria-label={dictating ? "Stop dictation" : "Dictate"}
            >
              {dictating ? "■" : "🎤"}
            </button>
            <textarea
              className="chat-draft"
              aria-label="Message"
              placeholder="Message"
              rows={1}
              value={draft}
              onChange={(event) => typeDraft(event.target.value)}
              onBlur={flushDraft}
              onKeyDown={draftKey}
            />
            {data?.sending || watching ? (
              // The native card swaps the same circle while a send is in
              // flight. The app's send is the dispatch, which is over in about
              // a second, so the page keeps Stop a beat longer -- the same
              // window in which it watches fast for the reply -- or a reader
              // could never reach it before the reply is done streaming.
              <button
                className="chat-send chat-stop"
                type="button"
                disabled={stopping}
                onClick={stop}
                aria-label="Stop"
              >
                ■
              </button>
            ) : (
              <button
                className="chat-send"
                type="submit"
                disabled={!sendable}
                aria-label="Send"
              >
                ↑
              </button>
            )}
          </form>
          {note && <div className="chat-note">{note}</div>}
        </div>

        <aside className="chat-panel">
          <div className="chat-panel-name">{data?.account || "Sign in"}</div>
          {data?.status && <div className="chat-status">{data.status}</div>}
          <div className="chat-panel-meta">
            {data?.thread ? `Thread ${data.thread.slice(0, 8)}…` : "New chat"}
          </div>

          <section className="chat-approvals" aria-label="Approvals">
            <div className="chat-panel-head">
              <h2 className="chat-panel-title">Approvals</h2>
              <button
                className="chat-check"
                type="button"
                disabled={checking}
                onClick={() => void refreshApprovals()}
              >
                Check approvals
              </button>
            </div>
            {approvalNote && (
              <p className="chat-approval-error">{approvalNote}</p>
            )}
            {approvals.note ? (
              <p className="chat-panel-meta">{approvals.note}</p>
            ) : !approvals.loaded ? (
              <p className="chat-panel-meta">No approvals read yet.</p>
            ) : approvals.cards.length === 0 ? (
              <p className="chat-panel-meta">No pending approvals.</p>
            ) : (
              approvals.cards.map((card) => (
                <article className="chat-approval" key={card.id}>
                  <div className="chat-approval-headline">{card.headline}</div>
                  {card.summary && (
                    <p className="chat-approval-summary">{card.summary}</p>
                  )}
                  {card.details.map((line) => (
                    <div className="chat-approval-detail" key={line}>
                      {line}
                    </div>
                  ))}
                  <div className="chat-approval-actions">
                    {card.choices.map((choice) => (
                      <button
                        key={choice.word}
                        type="button"
                        className={
                          choice.word === "deny"
                            ? "chat-approval-deny"
                            : "chat-approval-allow"
                        }
                        disabled={deciding !== null}
                        onClick={() => void decide(card, choice)}
                      >
                        {choice.label}
                      </button>
                    ))}
                  </div>
                </article>
              ))
            )}
          </section>

          <div className="chat-panel-note">
            Sends, Stop, pictures, decisions, Delete turn and the mic go through
            the app's own: the same sender, the same clipboard, the same decide
            path, the same history read, the same armed delete, the same take.
            The draft in that box is the window's own, so dictation fills both.
          </div>
        </aside>
      </div>

      <div className="foot">
        Web UI slice of the port: the same rows the native transcript holds.
      </div>
    </div>
  );
}
