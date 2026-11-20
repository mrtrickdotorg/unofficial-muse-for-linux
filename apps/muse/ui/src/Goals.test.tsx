// S2's gate for the Goals page: the page's behaviour against the same
// channel the app speaks, with no window and no service.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Goals } from "./Goals";
import { installChannel } from "./testChannel";
import type { GoalsSnapshot, ListingItem } from "./types";

const goal = (overrides: Partial<ListingItem>): ListingItem => ({
  title: "Untitled goal",
  kind: "in_progress",
  ...overrides,
});

const rows = (count: number, prefix: string, extra: Partial<ListingItem> = {}) =>
  Array.from({ length: count }, (_, index) =>
    goal({ id: `${prefix}-${index}`, title: `${prefix} ${index}`, ...extra }),
  );

const bootPayload = (
  goals: ListingItem[],
  tracking: ListingItem[],
  overrides: Partial<GoalsSnapshot> = {},
) => ({
  page: "goals" as const,
  goals,
  goals_total: goals.length,
  tracking,
  tracking_total: tracking.length,
  tracking_error: null,
  ...overrides,
});

const cards = () => document.querySelectorAll(".goal").length;

describe("the Goals web page", () => {
  it("opens on the held roster and splits Tracking from it", async () => {
    const roster = [
      goal({ id: "g1", title: "Ship the port", kind: "completed" }),
      goal({ id: "g2", title: "Write the slice" }),
      goal({ id: "g3", title: "Review the slice" }),
    ];
    const tracked = [goal({ id: "t1", title: "Agent task alpha" })];
    window.__MUSE_BOOT__ = bootPayload(roster, tracked);
    const commands = installChannel(() => ({
      ok: true,
      ...bootPayload(roster, tracked),
    }));

    render(<Goals />);

    expect(
      screen.getByRole("heading", { name: "Goals", level: 1 }),
    ).toBeInTheDocument();
    expect(screen.getByText("3 goals")).toBeInTheDocument();
    expect(screen.getByText("Ship the port")).toBeInTheDocument();
    expect(screen.getByText("Agent task alpha")).toBeInTheDocument();
    // The native row's checkbox reflects the service's own status and is
    // never the mutation control.
    expect(screen.getByLabelText("Ship the port is complete")).toBeChecked();
    expect(screen.getByLabelText("Ship the port is complete")).toBeDisabled();
    expect(screen.getByLabelText("Write the slice is not complete")).not.toBeChecked();
    expect(cards()).toBe(4);
    expect(commands).toEqual(["goals.snapshot"]);
  });

  it("names the not-loaded state instead of an empty page", async () => {
    const commands = installChannel(() => ({
      ok: true,
      ...bootPayload([], []),
    }));

    render(<Goals />);

    expect(await screen.findByText(/has not loaded yet/)).toBeInTheDocument();
    expect(screen.queryByText("No goals on this account yet.")).toBeNull();
    // No count at all: "0 goals" would be a claim about the service.
    expect(screen.queryByText(/^\d+ goals$/)).toBeNull();
    expect(cards()).toBe(0);
    expect(commands).toEqual(["goals.snapshot"]);
  });

  it("refreshes both sections over the wire and shows what came back", async () => {
    window.__MUSE_BOOT__ = bootPayload(rows(2, "Held"), []);
    const fresh = bootPayload(rows(3, "Fresh"), [
      goal({ id: "t9", title: "Fresh tracked" }),
    ], { goals_total: 40 });
    const commands = installChannel((command) =>
      command === "goals.refresh"
        ? { ok: true, ...fresh }
        : { ok: true, ...bootPayload(rows(2, "Held"), []) },
    );

    render(<Goals />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByText("Fresh 2")).toBeInTheDocument();
    expect(screen.getByText("3 of 40 goals")).toBeInTheDocument();
    expect(screen.getByText("Fresh tracked")).toBeInTheDocument();
    expect(screen.queryByText("Held 0")).toBeNull();
    // Snapshot on open, a real service read on the button.
    expect(commands).toEqual(["goals.snapshot", "goals.refresh"]);
  });

  it("reports a refused refresh and keeps the rows it has", async () => {
    window.__MUSE_BOOT__ = bootPayload(rows(2, "Held"), []);
    installChannel((command) =>
      command === "goals.refresh"
        ? { ok: false, error: "no account link in this app" }
        : { ok: true, ...bootPayload(rows(2, "Held"), []) },
    );

    render(<Goals />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(
      await screen.findByText("Command failed: no account link in this app"),
    ).toBeInTheDocument();
    expect(screen.getByText("2 goals")).toBeInTheDocument();
    expect(cards()).toBe(2);
  });

  it("filters by category and expands Tracking without another trip to the wire", async () => {
    const roster = [
      goal({ id: "w1", title: "Work alpha", category: "Work" }),
      goal({ id: "w2", title: "Work beta", category: "Work" }),
      goal({ id: "h1", title: "Home alpha", category: "Home" }),
      goal({ id: "h2", title: "Home beta", category: "Home" }),
    ];
    const tracked = rows(12, "Tracked", { kind: "tracking" });
    const boot = bootPayload(roster, tracked);
    window.__MUSE_BOOT__ = boot;
    const commands = installChannel(() => ({ ok: true, ...boot }));

    render(<Goals />);
    await waitFor(() => expect(commands).toEqual(["goals.snapshot"]));

    // The Tracking section paints one screenful first, then expands in
    // place: the rows were already in hand.
    expect(screen.getByText("Tracked 9")).toBeInTheDocument();
    expect(screen.queryByText("Tracked 11")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show 2 more" }));
    expect(screen.getByText("Tracked 11")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Show \d+ more/ })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Work" }));
    expect(screen.getByText("Work alpha")).toBeInTheDocument();
    expect(screen.queryByText("Home alpha")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "All" }));
    expect(screen.getByText("Home beta")).toBeInTheDocument();
    expect(screen.queryByText(/No goals match/)).toBeNull();
    expect(commands).toEqual(["goals.snapshot"]);
  });

  it("says a failed Tracking read could not be read instead of empty", async () => {
    const roster = [goal({ id: "g1", title: "Kept goal" })];
    window.__MUSE_BOOT__ = bootPayload(roster, []);
    installChannel(() => ({
      ok: true,
      ...bootPayload(roster, [], { tracking_error: "tracking read timed out" }),
    }));

    render(<Goals />);

    expect(
      await screen.findByText("Tracking failed: tracking read timed out"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/The Tracking section could not be read/),
    ).toBeInTheDocument();
    expect(screen.queryByText("Nothing is being tracked right now.")).toBeNull();
    expect(screen.getByText("Kept goal")).toBeInTheDocument();
  });
});
