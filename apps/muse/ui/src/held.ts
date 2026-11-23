// One rule for every page's first open: a page paints the app's held rows, and
// when the app says it holds none, the page owes exactly one read.
//
// It owes that read because the shell can be the only surface: the held rows
// are filled by the native page's paint, and a shell launch never runs one, so
// `loaded: false` is "nothing has been read", never an empty account. Strictly
// false -- an answer that does not say is not a claim of emptiness. Nothing
// polls: the read happens once on mount, and the held rows take over after it.
import { useEffect, useRef } from "react";

/// Pages whose owed read is still running in this window, by page. A page
/// remounted while its own read is in flight -- a rail press away and back --
/// waits for that read instead of starting a second one.
const running = new Map<string, Promise<void>>();

const unreadByDefault = (answer: unknown): boolean =>
  (answer as { loaded?: boolean }).loaded === false;

export function useHeldRead<T>(
  page: string,
  load: () => Promise<T | null>,
  read: () => Promise<unknown>,
  unread: (answer: T) => boolean = unreadByDefault,
): void {
  // The newest callbacks, read once at mount: the read belongs to the open,
  // not to every render its answer causes, and inline callers cannot loop it.
  const latest = useRef({ load, read, unread });
  latest.current = { load, read, unread };
  useEffect(() => {
    void (async () => {
      const { load, read, unread } = latest.current;
      const answer = await load();
      // A failed load already put its sentence on the page; a read on top of
      // it would turn one failure into two.
      if (answer === null || !unread(answer)) return;
      const existing = running.get(page);
      if (existing) {
        // Another open started this read: wait for it, then paint what it left
        // rather than sitting on the unread sentence it replaced.
        await existing.catch(() => {});
        await load();
        return;
      }
      const paid = (async () => {
        try {
          await read();
        } finally {
          // A refused read is still over: the next open may ask again.
          running.delete(page);
        }
      })();
      running.set(page, paid);
      await paid;
    })();
  }, [page]);
}
