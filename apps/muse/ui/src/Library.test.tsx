// The DOM tests S1's gate asks for: the Library page's behaviour, checked
// against the same channel the app speaks, with no window and no service.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Library } from "./Library";
import { installChannel } from "./testChannel";
import type { Artifact } from "./types";

const artifact = (overrides: Partial<Artifact>): Artifact => ({
  artifact_ref: "ref",
  title: "Untitled",
  pinned: false,
  can_open: true,
  can_share: false,
  target_kind: "file",
  updated_at_ms: Date.now(),
  ...overrides,
});

function rows(count: number, prefix: string, extra: Partial<Artifact> = {}): Artifact[] {
  return Array.from({ length: count }, (_, index) =>
    artifact({
      artifact_ref: `${prefix}-${index}`,
      title: `${prefix} ${index}`,
      ...extra,
    }),
  );
}

type Reply = Record<string, unknown>;

const cards = () => document.querySelectorAll(".card").length;

describe("the Library web page", () => {
  it("opens on the boot rows and names the service's total", async () => {
    window.__MUSE_BOOT__ = { artifacts: rows(50, "File"), total: 127 };
    const commands = installChannel(() => ({ ok: true, artifacts: rows(50, "File"), total: 127 }));

    render(<Library />);

    expect(screen.getByText("50 of 127 items")).toBeInTheDocument();
    expect(cards()).toBe(50);
    expect(await screen.findByText("File 49")).toBeInTheDocument();
    expect(commands).toEqual(["library.snapshot"]);
  });

  it("names the not-loaded state instead of showing an empty library", async () => {
    const commands = installChannel(() => ({ ok: true, artifacts: [], total: 0 }));

    render(<Library />);

    expect(await screen.findByText(/has not loaded yet/)).toBeInTheDocument();
    expect(screen.queryByText(/Nothing in the Library matches/)).toBeNull();
    // No count is shown at all: "0 of 0 items" would be a claim about the
    // service, and only the service's own total may make one.
    expect(screen.queryByText(/items/)).toBeNull();
    expect(cards()).toBe(0);
    expect(commands).toEqual(["library.snapshot"]);
  });

  it("refreshes over the wire and shows the listing that comes back", async () => {
    window.__MUSE_BOOT__ = { artifacts: rows(2, "Cached"), total: 9 };
    const commands = installChannel((command) =>
      command === "library.refresh"
        ? { ok: true, artifacts: rows(3, "Fresh"), total: 128 }
        : { ok: true, artifacts: rows(2, "Cached"), total: 9 },
    );

    render(<Library />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByText("Fresh 2")).toBeInTheDocument();
    expect(screen.getByText("3 of 128 items")).toBeInTheDocument();
    expect(screen.queryByText("Cached 0")).toBeNull();
    expect(cards()).toBe(3);
    // Snapshot on open, refresh on the button: the button is not a cache
    // re-read wearing a wire label.
    expect(commands).toEqual(["library.snapshot", "library.refresh"]);
  });

  it("reports a refused refresh as a failure and keeps the rows it has", async () => {
    window.__MUSE_BOOT__ = { artifacts: rows(2, "Held"), total: 9 };
    installChannel((command) =>
      command === "library.refresh"
        ? { ok: false, error: "no account link in this app" }
        : { ok: true, artifacts: rows(2, "Held"), total: 9 },
    );

    render(<Library />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(
      await screen.findByText("Command failed: no account link in this app"),
    ).toBeInTheDocument();
    expect(screen.getByText("2 of 9 items")).toBeInTheDocument();
    expect(cards()).toBe(2);
  });

  it("narrows by tab and search without another trip to the wire", async () => {
    const boot = [
      artifact({ artifact_ref: "s1", title: "Space alpha", target_kind: "space" }),
      artifact({ artifact_ref: "s2", title: "Space beta", target_kind: "space" }),
      artifact({ artifact_ref: "f1", title: "File gamma" }),
      artifact({ artifact_ref: "f2", title: "File delta" }),
    ];
    window.__MUSE_BOOT__ = { artifacts: boot, total: 4 };
    const commands = installChannel(() => ({ ok: true, artifacts: boot, total: 4 }));

    render(<Library />);
    await waitFor(() => expect(commands).toEqual(["library.snapshot"]));

    fireEvent.click(screen.getByRole("button", { name: "Spaces" }));
    expect(cards()).toBe(2);
    expect(screen.queryByText("File gamma")).toBeNull();

    fireEvent.change(screen.getByPlaceholderText("Quick Open"), {
      target: { value: "alpha" },
    });
    expect(cards()).toBe(1);
    expect(screen.getByText("Space alpha")).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText("Quick Open"), {
      target: { value: "delta" },
    });
    fireEvent.click(screen.getByRole("button", { name: "All" }));
    expect(cards()).toBe(1);
    expect(screen.getByText("File delta")).toBeInTheDocument();

    expect(screen.queryByText(/Nothing in the Library matches/)).toBeNull();
    expect(commands).toEqual(["library.snapshot"]);
  });

  // The one write on this page: a space's unshare, on the app's arm-then-
  // confirm rule. The first press sends nothing; the second sends the row's
  // handle, and the rows that come back are the listing read again.
  it("arms a space's unshare first, then sends it and paints the re-read rows", async () => {
    const space = artifact({
      artifact_ref: "space:calm-space",
      title: "Calm space",
      target_kind: "space",
      slug: "calm-space",
      can_share: true,
    });
    let unshares = 0;
    const seen: Record<string, unknown>[] = [];
    const commands = installChannel(
      (command) => {
        if (command === "library.snapshot") {
          return { ok: true, artifacts: [space], total: 1, armed: null };
        }
        unshares += 1;
        if (unshares === 1) {
          return {
            ok: true,
            accepted: false,
            slug: "calm-space",
            armed: "calm-space",
            artifacts: [space],
            total: 1,
          };
        }
        return {
          ok: true,
          accepted: true,
          slug: "calm-space",
          armed: null,
          artifacts: [
            space,
            artifact({ artifact_ref: "file:after", title: "Read again file" }),
          ],
          total: 2,
        };
      },
      (request) => seen.push(request),
    );

    render(<Library />);

    fireEvent.click(await screen.findByRole("button", { name: "Unshare" }));

    // The first press only arms: the app sent nothing, and the page says so.
    expect(await screen.findByText(/Nothing has been sent yet/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Confirm unshare" })).toBeInTheDocument();
    expect(unshares).toBe(1);

    fireEvent.click(screen.getByRole("button", { name: "Confirm unshare" }));

    expect(await screen.findByText(/listing was read again/)).toBeInTheDocument();
    // Rows from the answer, not the ones the page held: the re-read painted.
    expect(screen.getByText("Read again file")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Confirm unshare" })).toBeNull();
    expect(screen.getByRole("button", { name: "Unshare" })).toBeInTheDocument();
    expect(commands).toEqual([
      "library.snapshot",
      "library.unshare",
      "library.unshare",
    ]);
    // The row is addressed by its handle and nothing else: no ref, no path.
    const presses = seen.filter((request) => request.cmd === "library.unshare");
    expect(presses.map((request) => request.slug)).toEqual([
      "calm-space",
      "calm-space",
    ]);
    for (const press of presses) {
      expect(Object.keys(press).sort()).toEqual(["cmd", "id", "slug"]);
    }
  });

  // A refused press is not a listing. The unknown-handle answer carries no
  // rows, so the page must keep the ones it holds instead of painting an
  // empty answer over them.
  it("keeps the held rows when the app refuses an unshare", async () => {
    const space = artifact({
      artifact_ref: "space:held-space",
      title: "Held space",
      target_kind: "space",
      slug: "held-space",
    });
    let presses = 0;
    installChannel((command) => {
      if (command === "library.snapshot") {
        return { ok: true, artifacts: [space], total: 1, armed: null };
      }
      presses += 1;
      return { ok: false, error: "that space is not in the held Library listing" };
    });

    render(<Library />);

    fireEvent.click(await screen.findByRole("button", { name: "Unshare" }));

    expect(
      await screen.findByText(/did not take the press: that space is not in the held Library listing/),
    ).toBeInTheDocument();
    expect(screen.getByText("Held space")).toBeInTheDocument();
    expect(screen.queryByText(/has not loaded yet/)).toBeNull();
    expect(screen.queryByText(/Nothing in the Library matches/)).toBeNull();
    expect(presses).toBe(1);
  });

  // The page's second write: a space's delete, behind its own arm. The
  // unshare button stays "Unshare" throughout, so one write's arm never reads
  // as the other's, and the row it deleted is gone from the read that paints.
  it("arms a space's delete first, then sends it and paints the re-read rows", async () => {
    const space = artifact({
      artifact_ref: "space:calm-space",
      title: "Calm space",
      target_kind: "space",
      slug: "calm-space",
    });
    const replies = [
      {
        ok: true,
        accepted: false,
        slug: "calm-space",
        armed: null,
        delete_armed: "calm-space",
        artifacts: [space],
        total: 1,
      },
      {
        ok: true,
        accepted: true,
        slug: "calm-space",
        armed: null,
        delete_armed: null,
        artifacts: [],
        total: 0,
      },
    ];
    let presses = 0;
    const seen: Record<string, unknown>[] = [];
    const commands = installChannel(
      (command) => {
        if (command === "library.snapshot") {
          return {
            ok: true,
            artifacts: [space],
            total: 1,
            armed: null,
            delete_armed: null,
          };
        }
        return replies[presses++];
      },
      (request) => seen.push(request),
    );

    render(<Library />);

    fireEvent.click(await screen.findByRole("button", { name: "Delete space" }));

    // The first press only arms it, and says what the second one will do.
    expect(
      await screen.findByText(/Nothing has been sent yet: press Confirm delete/),
    ).toBeInTheDocument();
    expect(screen.getByText(/Deleting a space is permanent/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Confirm delete" })).toBeInTheDocument();
    // The other write's row is untouched by this arm.
    expect(screen.getByRole("button", { name: "Unshare" })).toBeInTheDocument();
    expect(presses).toBe(1);

    fireEvent.click(screen.getByRole("button", { name: "Confirm delete" }));

    expect(
      await screen.findByText(/accepted the delete, and the listing was read again/),
    ).toBeInTheDocument();
    // The rows are the answer's read, and the deleted row is gone from them.
    await waitFor(() => expect(screen.queryByText("Calm space")).toBeNull());
    expect(screen.queryByRole("button", { name: "Confirm delete" })).toBeNull();
    expect(commands).toEqual([
      "library.snapshot",
      "library.delete-space",
      "library.delete-space",
    ]);
    // The row is addressed by its handle and nothing else: no ref, no path.
    const sent = seen.filter((request) => request.cmd === "library.delete-space");
    expect(sent.map((request) => request.slug)).toEqual([
      "calm-space",
      "calm-space",
    ]);
    for (const press of sent) {
      expect(Object.keys(press).sort()).toEqual(["cmd", "id", "slug"]);
    }
  });

  it("offers no unshare on a file row", async () => {
    installChannel(() => ({
      ok: true,
      artifacts: [artifact({ title: "Plan.pdf", format: "pdf" })],
      total: 1,
      armed: null,
    }));

    render(<Library />);

    expect(await screen.findByText("Plan.pdf")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Unshare" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Delete space" })).toBeNull();
  });

  it("says a filter matched nothing rather than that the library is empty", async () => {
    window.__MUSE_BOOT__ = { artifacts: rows(2, "Held"), total: 2 };
    installChannel(() => ({ ok: true, artifacts: rows(2, "Held"), total: 2 }));

    render(<Library />);
    fireEvent.change(screen.getByPlaceholderText("Quick Open"), {
      target: { value: "nothing-matches-this" },
    });

    expect(await screen.findByText(/Nothing in the Library matches/)).toBeInTheDocument();
    expect(screen.queryByText(/has not loaded yet/)).toBeNull();
  });
});
