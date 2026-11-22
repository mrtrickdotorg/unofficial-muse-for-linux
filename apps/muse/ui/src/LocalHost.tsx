import { useCallback, useEffect, useState } from "react";
import { call, channelAvailable } from "./api";
import type { LocalHostSnapshot } from "./types";

/// The controller's phase in the native page's own words.
const PHASE_WORDS: Record<string, string> = {
  disconnected: "not running",
  connecting: "connecting",
  registered: "registered with your account",
};

/// The native page's consent sentence, word for word: the page must not
/// describe the boundary in softer terms than the window that enforces it.
const CONSENT =
  "I explicitly enable this local host and allow granted filenames and text to reach Muse.";
const READ_ONLY =
  "Grant read-only: folders granted while this is on can be read and searched, but Muse can never propose a write in them.";

/// The presses this page can have in flight: one at a time, because the host is
/// one boundary and a second press must not race the first.
type Press = "grant" | "start" | "stop" | "decide" | "policy" | "clear";

function sentence(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

/// The Local host page: the host's read model, plus every press the native page
/// offers -- grant a folder, start, stop, Allow once/Deny on one pending write,
/// the saved write permission, and Clear log. Each press is the app's own call,
/// keeps the page's one-press-one-write rule, and its outcome arrives in the
/// next snapshot.
export function LocalHost() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<LocalHostSnapshot | null>(
    boot && "grants" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);
  const [consent, setConsent] = useState(false);
  const [readOnly, setReadOnly] = useState(true);
  // One write at a time: the host is one boundary, so a start cannot race a
  // grant. A second press on any control is ignored until the first answers.
  const [writing, setWriting] = useState<Press | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);

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
      setError(sentence(failure));
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

  const write = async (
    kind: Press,
    command: string,
    extra: Record<string, unknown>,
  ) => {
    if (writing) return;
    setWriting(kind);
    setRefusal(null);
    try {
      await call(command, 15_000, extra);
      // The grant dialog answers on its own; a start or stop has landed by the
      // time the app replies, so re-read now instead of waiting for the pump.
      if (kind !== "grant") await loadSnapshot();
      if (kind === "stop") setConsent(false);
    } catch (failure) {
      setRefusal(sentence(failure));
    } finally {
      setWriting(null);
    }
  };

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
          the pink banner is kept for a refused read or write. */}
      {data?.notice && <p className="host-notice">{data.notice}</p>}
      {error && <div className="banner">Command failed: {error}</div>}
      {refusal && <div className="banner">Command failed: {refusal}</div>}

      <div className="host-boundary">
        <label className="host-check">
          <input
            type="checkbox"
            checked={consent}
            onChange={(event) => setConsent(event.target.checked)}
          />
          <span>{CONSENT}</span>
        </label>
        <label className="host-check">
          <input
            type="checkbox"
            checked={readOnly}
            onChange={(event) => setReadOnly(event.target.checked)}
          />
          <span>{READ_ONLY}</span>
        </label>
        <div className="host-actions">
          <button
            type="button"
            className="inline-button"
            disabled={writing !== null}
            onClick={() =>
              void write("grant", "localhost.grant", {
                consent,
                writable: !readOnly,
              })
            }
          >
            {writing === "grant" ? "Choosing a folder…" : "Grant a folder to Muse…"}
          </button>
          <button
            type="button"
            className="inline-button"
            disabled={writing !== null}
            onClick={() => void write("start", "localhost.start", { consent })}
          >
            {writing === "start" ? "Starting…" : "Start local host"}
          </button>
          <button
            type="button"
            className="inline-button danger"
            disabled={writing !== null}
            onClick={() => void write("stop", "localhost.stop", {})}
          >
            {writing === "stop" ? "Stopping…" : "Stop & revoke all"}
          </button>
        </div>
      </div>

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

      <label className="host-check">
        <input
          type="checkbox"
          checked={data?.policy_on ?? false}
          disabled={writing !== null}
          onChange={(event) =>
            void write("policy", "localhost.policy", { allow: event.target.checked })
          }
        />
        <span>{data?.policy_sentence ?? ""}</span>
      </label>

      <h2 className="section">
        {data?.approvals_head ?? "Pending file approvals"}
      </h2>
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
                {card.deciding ? "Deciding in the app…" : card.countdown}
              </p>
              <div className="host-actions">
                <button
                  type="button"
                  className="inline-button"
                  disabled={writing !== null || card.deciding}
                  onClick={() =>
                    void write("decide", "localhost.decide", {
                      approval_id: card.id,
                      allow: true,
                    })
                  }
                >
                  Allow once
                </button>
                <button
                  type="button"
                  className="inline-button danger"
                  disabled={writing !== null || card.deciding}
                  onClick={() =>
                    void write("decide", "localhost.decide", {
                      approval_id: card.id,
                      allow: false,
                    })
                  }
                >
                  Deny
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      <div className="host-actions">
        <h2 className="section">
          {data?.activity_head ?? "Session activity"}
        </h2>
        <button
          type="button"
          className="inline-button"
          disabled={writing !== null}
          onClick={() => void write("clear", "localhost.clear_log", {})}
        >
          {writing === "clear" ? "Clearing…" : "Clear log"}
        </button>
      </div>
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
        Web UI slice of the port: rendered from the host's own read model — the
        same grants, cards and log the native page paints. Every boundary press
        is here, and each one is the app's own call: granting, starting,
        stopping, Allow once/Deny on a card, the saved write permission and
        Clear log.
      </div>
    </div>
  );
}
