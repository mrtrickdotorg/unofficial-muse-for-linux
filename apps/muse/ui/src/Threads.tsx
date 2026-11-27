import { useCallback, useEffect, useMemo, useState } from "react";
import { call, channelAvailable } from "./api";
import { useHeldRead } from "./held";
import type { ThreadRow as Row, ThreadsSnapshot } from "./types";

/// One conversation's change: the verb the service is asked for, and the title
/// a rename carries. The app owns the vocabulary; the page only presses.
type ThreadOp = {
  id: string;
  verb: "pin" | "unpin" | "archive" | "unarchive" | "rename" | "delete";
  title?: string;
};

/// The answer one change comes back with: the service's acceptance and the
/// listing read again after it, which is the proof of what changed.
type ThreadOpAnswer = ThreadsSnapshot & {
  accepted?: boolean;
  reread_error?: string | null;
};

/// One conversation row: pin, title, unread badge, then the listing's
/// snippet and clock time. The day lives in the group header, as the native
/// list paints it. The row's own presses are the mac app's: pin, archive,
/// rename -- each one a service call, then the listing read again.
function ThreadLine({
  item,
  busy,
  renaming,
  armed,
  onRename,
  onOp,
  onCancelRename,
  onCancelDelete,
  onSeen,
}: {
  item: Row;
  busy: boolean;
  renaming: boolean;
  /// This row's Delete press is armed: the row paints the confirmation the
  /// app is holding, and the press that spends it.
  armed: boolean;
  onRename: (title: string) => void;
  onOp: (op: ThreadOp) => void;
  onCancelRename: () => void;
  onCancelDelete: () => void;
  onSeen: () => void;
}) {
  const [draft, setDraft] = useState(item.title);

  return (
    <div className={`thread${item.pinned ? " pinned" : ""}`}>
      <div className="thread-words">
        <h3>
          {item.pinned && (
            <span className="thread-pin" aria-hidden="true">
              📌{" "}
            </span>
          )}
          <span className="thread-title">{item.title}</span>
          {item.unread > 0 && (
            <span className="badge thread-unread" title={`${item.unread} unread`}>
              {item.unread}
            </span>
          )}
        </h3>
        {item.meta && <p className="thread-meta">{item.meta}</p>}
      </div>
      {renaming ? (
        <form
          className="thread-rename"
          onSubmit={(event) => {
            event.preventDefault();
            onRename(draft);
          }}
        >
          <input
            aria-label="New title"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            maxLength={200}
            autoFocus
          />
          <button type="submit" className="thread-action" disabled={busy}>
            {busy ? "Saving…" : "Save"}
          </button>
          <button type="button" className="thread-action" onClick={onCancelRename} disabled={busy}>
            Cancel
          </button>
        </form>
      ) : (
        <div className="thread-actions">
          {item.unread > 0 && (
            <button
              type="button"
              className="thread-action"
              aria-label={`Mark read ${item.title}`}
              onClick={onSeen}
              disabled={busy}
            >
              {busy ? "Marking…" : "Mark read"}
            </button>
          )}
          {!item.archived && (
            <button
              type="button"
              className="thread-action"
              aria-label={`${item.pinned ? "Unpin" : "Pin"} ${item.title}`}
              onClick={() => onOp({ id: item.id, verb: item.pinned ? "unpin" : "pin" })}
              disabled={busy}
            >
              {item.pinned ? "Unpin" : "Pin"}
            </button>
          )}
          <button
            type="button"
            className="thread-action"
            aria-label={`Rename ${item.title}`}
            onClick={() => onOp({ id: item.id, verb: "rename" })}
            disabled={busy}
          >
            Rename
          </button>
          <button
            type="button"
            className="thread-action"
            aria-label={`${item.archived ? "Unarchive" : "Archive"} ${item.title}`}
            onClick={() => onOp({ id: item.id, verb: item.archived ? "unarchive" : "archive" })}
            disabled={busy}
          >
            {item.archived ? "Unarchive" : "Archive"}
          </button>
          <button
            type="button"
            className={`thread-action${armed ? " danger" : ""}`}
            aria-label={armed ? `Confirm delete ${item.title}` : `Delete ${item.title}`}
            onClick={() => onOp({ id: item.id, verb: "delete" })}
            disabled={busy}
          >
            {busy && armed ? "Deleting…" : armed ? "Confirm delete" : "Delete"}
          </button>
          {armed && (
            <button type="button" className="thread-action" onClick={onCancelDelete} disabled={busy}>
              Cancel
            </button>
          )}
        </div>
      )}
    </div>
  );
}

