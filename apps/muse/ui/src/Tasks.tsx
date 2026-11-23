import { useCallback, useState } from "react";
import { call, channelAvailable } from "./api";
import { useHeldRead } from "./held";
import type { TaskRow, TasksSnapshot } from "./types";

function Rows({ rows }: { rows: TaskRow[] }) {
  return (
    <div className="rows">
      {rows.map((row) => (
        <div className="task" key={row.id}>
          <span>{row.label}</span>
        </div>
      ))}
    </div>
  );
}

export function Tasks() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<TasksSnapshot | null>(
    boot && "schedules" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Opening the page costs nothing on the wire for the schedules and runs:
  // the native side answers from what it already holds, and re-reads the
  // local reminder store because that is local state.
  const loadSnapshot = useCallback(async (): Promise<TasksSnapshot | null> => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return null;
    }
    try {
      const answer = await call<TasksSnapshot>("tasks.snapshot");
      setData(answer);
      setError(null);
      return answer;
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      return null;
    }
  }, []);

  // Refresh reads the service again, which is what the label promises; a
  // listing has taken 27 s here, so the wait is generous rather than a
  // premature timeout.
  const refresh = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    setBusy(true);
    try {
      setData(await call<TasksSnapshot>("tasks.refresh", 120_000));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  const reading = useHeldRead("tasks", loadSnapshot, refresh);

  const schedules = data?.schedules ?? [];
  const runs = data?.runs ?? [];
  const local = data?.local ?? [];
  const loaded = data?.loaded ?? false;

  return (
    <div className="page">
      <div className="top">
        <h1 className="title">Tasks</h1>
        <span className="count">
          {loaded
            ? `${schedules.length} scheduled · ${runs.length} recent`
            : ""}
        </span>
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
          {reading
            ? "Reading your schedules and runs now. They will appear here when the answer lands."
            : "The native Tasks page has not loaded yet, so its schedules and runs are not shown here. Open Tasks in the app, then press Refresh — this window is not claiming they are empty."}
        </div>
      ) : (
        <>
          <div className="section">Scheduled</div>
          {schedules.length === 0 ? (
            <div className="empty">No scheduled jobs on this account.</div>
          ) : (
            <Rows rows={schedules} />
          )}

          <div className="section">Recent runs</div>
          {runs.length === 0 ? (
            <div className="empty">No runs yet.</div>
          ) : (
            <Rows rows={runs} />
          )}
        </>
      )}

      {/* Local reminders are local state, in the store the runner reads:
          they are shown whether or not the service read has landed. */}
      <div className="section">Local reminders</div>
      {local.length === 0 ? (
        <div className="empty">No local reminders.</div>
      ) : (
        <Rows rows={local} />
      )}

      <div className="foot">
        Web UI slice of the port: read-only, rendered from the same rows the
        native page holds.
      </div>
    </div>
  );
}
