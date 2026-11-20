// S2's gate for the Threads page: the page's behaviour against the same
// channel the app speaks, with no window and no service.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Threads } from "./Threads";
import { installChannel } from "./testChannel";
import type { ThreadRow, ThreadsSnapshot } from "./types";

const row = (overrides: Partial<ThreadRow> = {}): ThreadRow => ({
  id: "s-1",
  title: "A calmer inbox",
  meta: "digest only · 09:14",
  group: "Today",
  unread: 0,
  pinned: false,
  side: false,
  ...overrides,
});

const bootPayload = (overrides: Partial<ThreadsSnapshot> = {}) => ({
  page: "threads" as const,
  threads: [] as ThreadRow[],
  total: 0,
  loaded: true,
  error: null,
  ...overrides,
});

const rows = () => document.querySelectorAll(".thread").length;

describe("the Threads web page", () => {
  it("opens on the held rows under their day groups", async () => {
    const held = bootPayload({
      threads: [
        row({ id: "s-1", title: "A calmer inbox", group: "Today" }),
        row({ id: "s-2", title: "Monday sync", group: "Monday", meta: "" }),
        row({ id: "s-3", title: "Pinned plan", pinned: true, group: "Yesterday" }),
        row({ id: "s-4", title: "Side question", side: true, group: "Today" }),
      ],
      total: 4,
    });
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Threads />);

    expect(screen.getByRole("heading", { name: "Threads", level: 1 })).toBeInTheDocument();
    expect(screen.getByText("A calmer inbox")).toBeInTheDocument();
    expect(screen.getByText("Monday sync")).toBeInTheDocument();
    expect(screen.getByText("Pinned plan")).toBeInTheDocument();
    expect(screen.getByText("Side question")).toBeInTheDocument();
    // Today appears once for the main list; the side chat has its own header.
    expect(screen.getAllByText("Today")).toHaveLength(1);
    expect(screen.getByText("Monday")).toBeInTheDocument();
    expect(screen.getByText("Side chats")).toBeInTheDocument();
    expect(screen.getByText("4 threads")).toBeInTheDocument();
    expect(rows()).toBe(4);
    expect(commands).toEqual(["threads.snapshot"]);
  });

  it("shows the unread count and the listing's snippet with its clock time", async () => {
    const held = bootPayload({
      threads: [row({ unread: 3, meta: "latest activity · 11:02" })],
      total: 1,
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Threads />);

    expect(screen.getByTitle("3 unread")).toHaveTextContent("3");
    expect(screen.getByText("latest activity · 11:02")).toBeInTheDocument();
  });

  it("names the not-loaded state instead of an empty account", async () => {
    const commands = installChannel(() => ({ ok: true, ...bootPayload({ loaded: false }) }));

    render(<Threads />);

    expect(await screen.findByText(/has not loaded yet/)).toBeInTheDocument();
    expect(screen.queryByText("No threads yet.")).toBeNull();
    expect(commands).toEqual(["threads.snapshot"]);
  });

  it("refreshes over the wire and shows the listing that comes back", async () => {
    window.__MUSE_BOOT__ = bootPayload({
      threads: [row({ id: "held", title: "Held thread" })],
      total: 1,
    });
    const fresh = bootPayload({
      threads: [
        row({ id: "s-1", title: "Fresh thread" }),
        row({ id: "s-2", title: "Second fresh thread", group: "Yesterday" }),
      ],
      total: 9,
    });
    const commands = installChannel((command) =>
      command === "threads.refresh"
        ? { ok: true, ...fresh }
        : { ok: true, ...bootPayload({ threads: [row({ id: "held", title: "Held thread" })], total: 1 }) },
    );

    render(<Threads />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByText("Fresh thread")).toBeInTheDocument();
    expect(screen.getByText("Second fresh thread")).toBeInTheDocument();
    expect(screen.getByText("2 of 9 threads")).toBeInTheDocument();
    expect(screen.queryByText("Held thread")).toBeNull();
    expect(commands).toEqual(["threads.snapshot", "threads.refresh"]);
  });

  it("reports a refused refresh and keeps the rows it has", async () => {
    window.__MUSE_BOOT__ = bootPayload({
      threads: [row({ id: "held", title: "Held thread" })],
      total: 1,
    });
    installChannel((command) =>
      command === "threads.refresh"
        ? { ok: false, error: "no account link in this app" }
        : { ok: true, ...bootPayload({ threads: [row({ id: "held", title: "Held thread" })], total: 1 }) },
    );

    render(<Threads />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByText("Command failed: no account link in this app")).toBeInTheDocument();
    expect(screen.getByText("Held thread")).toBeInTheDocument();
    expect(rows()).toBe(1);
  });

  it("says an account read as empty rather than that it has not loaded", async () => {
    window.__MUSE_BOOT__ = bootPayload();
    installChannel(() => ({ ok: true, ...bootPayload() }));

    render(<Threads />);

    await waitFor(() => expect(screen.getByText("No threads yet.")).toBeInTheDocument());
    expect(screen.queryByText(/has not loaded yet/)).toBeNull();
    expect(rows()).toBe(0);
  });

  it("reports a failed read beside the rows the page still holds", async () => {
    window.__MUSE_BOOT__ = bootPayload({
      threads: [row({ id: "held", title: "Held thread" })],
      total: 1,
    });
    installChannel(() => ({
      ok: true,
      ...bootPayload({
        threads: [row({ id: "held", title: "Held thread" })],
        total: 1,
        error: "the host stopped answering",
      }),
    }));

    render(<Threads />);

    expect(
      await screen.findByText("The last read failed: the host stopped answering"),
    ).toBeInTheDocument();
    expect(screen.getByText("Held thread")).toBeInTheDocument();
  });
});
