import { useCallback, useMemo, useState } from "react";
import { call, channelAvailable } from "./api";
import { useHeldRead } from "./held";
import { groupOf, snippet } from "./listing";
import type { GoalsSnapshot, ListingItem } from "./types";

// The Tracking section's first screenful, and how much each "Show more"
// adds: the native page uses the same step, and the rows are already in
// hand, so expanding never touches the wire.
const TRACKING_PAGE = 10;

const isDone = (item: ListingItem) => item.kind === "completed";

/// The category filters this fill offers: only categories present in the
/// rows in hand, in the order the service sent them.
function categories(goals: ListingItem[]): string[] {
  const kinds: string[] = [];
  for (const goal of goals) {
    const category = goal.category;
    if (category && !kinds.includes(category)) kinds.push(category);
  }
  return kinds;
}

function GoalRow({ item }: { item: ListingItem }) {
  const body = snippet(item.body);
  return (
    <div className="goal">
      <input
        type="checkbox"
        disabled
        checked={isDone(item)}
        aria-label={`${item.title} ${isDone(item) ? "is complete" : "is not complete"}`}
      />
      <div className="goal-words">
        <h3>{item.title}</h3>
        {body && <p className="goal-body">{body}</p>}
      </div>
    </div>
  );
}

export function Goals() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<GoalsSnapshot | null>(
    boot && "goals" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [filter, setFilter] = useState<string | null>(null);
  const [shown, setShown] = useState(TRACKING_PAGE);

  // Opening the page costs nothing on the wire: the native side answers
  // from the roster it already holds.
  const loadSnapshot = useCallback(async (): Promise<GoalsSnapshot | null> => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return null;
    }
    try {
      const answer = await call<GoalsSnapshot>("goals.snapshot");
      setData(answer);
      setError(null);
      return answer;
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      return null;
    }
  }, []);

  // Refresh reads the roster and the Tracking section from the service,
  // which is what the label promises; a listing has taken 27 s here, so
  // the wait is generous rather than a premature timeout.
  const refresh = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    setBusy(true);
    try {
      setData(await call<GoalsSnapshot>("goals.refresh", 120_000));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  const reading = useHeldRead("goals", loadSnapshot, refresh);

  const goals = useMemo(() => data?.goals ?? [], [data]);
  const tracking = data?.tracking ?? [];
  const kinds = useMemo(() => categories(goals), [goals]);
  const visible = useMemo(
    () => (filter ? goals.filter((goal) => goal.category === filter) : goals),
    [goals, filter],
  );

  // The native probe reads this: how many roster rows the page holds.
  (window as unknown as { __MUSE_ROWS__?: number }).__MUSE_ROWS__ = goals.length;

  // The snapshot's own flag, and nothing else: a read that lands on an empty
  // account is a read, and an empty row list alone cannot say which it is.
  const loaded = data?.loaded === true;
  const count =
    goals.length === 0
      ? ""
      : data && data.goals_total > goals.length
        ? `${goals.length} of ${data.goals_total} goals`
        : `${goals.length} goals`;

  // Group headers appear where the group changes, as the native roster
  // paints them.
  let section: string | null = null;

  return (
    <div className="page">
      <div className="top">
        <h1 className="title">Goals</h1>
        <span className="count">{count}</span>
        <button className="refresh" onClick={() => void refresh()} disabled={busy}>
          {busy ? "Refreshing…" : "Refresh"}
        </button>
      </div>

      {error && <div className="banner">Command failed: {error}</div>}

      {!loaded ? (
        <div className="empty">
          {reading
            ? "Reading your Goals now. They will appear here when the answer lands."
            : "The native Goals page has not loaded yet, so there is nothing to show here. Open Goals in the app, then press Refresh — this window is not claiming your Goals page is empty."}
        </div>
      ) : (
        <>
          {data?.tracking_error ? (
            <div className="banner">
              Tracking failed: {data.tracking_error}
            </div>
          ) : null}

          <div className="section">Tracking</div>
          {tracking.length === 0 ? (
            <div className="empty">
              {data?.tracking_error
                ? "The Tracking section could not be read, so it is not shown."
                : "Nothing is being tracked right now."}
            </div>
          ) : (
            <div className="rows">
              {tracking.slice(0, shown).map((item, index) => (
                <GoalRow key={item.id ?? `tracking-${index}`} item={item} />
              ))}
            </div>
          )}
          {tracking.length > shown && (
            <button
              className="tab more"
              onClick={() => setShown(shown + TRACKING_PAGE)}
            >
              Show {tracking.length - shown} more
            </button>
          )}

          <div className="section">Goals</div>
          {kinds.length > 1 && (
            <div className="tabs">
              {["All", ...kinds].map((name) => {
                const active = name === "All" ? filter === null : filter === name;
                return (
                  <button
                    key={name}
                    className={`tab${active ? " active" : ""}`}
                    onClick={() => setFilter(name === "All" ? null : name)}
                  >
                    {name}
                  </button>
                );
              })}
            </div>
          )}

          {visible.length === 0 ? (
            <div className="empty">
              {goals.length === 0
                ? "No goals on this account yet."
                : "No goals match this filter."}
            </div>
          ) : (
            <div className="rows">
              {visible.map((item, index) => {
                const name = groupOf(item);
                const header = name !== section ? name : null;
                section = name;
                return (
                  <div key={item.id ?? `goal-${index}`}>
                    {header && <div className="section">{header}</div>}
                    <GoalRow item={item} />
                  </div>
                );
              })}
            </div>
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
