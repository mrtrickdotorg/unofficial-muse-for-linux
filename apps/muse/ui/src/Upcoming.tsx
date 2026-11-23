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

  // `chat.approvals.refresh` answers when the read is dispatched, not when it
  // lands, so the page follows its own held state until the cards arrive. The
  // read is in progress for as long as that takes, and the sentence says so
  // rather than flashing "nothing has been read" back. Bounded, because a
  // refused read leaves the note instead of an answer to wait for.
  const readApprovals = useCallback(async () => {
    await call("chat.approvals.refresh", 30_000);
    for (let tries = 0; tries < 40; tries += 1) {
      const held = await load();
      if (!held || held.approvals.loaded !== false) return;
      await new Promise((settle) => setTimeout(settle, 250));
    }
  }, [load]);

  // The run this page names lives in the Tasks listing and its cards in the
  // approvals read, so an open asks for each half that has not been read --
  // the hook hands back the answer it decided from -- and then asks its own
  // snapshot again to paint what those reads left.
  const reading = useHeldRead(
    "upcoming",
    load,
    async (held) => {
      if (held.schedules_loaded === false) {
        await call("tasks.refresh", 120_000);
      }
      if (held.approvals.loaded === false) {
        await readApprovals();
        return;
      }
      await load();
    },
    (answer) =>
      answer.schedules_loaded === false || answer.approvals.loaded === false,
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
          {reading
            ? "Reading the schedule listing now. The next run will appear here when it lands."
            : "The schedule listing has not been read yet. Open Tasks in the app, then come back — this page is not claiming nothing is scheduled."}
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
          {reading && !approvals.note
            ? "Reading the approvals now. They will appear here when they land."
            : approvals.note ?? "The approvals have not been read yet."}
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
