// The web shell's DOM tests: the rail and drawer it builds from the app's page
// list, and that each selection mounts the page the native window would show.
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Shell } from "./Shell";
import { installChannel } from "./testChannel";
import type { ChatSnapshot, SettingsSnapshot, ShellSnapshot } from "./types";

const shell = (over: Partial<ShellSnapshot> = {}): ShellSnapshot & { page: "shell" } => ({
  page: "shell",
  pages: [
    { name: "chat", title: "Chat" },
    { name: "settings", title: "Settings" },
  ],
  account: "Muse",
  build: "build abc1234",
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

// Each page asks its own snapshot over the same channel the app speaks.
const answer = (command: string) => {
  if (command === "chat.snapshot") return { ok: true, ...chat };
  if (command === "settings.snapshot") return { ok: true, ...settings };
  if (command === "shell.snapshot") return { ok: true, ...shell() };
  return { ok: true };
};

describe("the web shell", () => {
  it("builds its rail and drawer from the app's page list", async () => {
    window.__MUSE_BOOT__ = shell();
    const commands = installChannel(answer);

    render(<Shell />);

    const rail = screen.getByRole("navigation", { name: "Pages" });
    const drawer = screen.getByRole("complementary", { name: "Drawer" });
    expect(within(rail).getAllByRole("button")).toHaveLength(2);
    expect(within(drawer).getAllByRole("button")).toHaveLength(2);
    expect(within(rail).getByRole("button", { name: "Chat" })).toBeInTheDocument();
    expect(within(drawer).getByRole("button", { name: "Settings" })).toBeInTheDocument();
    // The drawer carries the same account and build stamp the native one does.
    expect(screen.getByText("Muse")).toBeInTheDocument();
    expect(screen.getByText("build abc1234")).toBeInTheDocument();
    // The first page in the list is the one open, in both places.
    expect(within(rail).getByRole("button", { name: "Chat" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    await waitFor(() => expect(commands).toContain("chat.snapshot"));
    expect(await screen.findByText("hello from the shell")).toBeInTheDocument();
  });

  it("mounts the page a press selects, and asks that page's own snapshot", async () => {
    window.__MUSE_BOOT__ = shell();
    const commands = installChannel(answer);

    render(<Shell />);
    const drawer = screen.getByRole("complementary", { name: "Drawer" });
    fireEvent.click(within(drawer).getByRole("button", { name: "Settings" }));

    // The page is the ported Settings component, not a shell imitation of it.
    expect(await screen.findByRole("heading", { name: "Settings", level: 1 })).toBeInTheDocument();
    await waitFor(() => expect(commands).toContain("settings.snapshot"));
    expect(screen.getByText("/tmp/python")).toBeInTheDocument();
    // The mark follows the selection in both lists.
    expect(within(drawer).getByRole("button", { name: "Settings" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(within(drawer).getByRole("button", { name: "Chat" })).not.toHaveAttribute(
      "aria-current",
    );
    // The page it replaced is gone, not stacked underneath.
    expect(screen.queryByText("hello from the shell")).not.toBeInTheDocument();
  });

  it("opens from the rail as well as the drawer", async () => {
    window.__MUSE_BOOT__ = shell();
    installChannel(answer);

    render(<Shell />);
    const rail = screen.getByRole("navigation", { name: "Pages" });
    fireEvent.click(within(rail).getByRole("button", { name: "Settings" }));

    expect(await screen.findByRole("heading", { name: "Settings", level: 1 })).toBeInTheDocument();
  });

  it("names a page it has no web view for instead of hiding it", async () => {
    window.__MUSE_BOOT__ = shell({
      pages: [
        { name: "chat", title: "Chat" },
        { name: "archive", title: "Archive" },
      ],
      account: "",
    });
    installChannel(answer);

    render(<Shell />);
    const drawer = screen.getByRole("complementary", { name: "Drawer" });
    expect(within(drawer).getByRole("button", { name: "Archive" })).toBeInTheDocument();
    // The drawer names the signed-out state, as the native profile row does.
    expect(within(drawer).getByText("Sign in")).toBeInTheDocument();

    fireEvent.click(within(drawer).getByRole("button", { name: "Archive" }));
    expect(
      await screen.findByText(/This page has no web view yet/),
    ).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Archive", level: 1 })).toBeInTheDocument();
  });
});
