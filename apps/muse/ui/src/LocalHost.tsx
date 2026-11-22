import { useCallback, useEffect, useState } from "react";
import { call, channelAvailable } from "./api";
import type { LocalHostSnapshot } from "./types";

/// The controller's phase in the native page's own words.
const PHASE_WORDS: Record<string, string> = {
  disconnected: "not running",
  connecting: "connecting",
  registered: "registered with your account",
};

/// The Local host page: the host's read model, read-only. Every press that
/// changes the boundary -- granting a folder, starting, stopping, allowing or
/// denying a write -- stays on the native page, and this page says so instead
/// of drawing controls that cannot act.
export function LocalHost() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<LocalHostSnapshot | null>(
    boot && "grants" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);

  const loadSnapshot = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    try {
      const held = await call<LocalHostSnapshot>("localhost.snapshot");
      setData(held);
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }, []);

  useEffect(() => {
    void loadSnapshot();
  }, [loadSnapshot]);

  // The host polls its controller every 250 ms and each native paint
  // re-projects the read model, so following it here costs a cache read, not a
  // wire trip. It also keeps a card's countdown honest: the native side
  // recomputes it per paint, and the page shows what it last answered.
  useEffect(() => {
    const timer = setInterval(() => {
      void loadSnapshot();
    }, 1200);
    return () => clearInterval(timer);
  }, [loadSnapshot]);

  const phase = data?.phase ?? "";
  const grants = data?.grants ?? [];
  const cards = data?.pending ?? [];
  const log = data?.log ?? [];

  return (
    <div className="page host-page">
      <div className="top">
        <h1 className="title">Local host</h1>
        <span className="badge host-phase">{PHASE_WORDS[phase] ?? (phase || "unknown")}</span>
      </div>

      {data?.identity && <p className="note">{data.identity}</p>}
      {data?.device && <p className="note host-device">Device {data.device}</p>}
      {/* The controller's notice is its ordinary status line, not a failure:
          the pink banner is kept for a refused read. */}
      {data?.notice && <p className="host-notice">{data.notice}</p>}
      {error && <div className="banner">Command failed: {error}</div>}

      <h2 className="section">Granted folders</h2>
      {grants.length === 0 ? (
        <p className="note">No folders granted.</p>
      ) : (
        <div className="rows">
          {grants.map((grant) => (
            <p className="grant-row" key={`${grant.label}:${grant.root}`}>
              {grant.text}
            </p>
          ))}
        </div>
      )}

      <h2 className="section">Pending file approvals</h2>
      {cards.length === 0 ? (
        <p className="note">No pending approvals.</p>
      ) : (
        <div className="rows">
          {cards.map((card) => (
            <div className="approval-card" key={`${card.operation}:${card.path}`}>
              <h3 className="card-head">
                {card.operation} · {card.path}
              </h3>
              {card.target && <p className="note">→ {card.target}</p>}
              {card.preview && <pre className="preview">{card.preview}</pre>}
              <p className="note">
                {card.deciding ? "Deciding in the app…" : card.countdown} · Allow once
                and Deny stay in the app.
              </p>
            </div>
          ))}
        </div>
      )}

      <h2 className="section">Session activity</h2>
      {log.length === 0 ? (
        <p className="note">Nothing yet.</p>
      ) : (
        <div className="rows">
          {log.map((entry, index) => (
            <p className="log-line" key={`${index}:${entry}`}>
              {entry}
            </p>
          ))}
        </div>
      )}

      <div className="foot">
        Web UI slice of the port: read-only, rendered from the host's own read
        model — the same grants, cards and log the native page paints. Granting,
        starting, stopping, allowing and denying stay in the app.
      </div>
    </div>
  );
}
