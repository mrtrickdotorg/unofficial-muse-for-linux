// P5's gate for the Upcoming page: the next run and the waiting approvals
// against the same channel the app speaks, with no window and no service.
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { PageNav } from "./navigation";
import { Upcoming } from "./Upcoming";
import { installChannel } from "./testChannel";
import type { UpcomingSnapshot } from "./types";

const payload = (overrides: Partial<UpcomingSnapshot> = {}): UpcomingSnapshot => ({
  next_run: null,
  schedules_loaded: true,
  approvals: { cards: [], loaded: true, note: null },
  ...overrides,
});

const waiting = {
  id: "approval-1",
  headline: "Write /tmp/notes.txt",
  summary: "12 bytes",
  details: [],
  choices: [{ word: "allow_once", label: "Allow once" }],
};

describe("the Upcoming web page", () => {
  it("names the next run and the approvals waiting on the reader", async () => {
    const held = payload({
      next_run: { title: "Nightly build", when: "in 2h" },
      approvals: { loaded: true, cards: [waiting], note: null },
    });
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Upcoming />);

    expect(
      screen.getByRole("heading", { name: "Upcoming", level: 1 }),
    ).toBeInTheDocument();
    expect(await screen.findByText("Nightly build")).toBeInTheDocument();
    expect(screen.getByText("in 2h")).toBeInTheDocument();
    expect(screen.getByText("Write /tmp/notes.txt")).toBeInTheDocument();
    expect(screen.getByText("12 bytes")).toBeInTheDocument();
    // One read on mount, no timer behind it.
    expect(commands).toEqual(["upcoming.snapshot"]);
  });

  // The run this page names lives in the Tasks listing. With no native paint
  // behind the shell, the page asks for that read and then for its own
  // snapshot again -- one read, in that order.
  it("asks the Tasks listing for a read when nothing has been painted", async () => {
    let snapshots = 0;
    const commands = installChannel((command) => {
      if (command === "tasks.refresh") {
        return { ok: true, schedules: [], runs: [], local: [], loaded: true };
      }
      snapshots += 1;
      return {
        ok: true,
        ...(snapshots === 1
          ? payload({ schedules_loaded: false })
          : payload({ next_run: { title: "Nightly build", when: "in 2h" } })),
      };
    });

    render(<Upcoming />);

    expect(await screen.findByText("Nightly build")).toBeInTheDocument();
    expect(commands).toEqual([
      "upcoming.snapshot",
      "tasks.refresh",
      "upcoming.snapshot",
    ]);
  });

  it("separates an unread listing from an empty account", async () => {
    installChannel(() => ({
      ok: true,
      ...payload({
        schedules_loaded: false,
        approvals: { cards: [], loaded: false, note: "Link your account first." },
      }),
    }));

    render(<Upcoming />);

    expect(await screen.findByText(/not been read yet/)).toBeInTheDocument();
    expect(screen.getByText("Link your account first.")).toBeInTheDocument();
    expect(screen.queryByText("No run scheduled.")).not.toBeInTheDocument();
    expect(screen.queryByText("No pending approvals.")).not.toBeInTheDocument();
  });

  it("says plainly when the account has nothing ahead", async () => {
    installChannel(() => ({ ok: true, ...payload() }));

    render(<Upcoming />);

    expect(await screen.findByText("No run scheduled.")).toBeInTheDocument();
    expect(screen.getByText("No pending approvals.")).toBeInTheDocument();
  });

  // The app nudges the page when a Tasks read lands, instead of pushing rows:
  // the page asks its own snapshot again, so the "not read yet" sentence it
  // was showing is replaced by the read that just landed.
  it("re-reads its held state when a Tasks read lands", async () => {
    let read = false;
    const commands = installChannel(() => ({
      ok: true,
      ...payload(read ? { next_run: { title: "Nightly build", when: "in 2h" } } : {}),
      schedules_loaded: read,
    }));

    render(<Upcoming />);
    expect(await screen.findByText(/not been read yet/)).toBeInTheDocument();

    read = true;
    act(() => window.museUpcomingReady?.());

    expect(await screen.findByText("Nightly build")).toBeInTheDocument();
    // The opening snapshot says the schedules were never read, so the page
    // asks for that read and then for its own snapshot again; the nudge adds
    // one more ask.
    await waitFor(() =>
      expect(commands).toEqual([
        "upcoming.snapshot",
        "tasks.refresh",
        "upcoming.snapshot",
        "upcoming.snapshot",
      ]),
    );
  });

  // A waiting approval is named here and decided in Chat, so the row hands the
  // reader to Chat's card and sends no command of its own.
  it("takes the reader to Chat from a waiting approval without deciding", async () => {
    const commands = installChannel(() => ({
      ok: true,
      ...payload({ approvals: { loaded: true, cards: [waiting], note: null } }),
    }));
    const onNavigate = vi.fn();

    render(
      <PageNav.Provider value={onNavigate}>
        <Upcoming />
      </PageNav.Provider>,
    );

    const row = await screen.findByRole("button", {
      name: /Write \/tmp\/notes\.txt — open in Chat/,
    });
    fireEvent.click(row);

    expect(onNavigate).toHaveBeenCalledWith("chat");
    expect(commands).toEqual(["upcoming.snapshot"]);
  });
});
