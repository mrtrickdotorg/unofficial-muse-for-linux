import {
  useCallback,
  useEffect,
  useMemo,
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


  // The draft lives here, but the send does not: `chat.send` hands the words
  // back to the app's controller, which owns the `Send` slot, the receipt and
  // the history reload. This window never dispatches on its own.
  const [draft, setDraft] = useState("");
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
      // The app took it: the entry clears, exactly as a press in that window
      // clears it, and so does the quote -- one Reply answers one send. The
      // transcript and the status line carry the outcome.
      setDraft("");
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
    // The press that answers an armed menu item has done the deletion, so the
    // menu closes on it; the arming press leaves it open, showing the app's
    // confirming label.
    const confirming = menuTurn?.armed ?? false;
    try {
      await call<{ ok: boolean }>("chat.delete", 30_000);
      // The label and the rows both come from the app: the armed press names
      // itself there, and the turn only disappears once the app's own re-read
      // no longer carries it.
      if (confirming) setMenuOpen(false);
      void loadSnapshot();
    } catch (failure) {
      // The app's own sentence (a held send slot, a signed-out window). The
      // turn stays: nothing is deleted on this page's say-so.
      setTurnNote(sentence(failure));
    } finally {
      setDeleting(false);
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
  // The menu closes when the turn it named is gone: the app disarms on a moved
  // transcript, and a popover left open over an empty thread would offer a
  // press the app would refuse.
  const menuTurn = menuOpen ? turn : undefined;
  useEffect(() => {
    if (menuOpen && !turn) setMenuOpen(false);
  }, [menuOpen, turn]);

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
            {menuTurn && (
              <div
                className="chat-turn-pop"
                id="chat-turn-menu"
                role="group"
                aria-label="Turn options"
              >
                <button
                  className={`chat-turn-delete${menuTurn.armed ? " armed" : ""}`}
                  type="button"
                  disabled={deleting}
                  onClick={() => void deleteTurn()}
                >
                  {menuTurn.delete_label}
                </button>
                {menuTurn.armed && (
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
            <textarea
              className="chat-draft"
              aria-label="Message"
              placeholder="Message"
              rows={1}
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
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
            Sends, Stop, pictures and decisions go through the app's own: the
            same sender, the same clipboard, the same one decide path, the same
            history read. Dictation still lives in the app window.
          </div>
        </aside>
      </div>

      <div className="foot">
        Web UI slice of the port: the same rows the native transcript holds.
      </div>
    </div>
  );
}
