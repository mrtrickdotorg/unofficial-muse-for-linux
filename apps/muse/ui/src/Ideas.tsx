import { useCallback, useEffect, useMemo, useState } from "react";
import { call, channelAvailable } from "./api";
import { groupOf, snippet } from "./listing";
import type { IdeasSnapshot, ListingItem } from "./types";

function IdeaRow({ item }: { item: ListingItem }) {
  const body = snippet(item.body);
  return (
    <div className="idea">
      <span className="idea-icon" aria-hidden="true">
        {item.icon && item.icon.length > 0 ? item.icon : "💡"}
      </span>
      <div className="idea-words">
        <h3>{item.title}</h3>
        {body && <p className="idea-body">{body}</p>}
      </div>
    </div>
  );
}

export function Ideas() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<IdeasSnapshot | null>(
    boot && "ideas" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Opening the page costs nothing on the wire: the native side answers
  // from the roster it already holds.
  const loadSnapshot = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    try {
      setData(await call<IdeasSnapshot>("ideas.snapshot"));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }, []);

  // Refresh reads the ideas again, which is what the label promises.
  const refresh = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    setBusy(true);
    try {
      setData(await call<IdeasSnapshot>("ideas.refresh", 120_000));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void loadSnapshot();
  }, [loadSnapshot]);

  const ideas = useMemo(() => data?.ideas ?? [], [data]);
  const loaded = data?.loaded ?? false;

  // The native probe reads this: how many roster rows the page holds.
  (window as unknown as { __MUSE_ROWS__?: number }).__MUSE_ROWS__ = ideas.length;

  const count =
    ideas.length === 0
      ? ""
      : data && data.total > ideas.length
        ? `${ideas.length} of ${data.total} ideas`
        : `${ideas.length} ideas`;

  // Group headers appear where the group changes, as the native roster
  // paints them.
  let section: string | null = null;

  return (
    <div className="page">
      <div className="top">
        <h1 className="title">Ideas</h1>
        <span className="count">{count}</span>
        <button className="refresh" onClick={() => void refresh()} disabled={busy}>
          {busy ? "Refreshing…" : "Refresh"}
        </button>
      </div>

      {error && <div className="banner">Command failed: {error}</div>}
      {data?.error && (
        <div className="banner">The last read failed: {data.error}</div>
      )}

      {!loaded ? (
        <div className="empty">
          The native Ideas page has not loaded yet, so there is nothing to show
          here. Open Ideas in the app, then press Refresh — this window is not
          claiming your Ideas page is empty.
        </div>
      ) : ideas.length === 0 ? (
        <div className="empty">No ideas on this account yet.</div>
      ) : (
        <div className="rows">
          {ideas.map((item, index) => {
            const name = groupOf(item);
            const header = name !== section ? name : null;
            section = name;
            return (
              <div key={item.id ?? `idea-${index}`}>
                {header && <div className="section">{header}</div>}
                <IdeaRow item={item} />
              </div>
            );
          })}
        </div>
      )}

      <div className="foot">
        Web UI slice of the port: read-only, rendered from the same rows the
        native page holds.
      </div>
    </div>
  );
}
