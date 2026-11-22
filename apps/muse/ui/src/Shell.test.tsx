// The web shell's DOM tests: the rail and drawer it builds from the app's page
// list, the History rows it reads from the panel, the entry points that reach
// it, and the keyboard map -- all against the same channel the app speaks.
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Shell } from "./Shell";
import { installChannel } from "./testChannel";
import type { ChatSnapshot, SettingsSnapshot, ShellSnapshot } from "./types";

const shell = (over: Partial<ShellSnapshot> = {}): ShellSnapshot & { page: "shell" } => ({
  page: "shell",
  pages: [
    { name: "chat", title: "Chat" },
    { name: "search", title: "Search" },
    { name: "settings", title: "Settings" },
  ],
  account: "Muse",
  build: "build abc1234",
  history: [
    { id: "s-1", text: "📌 A calmer inbox (2 unread)", current: true },
    { id: "s-2", text: "(untitled)", current: false },
  ],
  ...over,
});

const chat: ChatSnapshot = {
  thread: "s-1",
  title: "A calmer inbox",
  account: "Muse",
  status: null,
  sending: false,
  staged: [],
  unread: 0,
  approvals: { cards: [], loaded: true, note: null },
  rows: [
    {
      kind: "message",
      id: "m-1",
      role: "assistant",
      text: "hello from the shell",
      day: "",
      sent_at: null,
      attachments: [],
      options: [],
      selected: null,
    },
  ],
  loaded: true,
  error: null,
};

const settings: SettingsSnapshot = {
  link: {
    linked: true,
    interpreter: "/tmp/python",
    session_folder: "/tmp/session",
  },
  updates: {
    appimage: false,
    status: "Up to date.",
    busy: false,
    checked: true,
    staged: false,
  },
  policy: { loaded: true, rows: [], connectors: [], withheld: 0, note: null },
};

// Each command answers the way the native side would: page snapshots from held
// state, History from the panel's own listing.
const answer = (command: string) => {
  if (command === "chat.snapshot") return { ok: true, ...chat };
  if (command === "settings.snapshot") return { ok: true, ...settings };
  if (command === "shell.snapshot") return { ok: true, ...shell() };
  if (command === "history.snapshot") return { ok: true, rows: shell().history };
  if (command === "search.snapshot" || command === "search.run") {
    return { ok: true, query: "", rows: [], total: 0, loaded: false };
  }
  return { ok: true };
};

const rail = () => screen.getByRole("navigation", { name: "Pages" });
const pageList = () => screen.getByRole("navigation", { name: "Page list" });
const drawer = () => screen.getByRole("complementary", { name: "Drawer" });

