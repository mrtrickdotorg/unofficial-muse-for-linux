import { useCallback, useEffect, useState } from "react";
import { call, channelAvailable } from "./api";
import type { SettingsSnapshot, SettingsUpdates } from "./types";

/// The native section's own sentence, plus what its buttons would say next.
/// The page never offers those presses -- it names the state they would act
/// on, so the two surfaces cannot tell different stories.
function updatesSentence(updates: SettingsUpdates): string {
  const status =
    updates.status ||
    (updates.appimage
      ? "Updates come from the rolling AppImage release."
      : "This copy updates through its own channel (store or checkout), not here.");
  if (updates.staged) {
    return `${status} An update is downloaded; restart from the native page to take it.`;
  }
  if (updates.busy) {
    return `${status} Checking now…`;
  }
  if (updates.checked) {
    return status;
  }
  return status;
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

export function Settings() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<SettingsSnapshot | null>(
    boot && "link" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);

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
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }, []);

  useEffect(() => {
    void loadSnapshot();
  }, [loadSnapshot]);

  const link = data?.link;
  const updates = data?.updates;
  const policy = data?.policy;

  return (
    <div className="page">
      <div className="top">
        <h1 className="title">Settings</h1>
      </div>

      {error && <div className="banner">Command failed: {error}</div>}

      <h2 className="section">This app</h2>
      {link?.linked ? (
        <div className="linked">
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

      <h2 className="section">Policy</h2>
      {policy?.loaded ? (
        <div className="policy-loaded">
          {policy.rows.map((row) => (
            <Row
              key={row.label}
              label={row.label}
              value={row.value}
              className="policy-row"
              hint={row.flip ? `set to ${row.flip} in the app` : undefined}
            />
          ))}
          {policy.connectors.map((name) => (
            <Row
              key={`connector-${name}`}
              label={name}
              value="connector grants"
              className="policy-row"
              hint="resettable in the app"
            />
          ))}
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
        Web UI slice of the port: read-only, rendered from the same link, update
        state and policy the native pages hold. Check, download, restart and
        sign out stay in the app.
      </div>
    </div>
  );
}
