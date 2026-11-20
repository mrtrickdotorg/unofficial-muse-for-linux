// The JS -> Rust channel: one JSON round trip per command, replied to by the
// native side through window.museReply. Outside the app (a plain browser
// preview) the handlers are absent, and callers fall back to boot data.
import type { Boot } from "./types";

type Reply = { ok?: boolean; error?: string } & Record<string, unknown>;

const pending = new Map<number, (reply: Reply) => void>();
let nextId = 1;

declare global {
  interface Window {
    webkit?: { messageHandlers?: { muse?: { postMessage(body: string): void } } };
    museReply?: (id: number, reply: Reply) => void;
    __MUSE_BOOT__?: Boot;
  }
}

window.museReply = (id, reply) => {
  const settle = pending.get(id);
  if (settle) {
    pending.delete(id);
    settle(reply);
  }
};

export function channelAvailable(): boolean {
  return Boolean(window.webkit?.messageHandlers?.muse);
}

export function call<T>(cmd: string, timeoutMs = 15_000): Promise<T> {
  const handler = window.webkit?.messageHandlers?.muse;
  if (!handler) return Promise.reject(new Error("no channel"));
  const id = nextId++;
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error("timed out"));
    }, timeoutMs);
    pending.set(id, (reply) => {
      clearTimeout(timer);
      if (reply && reply.ok === false) {
        reject(new Error(String(reply.error || "refused")));
      } else {
        resolve(reply as T);
      }
    });
    handler.postMessage(JSON.stringify({ id, cmd }));
  });
}
