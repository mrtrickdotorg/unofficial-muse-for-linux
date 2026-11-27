import { useCallback, useEffect, useState } from "react";
import { call } from "./api";
import type { FolderListing } from "./types";

function sentence(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

/// The app's folder read, one level at a time: the folder it stands in, the one
/// above it, and its subfolders. Nothing here browses files -- the app needs a
/// folder, and the read is the app's own (`folders.list`), so the sheet cannot
/// offer a path the app could not read.
///
/// A sheet rather than a page: the window keeps the page it was on, and the
/// choice comes back to it. Both places that need a folder (the session folder
/// on sign-in and a grant on Local host) use this one surface.
///
/// The sheet can also make the folder it stands in one deeper: the native
/// chooser this replaced had a Create Folder press, and without one a person
/// whose folder does not exist yet cannot get a session folder from inside the
/// app. The press asks the app (`folders.create`), which owns the rules, and
/// steps into what it made so the same list shows it.
export function FolderSheet({
  title,
  start,
  confirm,
  note,
  onChoose,
  onCancel,
}: {
  title: string;
  start: string;
  confirm: string;
  note?: string;
  onChoose: (path: string) => void;
  onCancel: () => void;
}) {
  const [listing, setListing] = useState<FolderListing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [name, setName] = useState("");
  const [creating, setCreating] = useState(false);

  const load = useCallback(async (path: string) => {
    setLoading(true);
    try {
      const held = await call<FolderListing>("folders.list", 15_000, { path });
      setListing(held);
      setError(null);
    } catch (failure) {
      setError(sentence(failure));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(start);
  }, [load, start]);

  const folders = listing?.folders ?? [];
  const here = listing?.path ?? start;

  const create = useCallback(async () => {
    const wanted = name.trim();
    if (wanted === "") return;
    setCreating(true);
    try {
      const made = await call<{ path: string }>("folders.create", 15_000, {
        path: here,
        name: wanted,
      });
      setName("");
      await load(made.path);
    } catch (failure) {
      setError(sentence(failure));
    } finally {
      setCreating(false);
    }
  }, [here, load, name]);

  return (
    <div className="sheet-backdrop">
      <div
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={(event) => {
          if (event.key === "Escape") onCancel();
        }}
      >
        <h2 className="sheet-title">{title}</h2>
        <p className="sheet-path" title={here}>
          {here || "The app's own folder"}
        </p>
        {note && <p className="note">{note}</p>}
        {error && <div className="banner">Command failed: {error}</div>}
        {listing?.note && <p className="note">{listing.note}</p>}

        <div className="sheet-new">
          <input
            type="text"
            className="sheet-new-name"
            aria-label="New folder name"
            placeholder="New folder name"
            spellCheck={false}
            value={name}
            disabled={creating}
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void create();
            }}
          />
          <button
            type="button"
            className="inline-button"
            disabled={creating || name.trim() === ""}
            onClick={() => void create()}
          >
            {creating ? "Creating…" : "Create folder"}
          </button>
        </div>

        <div className="sheet-list">
          {listing?.parent && (
            <button
              type="button"
              className="sheet-row sheet-up"
              disabled={loading}
              onClick={() => void load(listing.parent ?? "")}
            >
              ↑ Parent folder
            </button>
          )}
          {folders.map((folder) => (
            <button
              key={folder.path}
              type="button"
              className="sheet-row"
              disabled={loading}
              onClick={() => void load(folder.path)}
            >
              {folder.name}
            </button>
          ))}
          {listing && folders.length === 0 && (
            <p className="note">No folders inside this one.</p>
          )}
        </div>

        <div className="host-actions">
          <button
            type="button"
            className="inline-button"
            disabled={loading || listing === null}
            onClick={() => listing && onChoose(listing.path)}
          >
            {confirm}
          </button>
          <button type="button" className="inline-button" onClick={onCancel}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
