// S2's gate for the Tasks page: the page's behaviour against the same
// channel the app speaks, with no window and no service.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Tasks } from "./Tasks";
import { installChannel } from "./testChannel";
import type { TaskRow, TasksSnapshot } from "./types";

const row = (id: string, label: string): TaskRow => ({ id, label });

const bootPayload = (overrides: Partial<TasksSnapshot> = {}) => ({
  page: "tasks" as const,
  schedules: [] as TaskRow[],
  runs: [] as TaskRow[],
  local: [] as TaskRow[],
  loaded: true,
  error: null,
  ...overrides,
});

const cards = () => document.querySelectorAll(".task").length;

describe("the Tasks web page", () => {
  it("opens on the held schedules, runs and local reminders", async () => {
    const held = bootPayload({
      schedules: [row("job-1", "Nightly build (on)")],
      runs: [row("run-1", "ok: built")],
      local: [row("7", "Call the dentist · in 2h · pending")],
    });
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Tasks />);

    expect(
      screen.getByRole("heading", { name: "Tasks", level: 1 }),
    ).toBeInTheDocument();
    expect(screen.getByText("Nightly build (on)")).toBeInTheDocument();
    expect(screen.getByText("ok: built")).toBeInTheDocument();
    expect(screen.getByText("Call the dentist · in 2h · pending")).toBeInTheDocument();
    expect(screen.getByText("1 scheduled · 1 recent")).toBeInTheDocument();
    expect(cards()).toBe(3);
    expect(commands).toEqual(["tasks.snapshot"]);
  });

  it("names the not-loaded state instead of empty schedules and runs", async () => {
    const commands = installChannel(() => ({
      ok: true,
      ...bootPayload({ loaded: false }),
    }));

    render(<Tasks />);

    expect(await screen.findByText(/has not loaded yet/)).toBeInTheDocument();
    expect(screen.queryByText("No scheduled jobs on this account.")).toBeNull();
    expect(screen.queryByText(/^\d+ scheduled/)).toBeNull();
    // Local reminders do not depend on the service read, so their own
    // honest empty line still shows.
    expect(screen.getByText("No local reminders.")).toBeInTheDocument();
    expect(commands).toEqual(["tasks.snapshot"]);
  });

  it("refreshes over the wire and shows the listing that comes back", async () => {
    window.__MUSE_BOOT__ = bootPayload({
      schedules: [row("held", "Held job (on)")],
    });
    const fresh = bootPayload({
      schedules: [row("job-1", "Nightly build (on)"), row("job-2", "Weekly report (off)")],
      runs: [row("run-1", "ok: built")],
    });
    const commands = installChannel((command) =>
      command === "tasks.refresh"
        ? { ok: true, ...fresh }
        : {
            ok: true,
            ...bootPayload({ schedules: [row("held", "Held job (on)")] }),
          },
    );

    render(<Tasks />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByText("Weekly report (off)")).toBeInTheDocument();
    expect(screen.getByText("2 scheduled · 1 recent")).toBeInTheDocument();
    expect(screen.queryByText("Held job (on)")).toBeNull();
    expect(commands).toEqual(["tasks.snapshot", "tasks.refresh"]);
  });

  it("reports a refused refresh and keeps the rows it has", async () => {
    window.__MUSE_BOOT__ = bootPayload({
      schedules: [row("held", "Held job (on)")],
    });
    installChannel((command) =>
      command === "tasks.refresh"
        ? { ok: false, error: "no account link in this app" }
        : { ok: true, ...bootPayload({ schedules: [row("held", "Held job (on)")] }) },
    );

    render(<Tasks />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(
      await screen.findByText("Command failed: no account link in this app"),
    ).toBeInTheDocument();
    expect(screen.getByText("Held job (on)")).toBeInTheDocument();
    expect(cards()).toBe(1);
  });

  it("says an account read as empty rather than that it has not loaded", async () => {
    window.__MUSE_BOOT__ = bootPayload();
    installChannel(() => ({ ok: true, ...bootPayload() }));

    render(<Tasks />);

    await waitFor(() =>
      expect(screen.getByText("No scheduled jobs on this account.")).toBeInTheDocument(),
    );
    expect(screen.getByText("No runs yet.")).toBeInTheDocument();
    expect(screen.getByText("No local reminders.")).toBeInTheDocument();
    expect(screen.queryByText(/has not loaded yet/)).toBeNull();
    expect(screen.getByText("0 scheduled · 0 recent")).toBeInTheDocument();
    expect(cards()).toBe(0);
  });

  it("reports a failed read beside the rows the page still holds", async () => {
    window.__MUSE_BOOT__ = bootPayload({
      schedules: [row("held", "Held job (on)")],
    });
    installChannel(() => ({
      ok: true,
      ...bootPayload({
        schedules: [row("held", "Held job (on)")],
        error: "the host stopped answering",
      }),
    }));

    render(<Tasks />);

    expect(
      await screen.findByText("The last read failed: the host stopped answering"),
    ).toBeInTheDocument();
    expect(screen.getByText("Held job (on)")).toBeInTheDocument();
  });
});
