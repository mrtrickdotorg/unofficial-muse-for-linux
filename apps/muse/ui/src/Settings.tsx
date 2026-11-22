import { useCallback, useEffect, useState } from "react";
import { call, channelAvailable } from "./api";
import type { SettingsSnapshot, SettingsUpdates } from "./types";

/// The app's own sentence, word for word: the native card shows the same
/// string, so the page adds no clause of its own -- not while checking, not
/// when a download is waiting, not ever. The buttons carry what the state
/// offers next.
function updatesSentence(updates: SettingsUpdates): string {
  return (
    updates.status ||
    (updates.appimage
      ? "Updates come from the rolling AppImage release."
      : "This copy updates through its own channel (store or checkout), not here.")
  );
}

function Row({
  label,
  value,
  className,
  hint,
}: {
  label: string;
  value: string;
  className?: string;
  hint?: string;
}) {
  return (
    <div className={className ? `settings-row ${className}` : "settings-row"}>
      <span className="settings-label">{label}</span>
      <span className="settings-value">
        {value}
        {hint && <span className="note"> · {hint}</span>}
      </span>
    </div>
  );
}

function sentence(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

export function Settings() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<SettingsSnapshot | null>(
    boot && "link" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);
  // One settings change at a time: the app holds a single ticket for the
  // panel's presses, so every flip control waits while one is in flight --
  // the native panel's own rule, since a second press there is refused.
  const [flipping, setFlipping] = useState<string | null>(null);
  // A connector reset is two presses on the same row: the first arms it and
  // a repaint disarms, which is the native panel's safe default.
  const [armed, setArmed] = useState<string | null>(null);
  const [resetting, setResetting] = useState<string | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);
  // The updater's own one press at a time: the app holds a single check and a
  // single installer, and refuses a second press while one runs.
  const [updating, setUpdating] = useState<string | null>(null);
  const [updateRefusal, setUpdateRefusal] = useState<string | null>(null);

  // Opening the page costs nothing: the native side answers with the state its
  // own paints left behind.
  const loadSnapshot = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    try {
      const held = await call<SettingsSnapshot>("settings.snapshot");
      setData(held);
      setError(null);
    } catch (failure) {
      setError(sentence(failure));
    }
  }, []);

  useEffect(() => {
    void loadSnapshot();
  }, [loadSnapshot]);

  // An unread policy is not an empty one: the read is the chat panel's, asked
  // for when this page opens, and it lands after the first answer by an amount
  // this page cannot know. The app's own paint says when the projection changed
  // -- it calls this hook -- and only then does the page ask again. No timer,
  // no guessing at how long a read takes, and a policy that changes while the
  // window is open reaches it the same way.
  useEffect(() => {
    window.musePolicyReady = () => {
      void loadSnapshot();
    };
    // The updater's answer lands after the press that asked for it has already
    // answered, and it can be a press from the native card. The app says when
    // it is in, and the page reads again -- no timer, no guessed interval.
    window.museUpdatesReady = () => {
      void loadSnapshot();
    };
    return () => {
      delete window.musePolicyReady;
      delete window.museUpdatesReady;
    };
  }, [loadSnapshot]);

  const policy = data?.policy;
  const connectors = policy?.connectors.join("|") ?? "";
  // A repaint disarms the reset row: the connectors list changing is the paint.
  useEffect(() => {
    setArmed(null);
  }, [connectors]);

  const flip = async (label: string, value: string) => {
    if (flipping || resetting) return;
    setFlipping(label);
    setRefusal(null);
    try {
      // The app's own table decides what the row becomes; the page sends the
      // row it shows, not a word it made up.
      await call("settings.policy.set", 15_000, { label, value });
    } catch (failure) {
      setRefusal(sentence(failure));
    } finally {
      setFlipping(null);
    }
  };

  const reset = async (connector: string) => {
    if (resetting || flipping) return;
    if (armed !== connector) {
      setArmed(connector);
      return;
    }
    setArmed(null);
    setResetting(connector);
    setRefusal(null);
    try {
      await call("settings.connector.reset", 15_000, { connector });
    } catch (failure) {
      setRefusal(sentence(failure));
    } finally {
      setResetting(null);
    }
  };

  const updatePress = async (command: string) => {
    if (updating) return;
    setUpdating(command);
    setUpdateRefusal(null);
    try {
      // The check and the download run in the app, on its own threads; the
      // answer is the state, so the page reads it back either way.
      await call(command, 120_000);
    } catch (failure) {
      setUpdateRefusal(sentence(failure));
    } finally {
      setUpdating(null);
      await loadSnapshot();
    }
  };

  const link = data?.link;
  const updates = data?.updates;

  return (
    <div className="page settings-page">
      <div className="top">
        <h1 className="title">Settings</h1>
      </div>

      {error && <div className="banner">Command failed: {error}</div>}

      <h2 className="section">This app</h2>
      {link?.linked ? (
        <div className="linked">
          {/* The native label's own first word, so the two surfaces open the
              section the same way. */}
          <p className="note">Linked.</p>
          <Row label="Interpreter" value={link.interpreter} />
          <Row label="Session folder" value={link.session_folder} />
        </div>
      ) : (
        <p className="note">
          No saved link. Sign in from the native page; this window only shows
          what the app holds.
        </p>
      )}

      <h2 className="section">Updates</h2>
      <p className="note updates-status">
        {updates
          ? updatesSentence(updates)
          : "The native page has not painted its updates section yet."}
      </p>
      <div className="host-actions">
        {/* No check to offer, so no press drawn -- the native card's rule. A
            press during a check stays possible otherwise: the app refuses it
            with its own sentence and the page shows that. */}
        {updates?.appimage && (
          <button
            type="button"
            className="inline-button"
            disabled={updating !== null}
            onClick={() => void updatePress("update.check")}
          >
            {updating === "update.check" || updates.busy ? "Working…" : updates.check_label}
          </button>
        )}
        {updates?.action_label && (
          <button
            type="button"
            className="inline-button"
            disabled={updating !== null || updates.busy}
            onClick={() => void updatePress("update.install")}
          >
            {updating === "update.install" ? "Working…" : updates.action_label}
          </button>
        )}
      </div>
      {updateRefusal && <p className="note write-refusal">{updateRefusal}</p>}

      <h2 className="section">Policy</h2>
      {policy?.loaded ? (
        <div className="policy-loaded">
          {policy.rows.map((row) =>
            row.flip ? (
              <div className="settings-row policy-row" key={row.label}>
                <span className="settings-label">{row.label}</span>
                <span className="settings-value">
                  {row.value}
                  <button
                    type="button"
                    className="inline-button"
                    disabled={flipping !== null || resetting !== null}
                    onClick={() => void flip(row.label, row.value)}
                  >
                    {flipping === row.label ? "Setting…" : `Set to ${row.flip}`}
                  </button>
                </span>
              </div>
            ) : (
              <Row key={row.label} label={row.label} value={row.value} className="policy-row" />
            ),
          )}
          {policy.connectors.map((name) => (
            <div className="settings-row policy-row" key={`connector-${name}`}>
              <span className="settings-label">{name}</span>
              <span className="settings-value">
                connector grants
                <button
                  type="button"
                  className="inline-button danger"
                  disabled={flipping !== null || resetting !== null}
                  onClick={() => void reset(name)}
                >
                  {resetting === name
                    ? "Resetting…"
                    : armed === name
                      ? "Confirm reset"
                      : "Reset"}
                </button>
              </span>
            </div>
          ))}
          {refusal && <p className="note write-refusal">{refusal}</p>}
          {policy.withheld > 0 && (
            <p className="note">
              {policy.withheld} setting(s) the service withheld.
            </p>
          )}
          {policy.rows.length === 0 && policy.connectors.length === 0 && (
            <p className="note">No policy settings.</p>
          )}
        </div>
      ) : (
        <p className="note">
          {policy?.note ??
            "The chat panel has not read policy settings in this session, so this page is not claiming there are none."}
        </p>
      )}

      <div className="foot">
        Web UI slice of the port: rendered from the same link, update state and
        policy the native pages hold. A policy flip and a connector reset go
        through the app's own update path; check, download, restart and sign out
        stay in the app.
      </div>
    </div>
  );
}
