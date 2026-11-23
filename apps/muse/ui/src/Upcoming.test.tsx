// P5's gate for the Upcoming page: the next run and the waiting approvals
// against the same channel the app speaks, with no window and no service.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Upcoming } from "./Upcoming";
import { installChannel } from "./testChannel";
import type { UpcomingSnapshot } from "./types";

const payload = (overrides: Partial<UpcomingSnapshot> = {}): UpcomingSnapshot => ({
  next_run: null,
  schedules_loaded: true,
  approvals: { cards: [], loaded: true, note: null },
  ...overrides,
});

describe("the Upcoming web page", () => {
  it("names the next run and the approvals waiting on the reader", async () => {
    const held = payload({
      next_run: { title: "Nightly build", when: "in 2h" },
      approvals: {
        loaded: true,
        cards: [
          {
            id: "approval-1",
            headline: "Write /tmp/notes.txt",
            summary: "12 bytes",
            details: [],
            choices: [{ word: "allow_once", label: "Allow once" }],
          },
        ],
        note: null,
      },
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
});
