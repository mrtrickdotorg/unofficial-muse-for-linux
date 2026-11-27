// S2's gate for the Search page: the page's behaviour against the same
// channel the app speaks, with no window and no service.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Search } from "./Search";
import { installChannel } from "./testChannel";
import type { SearchRow, SearchSnapshot } from "./types";

const row = (overrides: Partial<SearchRow> = {}): SearchRow => ({
  id: "thread-1",
  icon: "🚗",
  title: "Visitor parking",
  meta: "Rowyn · message · 2026-11-20T09:14:00Z",
  image_url: "",
  ...overrides,
});

const bootPayload = (overrides: Partial<SearchSnapshot> = {}) => ({
  page: "search" as const,
  query: "",
  rows: [] as SearchRow[],
  total: 0,
  loaded: true,
  error: null,
  ...overrides,
});

const rowsOnPage = () => document.querySelectorAll(".search-row").length;

describe("the Search web page", () => {
  it("opens on the query and rows the native search last ran", async () => {
    const held = bootPayload({
      query: "parking",
      rows: [
        row({ id: "thread-1", title: "Visitor parking" }),
        row({ id: "thread-2", title: "Lot ramp", icon: "🅿️", meta: "" }),
      ],
      total: 3,
    });
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Search />);

    expect(screen.getByRole("heading", { name: "Search", level: 1 })).toBeInTheDocument();
    expect(screen.getByLabelText("Search every conversation")).toHaveValue("parking");
    expect(screen.getByText("Results for “parking”")).toBeInTheDocument();
    expect(screen.getByText("Visitor parking")).toBeInTheDocument();
    expect(screen.getByText("Lot ramp")).toBeInTheDocument();
    expect(screen.getByText("Rowyn · message · 2026-11-20T09:14:00Z")).toBeInTheDocument();
    expect(screen.getByText("2 of 3 results")).toBeInTheDocument();
    expect(rowsOnPage()).toBe(2);
    expect(commands).toEqual(["search.snapshot"]);
  });

  it("names the not-loaded state instead of an empty account", async () => {
    const commands = installChannel(() => ({ ok: true, ...bootPayload({ loaded: false }) }));

    render(<Search />);

    expect(await screen.findByText(/has not run a search yet/)).toBeInTheDocument();
    expect(screen.queryByText("No matches.")).not.toBeInTheDocument();
    expect(commands).toEqual(["search.snapshot"]);
  });

  it("runs the reader's words over the channel and paints what comes back", async () => {
    const held = bootPayload({ query: "parking", rows: [row({ id: "thread-1", title: "Held result" })], total: 1 });
    window.__MUSE_BOOT__ = held;
    const fresh = {
      ...held,
      query: "lot ramp",
      rows: [row({ id: "thread-3", title: "Ramp barrier", meta: "" })],
      total: 2,
    };
    const requests: Record<string, unknown>[] = [];
    installChannel(
      (command) => (command === "search.run" ? { ok: true, ...fresh } : { ok: true, ...held }),
      (request) => requests.push(request),
    );

    render(<Search />);
    fireEvent.change(screen.getByLabelText("Search every conversation"), {
      target: { value: "lot ramp" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Search" }));

    expect(await screen.findByText("Ramp barrier")).toBeInTheDocument();
    expect(screen.queryByText("Held result")).not.toBeInTheDocument();
    expect(screen.getByText("1 of 2 results")).toBeInTheDocument();
    expect(screen.getByText("Results for “lot ramp”")).toBeInTheDocument();
    // The words travel with the command: this is the only page whose read
    // carries the reader's own text.
    const sent = requests.find((request) => request.cmd === "search.run");
    expect(sent?.query).toBe("lot ramp");
  });

  it("opens a result's conversation through the shell's own switch", async () => {
    const held = bootPayload({
      query: "parking",
      rows: [row({ id: "thread-9", title: "Visitor parking" })],
      total: 1,
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));
    const opened: string[] = [];

    render(<Search onOpenThread={(id) => opened.push(id)} />);
    fireEvent.click(await screen.findByRole("button", { name: /Visitor parking/ }));

    // The row carries the conversation id the native row switches to, and
    // the press is the shell's own -- not a second, page-made read.
    expect(opened).toEqual(["thread-9"]);
  });

  it("leaves a result unpressable when no shell can open it", async () => {
    const held = bootPayload({
      query: "parking",
      rows: [row({ id: "thread-9", title: "Visitor parking" })],
      total: 1,
    });
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Search />);

    expect(await screen.findByText("Visitor parking")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Visitor parking/ })).not.toBeInTheDocument();
    expect(rowsOnPage()).toBe(1);
    expect(commands).toEqual(["search.snapshot"]);
  });

  it("refuses an empty query without asking the channel", async () => {
    const held = bootPayload({ query: "parking", rows: [row({ title: "Held result" })], total: 1 });
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Search />);
    fireEvent.change(screen.getByLabelText("Search every conversation"), {
      target: { value: "   " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Search" }));

    expect(screen.getByText("Type words to search for first.")).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Search" })).toBeEnabled(),
    );
    expect(commands).toEqual(["search.snapshot"]);
    expect(screen.getByText("Held result")).toBeInTheDocument();
  });

  it("keeps the held rows and reports a refused search", async () => {
    const held = bootPayload({ query: "parking", rows: [row({ title: "Held result" })], total: 1 });
    window.__MUSE_BOOT__ = held;
    installChannel((command) =>
      command === "search.run"
        ? { ok: false, error: "the gateway went away" }
        : { ok: true, ...held },
    );

    render(<Search />);
    fireEvent.click(screen.getByRole("button", { name: "Search" }));

    expect(await screen.findByText(/Command failed: the gateway went away/)).toBeInTheDocument();
    expect(screen.getByText("Held result")).toBeInTheDocument();
  });

  it("reports the native search's last failure beside the rows it kept", async () => {
    const held = bootPayload({
      query: "parking",
      rows: [row({ title: "Held result" })],
      total: 1,
      error: "the gateway went away",
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Search />);

    expect(screen.getByText(/The last read failed: the gateway went away/)).toBeInTheDocument();
    expect(screen.getByText("Held result")).toBeInTheDocument();
  });

  it("names matches the service counted but this build could not read", async () => {
    const held = bootPayload({ query: "Rowyn", rows: [], total: 20 });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Search />);

    // The live service reports a count with no rows this build can map; an
    // empty list beside a count of 20 must not read as "no matches".
    expect(
      await screen.findByText(
        "The service counted 20 matches for “Rowyn”, but none carried a conversation this build could open.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/No matches for/)).not.toBeInTheDocument();
  });

  it("says which words found nothing rather than claiming an empty account", async () => {
    const held = bootPayload({ query: "parking", rows: [row({ title: "Held result" })], total: 1 });
    window.__MUSE_BOOT__ = held;
    installChannel((command) =>
      command === "search.run"
        ? { ok: true, ...bootPayload({ query: "zebra", rows: [], total: 0 }) }
        : { ok: true, ...held },
    );

    render(<Search />);
    fireEvent.change(screen.getByLabelText("Search every conversation"), {
      target: { value: "zebra" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Search" }));

    expect(await screen.findByText("No matches for “zebra”.")).toBeInTheDocument();
    expect(rowsOnPage()).toBe(0);
  });
});
