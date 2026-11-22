import { useEffect, useState } from "react";
import { call, channelAvailable } from "./api";
import type { FirstRunSnapshot } from "./types";

function sentence(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

/// The first-run step: two lines and a spinner. It has nothing to press, and
/// it says so -- the file writes are the app's, on its own thread, and this
/// page only reads whether they have finished.
export function FirstRun() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<FirstRunSnapshot | null>(
    boot && "head" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    let live = true;
    const ask = async () => {
      try {
        const held = await call<FirstRunSnapshot>("firstrun.snapshot");
        if (!live) return;
        setData(held);
        setError(null);
        // Nothing to press and nothing to poll once it is done: the step is
        // over, and the page stops asking.
        if (!held.done) timer = setTimeout(() => void ask(), 400);
      } catch (failure) {
        if (live) setError(sentence(failure));
      }
    };
    let timer: ReturnType<typeof setTimeout> | undefined;
    void ask();
    return () => {
      live = false;
      if (timer) clearTimeout(timer);
    };
  }, []);

  const done = Boolean(data?.done);

  return (
    <div className="page firstrun-page">
      <div className="top">
        <h1 className="title firstrun-head">{data?.head ?? "Setting up Muse"}</h1>
      </div>
      <p className="note">
        {data?.sub ??
          "The app is preparing its folders. Nothing leaves this machine."}
      </p>
      {error && <div className="banner">Command failed: {error}</div>}
      {/* Read-only in words: a spinner while the app works, and a plain line
          when it is done, with no control the page could pretend to offer. */}
      <p className="note">
        {done ? "Done. The app window opens on its own." : "Working… this step has no presses."}
      </p>
    </div>
  );
}
