import { useCallback, useMemo, useState } from "react";
import { call, channelAvailable } from "./api";
import { useHeldRead } from "./held";
import type { Artifact, LibrarySnapshot } from "./types";

const TABS = ["All", "Spaces", "Files"] as const;
type Tab = (typeof TABS)[number];

const isSpace = (a: Artifact) => a.target_kind === "space";

function relative(ms?: number | null): string {
  if (!ms) return "";
  const days = Math.floor((Date.now() - ms) / 86_400_000);
  if (days <= 0) return "today";
  if (days === 1) return "yesterday";
  if (days < 30) return `${days} days ago`;
  const months = Math.floor(days / 30);
  if (months < 12) return `${months} ${months === 1 ? "month" : "months"} ago`;
  const years = Math.floor(months / 12);
  return `${years} ${years === 1 ? "year" : "years"} ago`;
}

type SpaceWrite = "unshare" | "delete";

function Card({
  artifact,
  armed,
  deleteArmed,
  pressing,
  onUnshare,
  onDelete,
}: {
  artifact: Artifact;
  armed: boolean;
  deleteArmed: boolean;
  pressing: SpaceWrite | null;
  onUnshare: (artifact: Artifact) => void;
  onDelete: (artifact: Artifact) => void;
}) {
  const label = isSpace(artifact)
    ? "space"
    : artifact.format || artifact.kind || "file";
  return (
    <div className="card">
      <h3>{artifact.title}</h3>
      <div className="meta">
        {isSpace(artifact) && <span className="badge space">{label}</span>}
        {artifact.pinned && <span className="badge">pinned</span>}
        <span>{relative(artifact.updated_at_ms)}</span>
      </div>
      {/* The page's writes: a space's unshare and its delete, each addressed by
          the row's own handle and each behind its own arm. The app arms them,
          so the labels here are the app's arms and cannot disagree with the
          native row. */}
      {isSpace(artifact) && artifact.slug && (
        <div className="card-actions">
          <button
            type="button"
            className={`inline-button danger${armed ? " armed" : ""}`}
            disabled={pressing !== null}
            onClick={() => onUnshare(artifact)}
            title={
              armed
                ? "Press again to unshare this space"
                : "Unsharing makes the space private"
            }
          >
            {pressing === "unshare"
              ? "Unsharing…"
              : armed
                ? "Confirm unshare"
                : "Unshare"}
          </button>
          <button
            type="button"
            className={`inline-button danger${deleteArmed ? " armed" : ""}`}
            disabled={pressing !== null}
            onClick={() => onDelete(artifact)}
            title={
              deleteArmed
                ? "Press again to delete this space"
                : "Deleting the space is permanent"
            }
          >
            {pressing === "delete"
              ? "Deleting…"
              : deleteArmed
                ? "Confirm delete"
                : "Delete space"}
          </button>
        </div>
      )}
    </div>
  );
}

