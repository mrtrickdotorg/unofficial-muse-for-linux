import { useCallback, useEffect, useMemo, useState } from "react";
import { call, channelAvailable } from "./api";
import type { SearchRow as Row, SearchSnapshot } from "./types";

/// One result: the icon and title the native row shows, then the
/// kicker/kind/stamp line beneath it. Read-only: the native row opens its
/// conversation in Chat, and this slice has no page to open.
function ResultLine({ item }: { item: Row }) {
  return (
    <div className="search-row">
      <span className="search-icon" aria-hidden="true">
        {item.icon}
      </span>
      <div className="search-words">
        <h3 className="search-title">{item.title}</h3>
        {item.meta && <p className="search-meta">{item.meta}</p>}
      </div>
    </div>
  );
}

/// `initialQuery` is the drawer's own entry point: the shell hands over the
/// words the reader typed there. They are typed, not read -- the page's own Run
/// still makes the one wire read, exactly as it does for words typed here.
export function Search({ initialQuery }: { initialQuery?: string } = {}) {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<SearchSnapshot | null>(
    boot && "query" in boot ? boot : null,
  );
  const [query, setQuery] = useState(
    boot && "query" in boot ? boot.query : (initialQuery ?? ""),
  );
  const [error, setError] = useState<string | null>(null);
  const [hint, setHint] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // What was asked for when the page found nothing: an empty account and an
  // empty result set for these words are different sentences.
  const [asked, setAsked] = useState<string | null>(null);

  // Opening the page costs nothing on the wire: the native side answers
  // with the query it last ran and the rows it painted.
  const loadSnapshot = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    try {
      const held = await call<SearchSnapshot>("search.snapshot");
      setData(held);
      if (held.query) setQuery(held.query);
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }, []);

  useEffect(() => {
    void loadSnapshot();
  }, [loadSnapshot]);

  // A search is a read: it leaves the loop and comes back as rows, the same
  // read the native page makes for these words.
  const run = useCallback(async () => {
    const words = query.trim();
    if (!words) {
      // No words, no read: the native page refuses the same way.
      setHint("Type words to search for first.");
      return;
    }
    setHint(null);
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    setBusy(true);
    try {
      const found = await call<SearchSnapshot>("search.run", 180_000, {
        query: words,
      });
      setData(found);
      setError(null);
      setAsked(words);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }, [query]);

  const rows = useMemo(() => data?.rows ?? [], [data]);
  const loaded = data?.loaded ?? false;

  // The native probe reads this: how many result rows the page holds.
  (window as unknown as { __MUSE_ROWS__?: number }).__MUSE_ROWS__ = rows.length;

  const count =
    rows.length === 0
      ? ""
      : data && data.total > rows.length
        ? `${rows.length} of ${data.total} results`
        : `${rows.length} results`;

  return (
    <div className="page">
      <div className="top">
        <h1 className="title">Search</h1>
        <span className="count">{count}</span>
      </div>

      <form
        className="search-box"
        onSubmit={(event) => {
          event.preventDefault();
          void run();
        }}
      >
        <input
          className="search-input"
          type="search"
          value={query}
          placeholder="Search every conversation"
          aria-label="Search every conversation"
          onChange={(event) => setQuery(event.target.value)}
        />
        <button className="refresh" type="submit" disabled={busy}>
          {busy ? "Searching…" : "Search"}
        </button>
      </form>

      {data?.query && loaded && (
        <div className="search-for">Results for “{data.query}”</div>
      )}
      {hint && <div className="banner">{hint}</div>}
      {error && <div className="banner">Command failed: {error}</div>}
      {data?.error && <div className="banner">The last read failed: {data.error}</div>}

      {!loaded ? (
        <div className="empty">
          The native Search page has not run a search yet, so there is nothing
          to show here. Search in the app, then press Search — this window is
          not claiming your account has no matches.
        </div>
      ) : rows.length === 0 ? (
        <div className="empty">
          {data && data.total > 0
            ? `The service counted ${data.total} matches for “${asked ?? data.query}”, but none carried a conversation this build could open.`
            : asked || data?.query
              ? `No matches for “${asked ?? data?.query}”.`
              : "No matches."}
        </div>
      ) : (
        <div className="rows">
          {rows.map((item) => (
            <ResultLine key={item.id} item={item} />
          ))}
        </div>
      )}

      <div className="foot">
        Web UI slice of the port: read-only, rendered from the same query and
        rows the native search holds.
      </div>
    </div>
  );
}
