import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";
import { call, channelAvailable } from "./api";
import type { ChatRow as Row, ChatSnapshot } from "./types";

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
    boot && "title" in boot ? boot : null,
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
  const sendable = draft.trim().length > 0 && !pending;
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
    if (!text || pending) return;
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
      setNote(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setPending(false);
    }
  };

  // Stop is the app's stop, not the page's: it goes back through `chat.stop`
  // and the app's own cancel path, which takes no session ticket precisely so
  // it can fire while a send holds the `Send` slot. What the service said it
  // did comes back in the status line, and what actually landed in the
  // transcript.
  const stop = async () => {
    setNote(null);
    try {
      await call<{ ok: boolean }>("chat.stop", 30_000);
    } catch (failure) {
      setNote(failure instanceof Error ? failure.message : String(failure));
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

  // The native probe reads this: how many transcript rows the page holds.
  (window as unknown as { __MUSE_ROWS__?: number }).__MUSE_ROWS__ = rows.length;

  return (
    <div className="page chat-page">
      <div className="top">
        <h1 className="title">{data?.title || "Chat"}</h1>
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
          <form className="chat-composer" onSubmit={send}>
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
          <div className="chat-panel-note">
            Sends and Stop go through the app's own: the same sender, the same
            cancel path, the same history read. Pictures, approvals and
            dictation still live in the app window.
          </div>
        </aside>
      </div>

      <div className="foot">
        Web UI slice of the port: the same rows the native transcript holds.
      </div>
    </div>
  );
}
