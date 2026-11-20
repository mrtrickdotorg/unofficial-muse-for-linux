import { useCallback, useEffect, useMemo, useState } from "react";
import { call, channelAvailable } from "./api";
import type { Artifact, LibrarySnapshot } from "./types";

const TABS = ["All", "Spaces", "Files"] as const;
type Tab = (typeof TABS)[number];

const isSpace = (a: Artifact) => a.target_kind === "space";

function relative(ms?: number | null): string {
  if (!ms) return "";
  const days = Math.floor((Date.now() - ms) / 86_400_000);
  if (days <= 0) return "today";
  if (days === 1) return "yesterday";
  if (days < 30) return `${days} days ago`;
  const months = Math.floor(days / 30);
  if (months < 12) return `${months} ${months === 1 ? "month" : "months"} ago`;
  const years = Math.floor(months / 12);
  return `${years} ${years === 1 ? "year" : "years"} ago`;
}

function Card({ artifact }: { artifact: Artifact }) {
  const label = isSpace(artifact)
    ? "space"
    : artifact.format || artifact.kind || "file";
  return (
    <div className="card">
      <h3>{artifact.title}</h3>
      <div className="meta">
        {isSpace(artifact) && <span className="badge space">{label}</span>}
        {artifact.pinned && <span className="badge">pinned</span>}
        <span>{relative(artifact.updated_at_ms)}</span>
      </div>
    </div>
  );
}

export function Library() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<LibrarySnapshot | null>(
    boot && "artifacts" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tab, setTab] = useState<Tab>("All");
  const [query, setQuery] = useState("");

  // Opening the page costs nothing on the wire: the native side answers
  // from the rows it already holds, which is the same list the boot payload
  // came from.
  const loadSnapshot = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    try {
      setData(await call<LibrarySnapshot>("library.snapshot"));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }, []);

  // Refresh reads the service again, which is what the label promises. A
  // listing has taken 27 s against this service, so the wait is generous
  // rather than a premature "timed out" over rows that are about to arrive.
  const refresh = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    setBusy(true);
    try {
      setData(await call<LibrarySnapshot>("library.refresh", 120_000));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  // Ask the native side for its rows on mount: boot data is a head start,
  // never the source of truth.
  useEffect(() => {
    void loadSnapshot();
  }, [loadSnapshot]);

  const rows = useMemo(() => {
    const all = data?.artifacts ?? [];
    const needle = query.trim().toLowerCase();
    return all.filter((artifact) => {
      if (needle && !artifact.title.toLowerCase().includes(needle)) return false;
      if (tab === "Spaces") return isSpace(artifact);
      if (tab === "Files") return !isSpace(artifact);
      return true;
    });
  }, [data, query, tab]);

  // The native probe reads this: how many rows the page actually holds.
  (window as unknown as { __MUSE_ROWS__?: number }).__MUSE_ROWS__ =
    (data?.artifacts ?? []).length;

  const pinned = rows.filter((a) => a.pinned);
  const rest = rows.filter((a) => !a.pinned);
  const total = data?.total ?? 0;
  const loaded = (data?.artifacts ?? []).length > 0;

  return (
    <div className="page">
      <div className="top">
        <h1 className="title">Library</h1>
        <span className="count">
          {loaded && total > 0
            ? `${data!.artifacts.length} of ${total} items`
            : loaded
              ? `${data!.artifacts.length} items`
              : ""}
        </span>
        <button className="refresh" onClick={() => void refresh()} disabled={busy}>
          {busy ? "Refreshing…" : "Refresh"}
        </button>
      </div>

      <input
        className="search"
        placeholder="Quick Open"
        autoComplete="off"
        value={query}
        onChange={(event) => setQuery(event.target.value)}
      />

      <div className="tabs">
        {TABS.map((name) => (
          <button
            key={name}
            className={`tab${tab === name ? " active" : ""}`}
            onClick={() => setTab(name)}
          >
            {name}
          </button>
        ))}
      </div>

      {error && <div className="banner">Command failed: {error}</div>}

      {!loaded ? (
        <div className="empty">
          The native Library page has not loaded yet, so there is nothing to
          show here. Open Library in the app, then press Refresh — this window
          is not claiming your Library is empty.
        </div>
      ) : rows.length === 0 ? (
        <div className="empty">Nothing in the Library matches this filter.</div>
      ) : (
        <>
          {pinned.length > 0 && (
            <>
              <div className="section">Pinned</div>
              <div className="grid">
                {pinned.map((a) => (
                  <Card key={a.artifact_ref} artifact={a} />
                ))}
              </div>
            </>
          )}
          {rest.length > 0 && (
            <>
              <div className="section">All</div>
              <div className="grid">
                {rest.map((a) => (
                  <Card key={a.artifact_ref} artifact={a} />
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
