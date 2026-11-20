// The roster pages (Ideas, Goals) show the same row shape, so they read their
// words from one place: the native page's grouping order and body clamp.
import type { ListingItem } from "./types";

export const SNIPPET_LIMIT = 240;

/// The row's group, in the native roster's order: the service's category
/// first, then its kind, then the kicker. `null` means it named none and the
/// row stands ungrouped.
export function groupOf(item: ListingItem): string | null {
  for (const value of [item.category, item.kind, item.kicker]) {
    if (value && value.length > 0) return value;
  }
  return null;
}

/// The row's second block: the body's first three non-empty lines joined,
/// capped at 240 characters, the same words the native row clamps.
export function snippet(body?: string | null): string {
  const text = (body ?? "")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .slice(0, 3)
    .join(" ");
  return text.length <= SNIPPET_LIMIT
    ? text
    : `${text.slice(0, SNIPPET_LIMIT)}…`;
}
