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
  // The shell can be the only surface: with nothing painted into the app's
  // held rows, the page asks for the listing itself rather than presenting an
  // account with no conversations.
  it("reads the listing itself when the app says none was painted", async () => {
    const commands = installChannel((command) =>
      command === "threads.refresh"
        ? {
            ok: true,
            ...bootPayload({ threads: [row({ title: "Read now" })], total: 1 }),
          }
        : { ok: true, ...bootPayload({ loaded: false }) },
    );

    render(<Threads />);

    expect(await screen.findByText("Read now")).toBeInTheDocument();
    expect(commands).toEqual(["threads.snapshot", "threads.refresh"]);
  });

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
    // An unread listing is a read owed: the page asks for it once itself, and
    // this channel answers "still unread", so the sentence stands.
    expect(commands).toEqual(["threads.snapshot", "threads.refresh"]);
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

  // The mac app's row menu, on this page: a pin is one `threads.op` and the
  // listing read that comes back is what the page paints.
  it("pins a conversation through the app and paints the listing it reads back", async () => {
    const held = bootPayload({ threads: [row({ id: "s-1", title: "A calmer inbox" })], total: 1 });
    window.__MUSE_BOOT__ = held;
    const asked: Array<Record<string, unknown>> = [];
    const commands = installChannel(
      (command) => {
        if (command === "threads.op") {
          return {
            ok: true,
            accepted: true,
            ...bootPayload({
              threads: [row({ id: "s-1", title: "A calmer inbox", pinned: true })],
              total: 1,
            }),
          };
        }
        return { ok: true, ...held };
      },
      (request) => {
        if (request.cmd === "threads.op") asked.push(request);
      },
    );

    render(<Threads />);
    fireEvent.click(screen.getByRole("button", { name: "Pin A calmer inbox" }));

    // The pin shows twice: the row's own 📌 and the press turning into Unpin.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Unpin A calmer inbox" })).toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: "Rename A calmer inbox" })).toBeInTheDocument();
    expect(commands).toEqual(["threads.snapshot", "threads.op"]);
    expect(asked[0]).toMatchObject({ thread: "s-1", verb: "pin" });
  });

  it("renames a conversation from the row's own form", async () => {
    const held = bootPayload({ threads: [row({ id: "s-1", title: "Old name" })], total: 1 });
    window.__MUSE_BOOT__ = held;
    const extra: Array<Record<string, unknown>> = [];
    installChannel(
      (command) => {
        if (command === "threads.op") {
          return {
            ok: true,
            accepted: true,
            ...bootPayload({ threads: [row({ id: "s-1", title: "New name" })], total: 1 }),
          };
        }
        return { ok: true, ...held };
      },
      (request) => {
        if (request.cmd === "threads.op") extra.push(request);
      },
    );

    render(<Threads />);
    fireEvent.click(screen.getByRole("button", { name: "Rename Old name" }));
    const field = screen.getByRole("textbox", { name: "New title" });
    fireEvent.change(field, { target: { value: "New name" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("New name")).toBeInTheDocument();
    expect(screen.queryByText("Old name")).toBeNull();
    expect(extra[0]).toMatchObject({ thread: "s-1", verb: "rename", title: "New name" });
  });

  it("archives a conversation and keeps the rows the service reads back", async () => {
    const held = bootPayload({
      threads: [row({ id: "s-1", title: "Done for now" }), row({ id: "s-2", title: "Still here", group: "Yesterday" })],
      total: 2,
    });
    window.__MUSE_BOOT__ = held;
    installChannel((command) => {
      if (command === "threads.op") {
        return {
          ok: true,
          accepted: true,
          ...bootPayload({ threads: [row({ id: "s-2", title: "Still here", group: "Yesterday" })], total: 1 }),
        };
      }
      return { ok: true, ...held };
    });

    render(<Threads />);
    fireEvent.click(screen.getByRole("button", { name: "Archive Done for now" }));

    await waitFor(() => expect(screen.queryByText("Done for now")).toBeNull());
    expect(screen.getByText("Still here")).toBeInTheDocument();
    expect(screen.getByText("1 threads")).toBeInTheDocument();
  });

  // Deleting a conversation is a real removal on the service, so the row asks
  // twice: the first press arms it, the second spends the arm.
  it("deletes a conversation only after a second press on the same row", async () => {
    const held = bootPayload({
      threads: [row({ id: "s-1", title: "Done for good" }), row({ id: "s-2", title: "Keep me", group: "Yesterday" })],
      total: 2,
    });
    window.__MUSE_BOOT__ = held;
    let deletes = 0;
    const commands = installChannel((command) => {
      if (command !== "threads.op") return { ok: true, ...held };
      deletes += 1;
      if (deletes === 1) {
        return { ok: true, accepted: false, armed: "s-1", ...held };
      }
      return {
        ok: true,
        accepted: true,
        armed: null,
        ...bootPayload({ threads: [row({ id: "s-2", title: "Keep me", group: "Yesterday" })], total: 1 }),
      };
    });

    render(<Threads />);
    fireEvent.click(screen.getByRole("button", { name: "Delete Done for good" }));

    // Armed, not deleted: the row is still here and the press now confirms.
    const confirm = await screen.findByRole("button", { name: "Confirm delete Done for good" });
    expect(screen.getByText("Done for good")).toBeInTheDocument();
    expect(commands).toEqual(["threads.snapshot", "threads.op"]);

    fireEvent.click(confirm);

    await waitFor(() => expect(screen.queryByText("Done for good")).toBeNull());
    expect(screen.getByText("Keep me")).toBeInTheDocument();
    expect(deletes).toBe(2);
  });

  // The mac app clears a conversation's unread badge when you open it; this
  // page also lets you clear one from the row, through the same service call.
  it("marks a row read through the app and paints the listing it reads back", async () => {
    const held = bootPayload({ threads: [row({ id: "s-1", title: "Something new", unread: 2 })], total: 1 });
    window.__MUSE_BOOT__ = held;
    const asked: Array<Record<string, unknown>> = [];
    const commands = installChannel(
      (command) =>
        command === "threads.seen"
          ? {
              ok: true,
              accepted: true,
              verb: "seen",
              ...bootPayload({ threads: [row({ id: "s-1", title: "Something new", unread: 0 })], total: 1 }),
            }
          : { ok: true, ...held },
      (request) => {
        if (request.cmd === "threads.seen") asked.push(request);
      },
    );

    render(<Threads />);
    expect(screen.getByTitle("2 unread")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Mark read Something new" }));

    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Mark read Something new" })).toBeNull(),
    );
    expect(screen.queryByTitle("2 unread")).toBeNull();
    expect(commands).toEqual(["threads.snapshot", "threads.seen"]);
    expect(asked[0]).toMatchObject({ thread: "s-1" });
  });

  it("offers no Mark read press on a row with nothing unread", async () => {
    const held = bootPayload({ threads: [row({ id: "s-1", title: "All caught up", unread: 0 })], total: 1 });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Threads />);

    await screen.findByText("All caught up");
    expect(screen.queryByRole("button", { name: "Mark read All caught up" })).toBeNull();
  });

  it("keeps the badge when the service refuses the read", async () => {
    const held = bootPayload({ threads: [row({ id: "s-1", title: "Still new", unread: 1 })], total: 1 });
    window.__MUSE_BOOT__ = held;
    installChannel((command) =>
      command === "threads.seen"
        ? { ok: true, accepted: false, verb: "seen", ...held }
        : { ok: true, ...held },
    );

    render(<Threads />);
    fireEvent.click(screen.getByRole("button", { name: "Mark read Still new" }));

    expect(
      await screen.findByText("Command failed: The service did not change it. Nothing changed."),
    ).toBeInTheDocument();
    expect(screen.getByTitle("1 unread")).toBeInTheDocument();
  });

  it("cancels an armed delete without sending a second press", async () => {
    const held = bootPayload({ threads: [row({ id: "s-1", title: "Spared thread" })], total: 1 });
    window.__MUSE_BOOT__ = held;
    const commands = installChannel((command) =>
      command === "threads.op"
        ? { ok: true, accepted: false, armed: "s-1", ...held }
        : { ok: true, ...held },
    );

    render(<Threads />);
    fireEvent.click(screen.getByRole("button", { name: "Delete Spared thread" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

    expect(await screen.findByRole("button", { name: "Delete Spared thread" })).toBeInTheDocument();
    expect(screen.getByText("Spared thread")).toBeInTheDocument();
    expect(commands).toEqual(["threads.snapshot", "threads.op"]);
  });

  it("paints the armed row from the app's snapshot instead of keeping its own", async () => {
    const held = bootPayload({
      threads: [row({ id: "s-1", title: "Armed already" })],
      total: 1,
      armed: "s-1",
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Threads />);

    expect(
      await screen.findByRole("button", { name: "Confirm delete Armed already" }),
    ).toBeInTheDocument();
  });

  it("says a refused change left the rows as they were", async () => {
    const held = bootPayload({ threads: [row({ id: "s-1", title: "Held thread" })], total: 1 });
    window.__MUSE_BOOT__ = held;
    installChannel((command) =>
      command === "threads.op"
        ? {
            ok: true,
            accepted: false,
            ...bootPayload({ threads: [row({ id: "s-1", title: "Held thread" })], total: 1 }),
          }
        : { ok: true, ...held },
    );

    render(<Threads />);
    fireEvent.click(screen.getByRole("button", { name: "Archive Held thread" }));

    expect(
      await screen.findByText("Command failed: The service did not change it. Nothing changed."),
    ).toBeInTheDocument();
    expect(screen.getByText("Held thread")).toBeInTheDocument();
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
