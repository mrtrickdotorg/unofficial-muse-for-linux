import { useCallback, useEffect, useState } from "react";
import { call, channelAvailable } from "./api";
import type { SigninSnapshot } from "./types";

/// The presses this page can have in flight: one at a time, because the app
/// keeps one browser flow and one save.
type Press = "connect" | "browser" | "cancel" | "choose";

function sentence(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

/// The sign-in surface: the interpreter, the session folder, the consent, and
/// the three presses the native form has -- Connect, Sign in with browser, and
/// the folder chooser. Every answer is the app's own: a link the app would
/// refuse is refused here in the same words, because the validation and the
/// save are the app's functions, not this page's.
export function Signin() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<SigninSnapshot | null>(
    boot && "consent_sentence" in boot ? boot : null,
  );
  const [python, setPython] = useState(data?.python ?? "");
  const [folder, setFolder] = useState(data?.folder ?? "");
  const [consent, setConsent] = useState(data?.consent ?? false);
  // A refused press and a failed read are different things: the read's own
  // failure is cleared by the next good answer, a refusal stands until the
  // next press, so the page cannot lose the sentence it was shown.
  const [error, setError] = useState<string | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);
  const [pressing, setPressing] = useState<Press | null>(null);

  const loadSnapshot = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    try {
      const held = await call<SigninSnapshot>("signin.snapshot");
      setData(held);
      // The folder the chooser returned is the app's answer, not this page's
      // guess: take it from the snapshot, and keep what was typed otherwise.
      if (held.folder) setFolder(held.folder);
      setError(null);
    } catch (failure) {
      setError(sentence(failure));
    }
  }, []);

  useEffect(() => {
    void loadSnapshot();
  }, [loadSnapshot]);

  const press = async (kind: Press) => {
    if (pressing) return;
    setPressing(kind);
    setRefusal(null);
    try {
      await call("signin.press", 30_000, {
        press: kind,
        python,
        folder,
        consent,
      });
    } catch (failure) {
      setRefusal(sentence(failure));
    } finally {
      await loadSnapshot();
      setPressing(null);
    }
  };

  const busy = pressing !== null;
  const browserOpen = Boolean(data?.browser_open);

  return (
    <div className="page signin-page">
      <div className="top">
        <h1 className="title">{data?.title ?? "Sign in"}</h1>
      </div>

      {data?.hint && <p className="note">{data.hint}</p>}
      {(error || refusal) && (
        <div className="banner">Command failed: {error ?? refusal}</div>
      )}
      {data?.error && <div className="banner">{data.error}</div>}
      {data?.status && <p className="note signin-status">{data.status}</p>}

      <label className="field">
        <span className="settings-label">Python</span>
        <input
          type="text"
          value={python}
          spellCheck={false}
          onChange={(event) => setPython(event.target.value)}
        />
      </label>
      <label className="field">
        <span className="settings-label">Session folder</span>
        <input
          type="text"
          value={folder}
          spellCheck={false}
          onChange={(event) => setFolder(event.target.value)}
        />
      </label>

      <label className="host-check">
        <input
          type="checkbox"
          checked={consent}
          onChange={(event) => setConsent(event.target.checked)}
        />
        {data?.consent_sentence ??
          "I consent to the app using the session this sign-in creates."}
      </label>

      <div className="host-actions">
        <button
          type="button"
          className="inline-button"
          disabled={busy || browserOpen}
          onClick={() => void press("connect")}
        >
          {pressing === "connect" ? "Signing in…" : (data?.connect_label ?? "Connect")}
        </button>
        <button
          type="button"
          className="inline-button"
          disabled={busy || browserOpen}
          onClick={() => void press("browser")}
        >
          {pressing === "browser" ? "Opening…" : (data?.browser_label ?? "Sign in with browser")}
        </button>
        <button
          type="button"
          className="inline-button"
          disabled={busy}
          onClick={() => void press("choose")}
        >
          {data?.choose_label ?? "Choose folder…"}
        </button>
        {browserOpen && (
          <button
            type="button"
            className="inline-button danger"
            disabled={busy}
            onClick={() => void press("cancel")}
          >
            {data?.cancel_label ?? "Cancel"}
          </button>
        )}
      </div>
    </div>
  );
}
