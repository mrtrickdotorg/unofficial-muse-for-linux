import { useCallback, useEffect, useState } from "react";
import { call, channelAvailable } from "./api";
import type { UpcomingSnapshot } from "./types";

/// Upcoming: the run the account has next and the approvals waiting on the
/// reader, as the Mac client opens that surface. Both halves are held state --
/// the app answers from what its own reads already said -- so opening the page
/// costs nothing on the wire and cannot queue behind a listing.
export function Upcoming() {
  const [data, setData] = useState<UpcomingSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    try {
      setData(await call<UpcomingSnapshot>("upcoming.snapshot"));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }, []);

  // One read on mount, the way every page asks its own snapshot. Nothing here
  // polls: a run or an approval that lands later shows on the next visit.
  useEffect(() => {
    void load();
  }, [load]);

  const approvals = data?.approvals ?? null;

  return (
    <div className="page">
      <div className="top">
        <h1 className="title">Upcoming</h1>
      </div>

      {error && <div className="banner">Command failed: {error}</div>}

      <div className="section">Next scheduled run</div>
      {!data ? (
        <div className="empty">Reading what the app holds…</div>
      ) : !data.schedules_loaded ? (
        <div className="empty">
          The schedule listing has not been read yet. Open Tasks in the app,
          then come back — this page is not claiming nothing is scheduled.
        </div>
      ) : data.next_run ? (
        <div className="rows">
          <div className="task">
            <div>{data.next_run.title}</div>
            <div className="meta">{data.next_run.when}</div>
          </div>
        </div>
      ) : (
        <div className="empty">No run scheduled.</div>
      )}

      <div className="section">Waiting on you</div>
      {!approvals ? (
        <div className="empty">Reading what the app holds…</div>
      ) : !approvals.loaded ? (
        <div className="empty">
          {approvals.note ?? "The approvals have not been read yet."}
        </div>
      ) : approvals.cards.length === 0 ? (
        <div className="empty">No pending approvals.</div>
      ) : (
        <div className="rows">
          {approvals.cards.map((card) => (
            <div className="task" key={card.id}>
              <div>{card.headline}</div>
              {card.summary && <div className="meta">{card.summary}</div>}
            </div>
          ))}
        </div>
      )}

      <div className="foot">
        Read-only here: Allow once and Deny stay on Chat's cards, where the
        app's own decision path reads them.
      </div>
    </div>
  );
}
