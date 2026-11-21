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

/// A message turn: the reader's words in the blue pill, Muse's in the grey
/// bubble, with the attachments and option pills the native row carries.
function Turn({ row }: { row: Row }) {
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

  const send = async (event?: FormEvent) => {
    event?.preventDefault();
    const text = draft.trim();
    if (!text || pending) return;
    setPending(true);
    setNote(null);
    try {
      await call<{ ok: boolean }>("chat.send", 30_000, { text });
      // The app took it: the entry clears, exactly as a press in that window
      // clears it. The transcript and the status line carry the outcome.
      setDraft("");
      void loadSnapshot();
    } catch (failure) {
      // The app's own sentence (a held slot, a signed-out window), and the
      // draft stays: nothing typed is lost to a refusal.
      setNote(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setPending(false);
    }
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
                <Turn key={row.id} row={row} />
              ),
            )
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
            <button
              className="chat-send"
              type="submit"
              disabled={!sendable}
              aria-label="Send"
            >
              ↑
            </button>
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
            Sends go through the app's own send: the same slot, the same
            receipt, the same history read. Pictures, approvals, Stop and
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
