import { useCallback, useEffect, useState } from "react";
import { call, channelAvailable } from "./api";
import { useHeldRead } from "./held";
import { usePageNav } from "./navigation";
import type { UpcomingSnapshot } from "./types";

/// Upcoming: the run the account has next and the approvals waiting on the
/// reader, as the Mac client opens that surface. Both halves are held state --
/// the app answers from what its own reads already said -- so opening the page
/// costs nothing on the wire and cannot queue behind a listing.
export function Upcoming() {
  const onNavigate = usePageNav();
  const [data, setData] = useState<UpcomingSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (): Promise<UpcomingSnapshot | null> => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return null;
    }
    try {
      const answer = await call<UpcomingSnapshot>("upcoming.snapshot");
      setData(answer);
      setError(null);
      return answer;
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      return null;
    }
  }, []);

  // The run this page names lives in the Tasks listing, so the owed read is
  // that listing first and this page's snapshot after it, in that order.
  useHeldRead(
    "tasks",
    load,
    async () => {
      await call("tasks.refresh", 120_000);
      await load();
    },
    (answer) => answer.schedules_loaded === false,
  );

  // A Tasks read landing anywhere in the app moves the run this page names.
  // The app nudges this hook rather than pushing rows, so the page re-asks its
  // own snapshot and never keeps the "not read yet" sentence from before that
  // read. Nothing here polls.
  useEffect(() => {
    window.museUpcomingReady = () => void load();
    return () => {
      window.museUpcomingReady = undefined;
    };
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
          {/* The row opens Chat, where the decision lives: Upcoming names a
              waiting approval and hands the reader to its card, and never
              offers a decision of its own. */}
          {approvals.cards.map((card) => (
            <button
              type="button"
              className="task task-link"
              key={card.id}
              title="Open in Chat"
              aria-label={`${card.headline} — open in Chat`}
              onClick={() => onNavigate("chat")}
            >
              <span>{card.headline}</span>
              {card.summary && <span className="meta">{card.summary}</span>}
              <span className="task-hint">Open in Chat</span>
            </button>
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
