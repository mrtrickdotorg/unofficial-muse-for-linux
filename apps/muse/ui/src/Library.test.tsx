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