export function Threads() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<ThreadsSnapshot | null>(
    boot && "threads" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /// The row a change is running against, so only that row's presses wait: a
  /// rename on one conversation must not freeze the rest of the listing.
  const [pending, setPending] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  /// The row the app has armed for deletion, as the app reports it.
  const [armed, setArmed] = useState<string | null>(data?.armed ?? null);

  // Opening the page costs nothing on the wire: the native side answers
  // from the listing it already holds.
  const loadSnapshot = useCallback(async (): Promise<ThreadsSnapshot | null> => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return null;
    }
    try {
      const answer = await call<ThreadsSnapshot>("threads.snapshot");
      setData(answer);
      setArmed(answer.armed ?? null);
      setError(null);
      return answer;
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      return null;
    }
  }, []);

  // Refresh reads the listing again, which is what the label promises; the
  // account read can spend seconds on the conversation list.
  const refresh = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    setBusy(true);
    try {
      const answer = await call<ThreadsSnapshot>("threads.refresh", 120_000);
      setData(answer);
      setArmed(answer.armed ?? null);
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  const reading = useHeldRead("threads", loadSnapshot, refresh);

  // The app holds the listing, and a message arriving on a conversation nobody
  // has open changes a badge there. This page re-reads the held rows on a slow
  // beat -- no wire read, no remount -- so that badge appears here too. A
  // failed poll says nothing: the page keeps the rows it has rather than
  // painting a failure over them.
  useEffect(() => {
    const timer = window.setInterval(() => {
      if (!channelAvailable() || pending !== null || busy) {
        return;
      }
      void call<ThreadsSnapshot>("threads.snapshot")
        .then((answer) => {
          setData(answer);
          setArmed(answer.armed ?? null);
        })
        .catch(() => {});
    }, 8_000);
    return () => window.clearInterval(timer);
  }, [pending, busy]);

  // One change, on the app's own channel: the service answers whether it took
  // the verb, and the listing that comes back is what the page paints. A
  // refused verb leaves the rows as they were and says so in words.
  const runOp = useCallback(
    async (op: ThreadOp) => {
      if (op.verb === "rename" && op.title === undefined) {
        setRenaming(op.id);
        return;
      }
      if (!channelAvailable()) {
        setError("The app's command channel is not available in this window.");
        return;
      }
      setPending(op.id);
      try {
        const answer = await call<ThreadOpAnswer>("threads.op", 120_000, {
          thread: op.id,
          verb: op.verb,
          ...(op.title === undefined ? {} : { title: op.title }),
        });
        setArmed(answer.armed ?? null);
        if (answer.accepted === false) {
          // A delete's first press is its confirmation owed, not a refusal:
          // the app's arm says which it was, and the row paints it.
          setError(
            answer.armed
              ? null
              : "The service did not change it. Nothing changed.",
          );
        } else {
          setData({ threads: answer.threads, total: answer.total, loaded: answer.loaded });
          setError(
            answer.reread_error
              ? `The change was accepted and the listing read failed: ${answer.reread_error}`
              : null,
          );
          setRenaming(null);
        }
      } catch (failure) {
        setError(failure instanceof Error ? failure.message : String(failure));
      } finally {
        setPending(null);
      }
    },
    [],
  );

  // Clearing one row's unread badge: one `mark_seen` on the service, then the
  // listing it reads back. A refusal leaves the badge exactly where it was.
  const markRead = useCallback(async (id: string) => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    setPending(id);
    try {
      const answer = await call<ThreadOpAnswer>("threads.seen", 120_000, { thread: id });
      setArmed(answer.armed ?? null);
      if (answer.accepted === false) {
        setError("The service did not change it. Nothing changed.");
      } else {
        setData({ threads: answer.threads, total: answer.total, loaded: answer.loaded });
        setError(
          answer.reread_error
            ? `The change was accepted and the listing read failed: ${answer.reread_error}`
            : null,
        );
      }
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setPending(null);
    }
  }, []);

  const threads = useMemo(() => data?.threads ?? [], [data]);
  const main = useMemo(() => threads.filter((thread) => !thread.side), [threads]);
  const side = useMemo(() => threads.filter((thread) => thread.side), [threads]);
  const loaded = data?.loaded ?? false;

  // The native probe reads this: how many conversation rows the page holds.
  (window as unknown as { __MUSE_ROWS__?: number }).__MUSE_ROWS__ = threads.length;

  const count =
    threads.length === 0
      ? ""
      : data && data.total > threads.length
        ? `${threads.length} of ${data.total} threads`
        : `${threads.length} threads`;

  // Pinned rows keep their pride of place above the day groups; the rest
  // file under the group header the native row helper computed.
  let section: string | null = null;

  const line = (item: Row) => (
    <ThreadLine
      key={item.id}
      item={item}
      busy={pending === item.id}
      renaming={renaming === item.id}
      armed={armed === item.id}
      onRename={(title) => void runOp({ id: item.id, verb: "rename", title })}
      onOp={(op) => void runOp(op)}
      onCancelRename={() => setRenaming(null)}
      onCancelDelete={() => setArmed(null)}
      onSeen={() => void markRead(item.id)}
    />
  );

  return (
    <div className="page">
      <div className="top">
        <h1 className="title">Threads</h1>
        <span className="count">{count}</span>
        <button className="refresh" onClick={() => void refresh()} disabled={busy}>
          {busy ? "Refreshing…" : "Refresh"}
        </button>
      </div>

      {error && <div className="banner">Command failed: {error}</div>}
      {data?.error && <div className="banner">The last read failed: {data.error}</div>}

      {!loaded ? (
        <div className="empty">
          {reading
            ? "Reading your conversations now. They will appear here when the answer lands."
            : "The native Threads page has not loaded yet, so there is nothing to show here. Open Threads in the app, then press Refresh — this window is not claiming your account has no conversations."}
        </div>
      ) : threads.length === 0 ? (
        <div className="empty">No threads yet.</div>
      ) : (
        <>
          <div className="rows">
            {main.map((item) => {
              const header = item.pinned || item.group === section ? null : item.group;
              if (!item.pinned) section = item.group;
              return (
                <div key={item.id}>
                  {header && <div className="section">{header}</div>}
                  {line(item)}
                </div>
              );
            })}
          </div>

          {side.length > 0 && (
            <>
              <div className="section">Side chats</div>
              <div className="rows">{side.map((item) => line(item))}</div>
            </>
          )}
        </>
      )}

      <div className="foot">
        Web UI slice of the port: the same rows the native page holds, with the same presses — pin,
        rename, archive and delete go through the app's own `session_op`, and the listing read after
        each one is what this page paints. An archived row files under Archived and offers Unarchive:
        the service stops listing an archived session, so the app remembers it until it comes back.
        Delete asks twice, because a conversation the service removes is gone for good. Opening a
        conversation marks it read the same way, and the row's own Mark read press clears a badge
        without opening anything.
      </div>
    </div>
  );
}
