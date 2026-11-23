import { useCallback, useMemo, useState } from "react";
import { call, channelAvailable } from "./api";
import { useHeldRead } from "./held";
import type { ThreadRow as Row, ThreadsSnapshot } from "./types";

/// One conversation row: pin, title, unread badge, then the listing's
/// snippet and clock time. The day lives in the group header, as the native
/// list paints it.
function ThreadLine({ item }: { item: Row }) {
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
      setData(await call<ThreadsSnapshot>("threads.refresh", 120_000));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  const reading = useHeldRead("threads", loadSnapshot, refresh);

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
                  <ThreadLine item={item} />
                </div>
              );
            })}
          </div>

          {side.length > 0 && (
            <>
              <div className="section">Side chats</div>
              <div className="rows">
                {side.map((item) => (
                  <ThreadLine key={item.id} item={item} />
                ))}
              </div>
            </>
          )}
        </>
      )}

      <div className="foot">
        Web UI slice of the port: read-only, rendered from the same rows the
        native page holds.
      </div>
    </div>
  );
}
