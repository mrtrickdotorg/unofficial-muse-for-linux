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

  // The cards come from the approvals read, not from the Tasks listing, so an
  // open that finds them unread asks for that read too -- and only that half,
  // because the schedule it names was already held.
  it("asks the approvals for a read when none has been painted", async () => {
    let snapshots = 0;
    const commands = installChannel((command) => {
      if (command === "chat.approvals.refresh") return { ok: true };
      snapshots += 1;
      return {
        ok: true,
        ...payload(
          snapshots === 1
            ? { approvals: { loaded: false, cards: [], note: null } }
            : { approvals: { loaded: true, cards: [waiting], note: null } },
        ),
      };
    });

    render(<Upcoming />);

    expect(await screen.findByText("Write /tmp/notes.txt")).toBeInTheDocument();
    expect(commands).toEqual([
      "upcoming.snapshot",
      "chat.approvals.refresh",
      "upcoming.snapshot",
    ]);
  });

  // The approvals route answers before its read lands, so the sentence has to
  // outlive that answer: the page keeps saying a read is in progress until the
  // cards are in its own snapshot, and never falls back to "nothing read".
  it("keeps saying a read is in progress until the approvals land", async () => {
    vi.useFakeTimers();
    try {
      let snapshots = 0;
      installChannel(
        (command) => {
          if (command === "chat.approvals.refresh") return { ok: true };
          snapshots += 1;
          return {
            ok: true,
            ...payload(
              snapshots === 1
                ? { approvals: { loaded: false, cards: [], note: null } }
                : { approvals: { loaded: true, cards: [waiting], note: null } },
            ),
          };
        },
        undefined,
        (command) => (command === "upcoming.snapshot" ? 500 : 0),
      );

      render(<Upcoming />);
      // The opening snapshot, then the approvals route's own answer: after
      // both the read is still running, and only the snapshot can end it.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(600);
      });
      // The opening snapshot has answered unread and the approvals route has
      // answered too; the cards are not in the page yet.
      expect(
        screen.getByText(/Reading the approvals now/),
      ).toBeInTheDocument();
      expect(screen.queryByText(/not been read yet/)).not.toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(600);
      });
      expect(screen.getByText("Write /tmp/notes.txt")).toBeInTheDocument();
      expect(screen.queryByText(/Reading the approvals now/)).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  // The reads land and leave the listing unread while the approvals come back
  // empty: the page names the unread half instead of reading as an empty
  // account, and "no pending approvals" only once that list has been read.
  it("separates an unread listing from an empty account", async () => {
    let snapshots = 0;
    installChannel((command) => {
      if (command === "tasks.refresh") {
        return { ok: true, schedules: [], runs: [], local: [], loaded: true };
      }
      if (command === "chat.approvals.refresh") return { ok: true };
      snapshots += 1;
      return {
        ok: true,
        ...payload(
          snapshots === 1
            ? {
                schedules_loaded: false,
                approvals: { cards: [], loaded: false, note: null },
              }
            : { schedules_loaded: false },
        ),
      };
    });

    render(<Upcoming />);

    expect(
      await screen.findByText(/not been read yet/),
    ).toBeInTheDocument();
    expect(screen.getByText("No pending approvals.")).toBeInTheDocument();
    expect(screen.queryByText("No run scheduled.")).not.toBeInTheDocument();
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
    // The read runs first, and its answer says the listing is still unread.
    expect(
      await screen.findByText(/Reading the schedule listing now/),
    ).toBeInTheDocument();
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