describe("the web shell", () => {
  it("builds its rail, page list and History from the app's own lists", async () => {
    window.__MUSE_BOOT__ = shell();
    const commands = installChannel(answer);

    render(<Shell />);

    expect(within(rail()).getAllByRole("button")).toHaveLength(3);
    expect(within(pageList()).getAllByRole("button")).toHaveLength(3);
    expect(within(rail()).getByRole("button", { name: "Chat" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    // History is the panel's own wording, verbatim, with the open thread marked
    // and nothing navigated by merely reading it.
    expect(
      within(drawer()).getByRole("button", { name: "📌 A calmer inbox (2 unread)" }),
    ).toHaveAttribute("aria-current", "true");
    expect(within(drawer()).getByRole("button", { name: "(untitled)" })).not.toHaveAttribute(
      "aria-current",
    );
    // The drawer carries the same account and build stamp the native one does.
    expect(within(drawer()).getByText("Muse")).toBeInTheDocument();
    expect(within(drawer()).getByText("build abc1234")).toBeInTheDocument();
    await waitFor(() => expect(commands).toContain("chat.snapshot"));
    expect(commands).toContain("history.snapshot");
    expect(await screen.findByText("hello from the shell")).toBeInTheDocument();
  });

  it("mounts the page a press selects, and asks that page's own snapshot", async () => {
    window.__MUSE_BOOT__ = shell();
    const commands = installChannel(answer);

    render(<Shell />);
    fireEvent.click(within(pageList()).getByRole("button", { name: "Settings" }));

    // The page is the ported Settings component, not a shell imitation of it.
    expect(await screen.findByRole("heading", { name: "Settings", level: 1 })).toBeInTheDocument();
    await waitFor(() => expect(commands).toContain("settings.snapshot"));
    expect(screen.getByText("/tmp/python")).toBeInTheDocument();
    expect(within(pageList()).getByRole("button", { name: "Settings" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(within(pageList()).getByRole("button", { name: "Chat" })).not.toHaveAttribute(
      "aria-current",
    );
    // The page it replaced is gone, not stacked underneath.
    expect(screen.queryByText("hello from the shell")).not.toBeInTheDocument();
  });

  it("opens from the rail as well as the page list", async () => {
    window.__MUSE_BOOT__ = shell();
    installChannel(answer);

    render(<Shell />);
    fireEvent.click(within(rail()).getByRole("button", { name: "Settings" }));

    expect(await screen.findByRole("heading", { name: "Settings", level: 1 })).toBeInTheDocument();
  });

  it("opens a thread from History and a fresh chat from its press", async () => {
    window.__MUSE_BOOT__ = shell();
    const seen: Record<string, unknown>[] = [];
    const commands = installChannel(answer, (request) => seen.push(request));

    render(<Shell />);
    fireEvent.click(within(drawer()).getByRole("button", { name: "(untitled)" }));

    // The row's id is the app's own session id, not a label the page invented.
    await waitFor(() =>
      expect(seen.find((request) => request.cmd === "history.open")).toMatchObject({
        thread: "s-2",
      }),
    );
    // Opening a thread is the Chat page's own switch, so the shell shows it.
    expect(await screen.findByText("A calmer inbox", {}, { timeout: 2000 })).toBeInTheDocument();
    // The listing's mark moved: the panel's rows are re-read after the open.
    await waitFor(() =>
      expect(commands.filter((command) => command === "history.snapshot").length).toBeGreaterThan(
        1,
      ),
    );

    await waitFor(() => expect(within(drawer()).getByRole("button", { name: "New chat" })).toBeEnabled());
    fireEvent.click(within(drawer()).getByRole("button", { name: "New chat" }));
    await waitFor(() => expect(commands).toContain("chat.new"));
  });

  it("hands the drawer's words to the Search page without reading them", async () => {
    window.__MUSE_BOOT__ = shell();
    const commands = installChannel(answer);

    render(<Shell />);
    fireEvent.change(screen.getByRole("searchbox", { name: "Search the account" }), {
      target: { value: "mirror" },
    });
    fireEvent.submit(screen.getByRole("search"));

    // The page opens with the words in its own box; its Run is still the one
    // read, so nothing went to the wire yet.
    const page = within(await screen.findByRole("main")).getByRole("searchbox");
    expect(page).toHaveValue("mirror");
    expect(commands).not.toContain("search.run");
    expect(within(pageList()).getByRole("button", { name: "Search" })).toHaveAttribute(
      "aria-current",
      "page",
    );
  });

  it("runs the app's keyboard map from wherever focus is", async () => {
    window.__MUSE_BOOT__ = shell();
    const commands = installChannel(answer);

    render(<Shell />);
    const box = screen.getByRole("searchbox", { name: "Search the account" });

    // Ctrl+K reaches the search entry, even from a page that took the focus.
    within(rail()).getByRole("button", { name: "Chat" }).focus();
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    expect(document.activeElement).toBe(box);

    // Ctrl+N is a fresh thread, the rail's plus by another route.
    fireEvent.keyDown(window, { key: "n", ctrlKey: true });
    await waitFor(() => expect(commands).toContain("chat.new"));

    // Ctrl+, lands on Settings.
    fireEvent.keyDown(window, { key: ",", ctrlKey: true });
    expect(await screen.findByRole("heading", { name: "Settings", level: 1 })).toBeInTheDocument();

    // F6 walks the three panes this window has, and Shift+F6 walks back.
    fireEvent.keyDown(window, { key: "F6" });
    expect(document.activeElement).toBe(rail());
    fireEvent.keyDown(window, { key: "F6" });
    expect(document.activeElement).toBe(drawer());
    fireEvent.keyDown(window, { key: "F6" });
    expect(document.activeElement).toBe(screen.getByRole("main"));
    fireEvent.keyDown(window, { key: "F6", shiftKey: true });
    expect(document.activeElement).toBe(drawer());

    // Escape leaves the search box; it does not swallow a press meant for the
    // page when the box is not the thing focused.
    box.focus();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(document.activeElement).not.toBe(box);
  });

  it("names an unread History instead of claiming an empty account", async () => {
    window.__MUSE_BOOT__ = shell({ history: null });
    installChannel((command) =>
      command === "history.snapshot"
        ? { ok: false, error: "this window cannot read history" }
        : answer(command),
    );

    render(<Shell />);

    expect(
      await screen.findByText("this window cannot read history"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("No threads in the app's listing yet."),
    ).not.toBeInTheDocument();
  });

  it("names a page it has no web view for instead of hiding it", async () => {
    window.__MUSE_BOOT__ = shell({
      pages: [
        { name: "chat", title: "Chat" },
        { name: "archive", title: "Archive" },
      ],
      account: "",
      history: [],
    });
    installChannel(answer);

    render(<Shell />);
    expect(within(pageList()).getByRole("button", { name: "Archive" })).toBeInTheDocument();
    expect(within(drawer()).getByText("Sign in")).toBeInTheDocument();
    expect(screen.getByText("No threads in the app's listing yet.")).toBeInTheDocument();

    fireEvent.click(within(pageList()).getByRole("button", { name: "Archive" }));
    expect(await screen.findByText(/This page has no web view yet/)).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Archive", level: 1 })).toBeInTheDocument();
  });
});