export function Library() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<LibrarySnapshot | null>(
    boot && "artifacts" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tab, setTab] = useState<Tab>("All");
  const [query, setQuery] = useState("");
  // What the app's last unshare answer said: armed, accepted, refused or
  // unproven. The page keeps no arm of its own.
  const [note, setNote] = useState<string | null>(null);
  const [pressing, setPressing] = useState<{ slug: string; write: SpaceWrite } | null>(
    null,
  );

  // Opening the page costs nothing on the wire: the native side answers
  // from the rows it already holds, which is the same list the boot payload
  // came from.
  const loadSnapshot = useCallback(async (): Promise<LibrarySnapshot | null> => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return null;
    }
    try {
      const answer = await call<LibrarySnapshot>("library.snapshot");
      setData(answer);
      setError(null);
      return answer;
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      return null;
    }
  }, []);

  // Refresh reads the service again, which is what the label promises. A
  // listing has taken 27 s against this service, so the wait is generous
  // rather than a premature "timed out" over rows that are about to arrive.
  const refresh = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    setBusy(true);
    try {
      setData(await call<LibrarySnapshot>("library.refresh", 120_000));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  const reading = useHeldRead("library", loadSnapshot, refresh);

  // A space write, on the app's one-armed-press rule: the first press sends
  // nothing, the second press on the same row and write sends the app's own
  // path-only DELETE, and the rows that come back are the listing read again.
  // Each write has its own arm in the app, so an armed unshare never stands
  // for a delete.
  const spaceWrite = useCallback(
    async (artifact: Artifact, write: SpaceWrite) => {
      const slug = artifact.slug;
      if (!slug || pressing) return;
      if (!channelAvailable()) {
        setError("The app's command channel is not available in this window.");
        return;
      }
      const route =
        write === "unshare" ? "library.unshare" : "library.delete-space";
      const noun = write === "unshare" ? "unshare" : "delete";
      setPressing({ slug, write });
      try {
        const answer = await call<
          LibrarySnapshot & {
            ok?: boolean;
            accepted?: boolean;
            error?: string;
            reread_error?: string;
          }
        >(route, 120_000, { slug });
        // A refusal is not a listing: an unknown handle answers without rows
        // (and the channel rejects an `ok: false` reply before it gets here),
        // so the held ones stand and only the sentence changes.
        if (answer.ok === false) {
          setNote(
            `The app did not take the press: ${answer.error ?? "refused"}. The listing was left as it was.`,
          );
          return;
        }
        setData({
          artifacts: answer.artifacts ?? [],
          total: answer.total ?? 0,
          armed: answer.armed ?? null,
          delete_armed: answer.delete_armed ?? null,
        });
        if (answer.delete_armed) {
          setNote(
            "Nothing has been sent yet: press Confirm delete to send it. Deleting a space is permanent.",
          );
        } else if (answer.armed) {
          setNote("Nothing has been sent yet: press Confirm unshare to send it.");
        } else if (answer.accepted && answer.reread_error) {
          setNote(
            `The service accepted the ${noun}, but the listing could not be read again: ${answer.reread_error}`,
          );
        } else if (answer.accepted) {
          setNote(
            `The service accepted the ${noun}, and the listing was read again.`,
          );
        } else {
          setNote(
            `The service did not ${noun} it. Nothing changed.${answer.error ? ` (${answer.error})` : ""}`,
          );
        }
        setError(null);
      } catch (failure) {
        // A refused press answers `ok: false`, which the channel rejects: the
        // rows must not move for it, and the refusal is shown in the app's own
        // words instead of a blank listing or a generic banner.
        setNote(
          `The app did not take the press: ${
            failure instanceof Error ? failure.message : String(failure)
          }. The listing was left as it was.`,
        );
      } finally {
        setPressing(null);
      }
    },
    [pressing],
  );

  const rows = useMemo(() => {
    const all = data?.artifacts ?? [];
    const needle = query.trim().toLowerCase();
    return all.filter((artifact) => {
      if (needle && !artifact.title.toLowerCase().includes(needle)) return false;
      if (tab === "Spaces") return isSpace(artifact);
      if (tab === "Files") return !isSpace(artifact);
      return true;
    });
  }, [data, query, tab]);

  // The native probe reads this: how many rows the page actually holds.
  (window as unknown as { __MUSE_ROWS__?: number }).__MUSE_ROWS__ =
    (data?.artifacts ?? []).length;

  const pinned = rows.filter((a) => a.pinned);
  const rest = rows.filter((a) => !a.pinned);
  const total = data?.total ?? 0;
  const loaded = (data?.artifacts ?? []).length > 0;

  return (
    <div className="page">
      <div className="top">
        <h1 className="title">Library</h1>
        <span className="count">
          {loaded && total > 0
            ? `${data!.artifacts.length} of ${total} items`
            : loaded
              ? `${data!.artifacts.length} items`
              : ""}
        </span>
        <button className="refresh" onClick={() => void refresh()} disabled={busy}>
          {busy ? "Refreshing…" : "Refresh"}
        </button>
      </div>

      <input
        className="search"
        placeholder="Quick Open"
        autoComplete="off"
        value={query}
        onChange={(event) => setQuery(event.target.value)}
      />

      <div className="tabs">
        {TABS.map((name) => (
          <button
            key={name}
            className={`tab${tab === name ? " active" : ""}`}
            onClick={() => setTab(name)}
          >
            {name}
          </button>
        ))}
      </div>

      {error && <div className="banner">Command failed: {error}</div>}
      {note && <div className="note write-note">{note}</div>}

      {!loaded ? (
        <div className="empty">
          {reading
            ? "Reading your Library now. It will appear here when the answer lands."
            : "The native Library page has not loaded yet, so there is nothing to show here. Open Library in the app, then press Refresh — this window is not claiming your Library is empty."}
        </div>
      ) : rows.length === 0 ? (
        <div className="empty">Nothing in the Library matches this filter.</div>
      ) : (
        <>
          {pinned.length > 0 && (
            <>
              <div className="section">Pinned</div>
              <div className="grid">
                {pinned.map((a) => (
                  <Card
                    key={a.artifact_ref}
                    artifact={a}
                    armed={data?.armed === a.slug}
                    deleteArmed={data?.delete_armed === a.slug}
                    pressing={
                      pressing && pressing.slug === a.slug
                        ? pressing.write
                        : null
                    }
                    onUnshare={(artifact) => void spaceWrite(artifact, "unshare")}
                    onDelete={(artifact) => void spaceWrite(artifact, "delete")}
                  />
                ))}
              </div>
            </>
          )}
          {rest.length > 0 && (
            <>
              <div className="section">All</div>
              <div className="grid">
                {rest.map((a) => (
                  <Card
                    key={a.artifact_ref}
                    artifact={a}
                    armed={data?.armed === a.slug}
                    deleteArmed={data?.delete_armed === a.slug}
                    pressing={
                      pressing && pressing.slug === a.slug
                        ? pressing.write
                        : null
                    }
                    onUnshare={(artifact) => void spaceWrite(artifact, "unshare")}
                    onDelete={(artifact) => void spaceWrite(artifact, "delete")}
                  />
                ))}
              </div>
            </>
          )}
        </>
      )}

      <div className="foot">
        Web UI slice of the port: rendered from the same rows the native page
        holds. Unsharing and deleting a space are the writes here, each the
        app's own path-only DELETE behind its own armed press.
      </div>
    </div>
  );
}
