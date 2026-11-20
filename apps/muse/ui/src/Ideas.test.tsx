// S2's gate for the Ideas page: the page's behaviour against the same
// channel the app speaks, with no window and no service.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Ideas } from "./Ideas";
import { installChannel } from "./testChannel";
import type { IdeasSnapshot, ListingItem } from "./types";

const idea = (overrides: Partial<ListingItem>): ListingItem => ({
  title: "Untitled idea",
  kind: "idea",
  ...overrides,
});

const rows = (count: number, prefix: string, extra: Partial<ListingItem> = {}) =>
  Array.from({ length: count }, (_, index) =>
    idea({ id: `${prefix}-${index}`, title: `${prefix} ${index}`, ...extra }),
  );

const bootPayload = (ideas: ListingItem[], overrides: Partial<IdeasSnapshot> = {}) => ({
  page: "ideas" as const,
  ideas,
  total: ideas.length,
  loaded: true,
  error: null,
  ...overrides,
});

const cards = () => document.querySelectorAll(".idea").length;

describe("the Ideas web page", () => {
  it("opens on the held roster with its groups and icons", async () => {
    const held = bootPayload([
      idea({ id: "1", title: "A calmer inbox", kind: "Work", icon: "📥", body: "Digest\nonly" }),
      idea({ id: "2", title: "Weekly review", kind: "Work" }),
      idea({ id: "3", title: "Garden plan", kind: "Home" }),
    ]);
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Ideas />);

    expect(
      screen.getByRole("heading", { name: "Ideas", level: 1 }),
    ).toBeInTheDocument();
    expect(screen.getByText("3 ideas")).toBeInTheDocument();
    expect(screen.getByText("A calmer inbox")).toBeInTheDocument();
    expect(screen.getByText("Digest only")).toBeInTheDocument();
    // Group headers where the group changes, as the native roster paints.
    expect(screen.getAllByText("Work")).toHaveLength(1);
    expect(screen.getByText("Home")).toBeInTheDocument();
    // Rows without an icon fall back to the native page's default.
    expect(screen.getAllByText("💡")).toHaveLength(2);
    expect(cards()).toBe(3);
    expect(commands).toEqual(["ideas.snapshot"]);
  });

  it("names the not-loaded state instead of an empty Ideas page", async () => {
    const commands = installChannel(() => ({
      ok: true,
      ...bootPayload([], { loaded: false }),
    }));

    render(<Ideas />);

    expect(await screen.findByText(/has not loaded yet/)).toBeInTheDocument();
    expect(screen.queryByText("No ideas on this account yet.")).toBeNull();
    expect(screen.queryByText(/^\d+ ideas$/)).toBeNull();
    expect(cards()).toBe(0);
    expect(commands).toEqual(["ideas.snapshot"]);
  });

  it("refreshes over the wire and shows the listing that comes back", async () => {
    window.__MUSE_BOOT__ = bootPayload(rows(2, "Held"));
    const fresh = bootPayload(rows(3, "Fresh"), { total: 12 });
    const commands = installChannel((command) =>
      command === "ideas.refresh"
        ? { ok: true, ...fresh }
        : { ok: true, ...bootPayload(rows(2, "Held")) },
    );

    render(<Ideas />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByText("Fresh 2")).toBeInTheDocument();
    expect(screen.getByText("3 of 12 ideas")).toBeInTheDocument();
    expect(screen.queryByText("Held 0")).toBeNull();
    expect(commands).toEqual(["ideas.snapshot", "ideas.refresh"]);
  });

  it("reports a refused refresh and keeps the rows it has", async () => {
    window.__MUSE_BOOT__ = bootPayload(rows(2, "Held"));
    installChannel((command) =>
      command === "ideas.refresh"
        ? { ok: false, error: "no account link in this app" }
        : { ok: true, ...bootPayload(rows(2, "Held")) },
    );

    render(<Ideas />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(
      await screen.findByText("Command failed: no account link in this app"),
    ).toBeInTheDocument();
    expect(screen.getByText("2 ideas")).toBeInTheDocument();
    expect(cards()).toBe(2);
  });

  it("says an account read as empty rather than that it has not loaded", async () => {
    window.__MUSE_BOOT__ = bootPayload([]);
    installChannel(() => ({ ok: true, ...bootPayload([]) }));

    render(<Ideas />);

    await waitFor(() =>
      expect(screen.getByText("No ideas on this account yet.")).toBeInTheDocument(),
    );
    expect(screen.queryByText(/has not loaded yet/)).toBeNull();
    expect(screen.queryByText(/^\d+ ideas$/)).toBeNull();
    expect(cards()).toBe(0);
  });

  it("reports a failed read beside the rows the page still holds", async () => {
    window.__MUSE_BOOT__ = bootPayload(rows(1, "Held"));
    installChannel(() => ({
      ok: true,
      ...bootPayload(rows(1, "Held"), { error: "the host stopped answering" }),
    }));

    render(<Ideas />);

    expect(
      await screen.findByText("The last read failed: the host stopped answering"),
    ).toBeInTheDocument();
    expect(screen.getByText("Held 0")).toBeInTheDocument();
    expect(cards()).toBe(1);
  });
});
