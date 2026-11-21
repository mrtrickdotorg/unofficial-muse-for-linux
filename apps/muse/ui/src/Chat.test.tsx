// The Chat page's DOM tests: the transcript the native list built, the states
// that keep an unread thread from reading as an empty one, and -- the reason
// this page exists -- a reply that grows keeping its element.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Chat } from "./Chat";
import { installChannel } from "./testChannel";
import type { ChatRow, ChatSnapshot } from "./types";

const message = (over: Partial<ChatRow>): ChatRow => ({
  kind: "message",
  id: "m-0",
  role: "assistant",
  text: "",
  day: "",
  sent_at: null,
  attachments: [],
  options: [],
  selected: null,
  ...over,
});

const snapshot = (rows: ChatRow[], over: Partial<ChatSnapshot> = {}): ChatSnapshot => ({
  thread: "s-1",
  title: "A calmer inbox",
  account: "Muse",
  status: null,
  sending: false,
  rows,
  loaded: true,
  error: null,
  ...over,
});

describe("the Chat web page", () => {
  it("opens on the native transcript's rows, notices and option pills", async () => {
    const boot = snapshot([
      { ...message({}), kind: "day", id: "", day: "Today" },
      message({ id: "m-1", role: "user", text: "hi" }),
      message({ id: "m-2", text: "hello back", sent_at: "12:01" }),
      { ...message({}), kind: "notice", id: "", text: "Usage limit reached" },
      message({
        id: "m-3",
        text: "which way?",
        options: ["Left", "Right"],
        selected: "Left",
      }),
    ]);
    window.__MUSE_BOOT__ = boot;
    const commands = installChannel(() => ({ ok: true, ...boot }));

    render(<Chat />);

    expect(screen.getByText("A calmer inbox")).toBeInTheDocument();
    expect(screen.getByText("Today")).toBeInTheDocument();
    expect(screen.getByText("hi")).toBeInTheDocument();
    expect(screen.getByText("hello back")).toBeInTheDocument();
    expect(screen.getByText("12:01")).toBeInTheDocument();
    expect(screen.getByText("Usage limit reached")).toBeInTheDocument();
    // The reader's pick is marked, the other option is offered: the native
    // row's own shape.
    expect(screen.getByText(/Left/)).toHaveTextContent("✓ Left");
    expect(screen.getByText("Right")).toBeInTheDocument();
    // Three message turns; the divider and the notice are their own shapes.
    expect(document.querySelectorAll('[data-testid^="turn-"]')).toHaveLength(3);
    await waitFor(() => expect(commands).toContain("chat.snapshot"));
  });

  it("names the unread thread instead of showing an empty one", async () => {
    installChannel(() => ({ ok: true, ...snapshot([], { loaded: false }) }));

    render(<Chat />);

    expect(await screen.findByText(/has not read a thread yet/)).toBeInTheDocument();
    expect(screen.queryByText("No messages yet.")).toBeNull();
  });

  it("says the composer's status line, so a failed send is visible here too", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "hi" })], {
      status: "Send failed (Timeout).",
    });
    installChannel(() => ({ ok: true, ...window.__MUSE_BOOT__ }));

    render(<Chat />);

    expect(screen.getByText("Send failed (Timeout).")).toBeInTheDocument();
  });

  it("keeps a growing reply's element and takes the new words", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-9", text: "Hel" })]);
    let reads = 0;
    installChannel(() => {
      reads += 1;
      return {
        ok: true,
        ...snapshot([
          message({
            id: "m-9",
            text: reads === 1 ? "Hel" : "Hello there, arriving in pieces.",
          }),
        ]),
      };
    });

    render(<Chat pollMs={20} />);
    const bubble = screen.getByText("Hel");

    // The poll lands the grown turn: same element, new words -- the DOM half
    // of the in-place rewrite the native transcript does.
    const grown = await screen.findByText("Hello there, arriving in pieces.");
    expect(grown).toBe(bubble);
    expect(document.querySelectorAll('[data-testid^="turn-"]')).toHaveLength(1);
    expect(reads).toBeGreaterThan(1);
  });

  it("sends through the app's own command and clears the entry", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "hi" })]);
    let sent: { text?: string } | null = null;
    installChannel(
      (cmd) => (cmd === "chat.send" ? { ok: true } : { ok: true, ...snapshot([]) }),
      (request) => {
        if (request.cmd === "chat.send") sent = request as unknown as { text?: string };
      },
    );

    render(<Chat pollMs={10_000} />);
    fireEvent.change(screen.getByPlaceholderText("Message"), {
      target: { value: "  hello there  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    // The words leave trimmed, on the one command that mutates.
    await waitFor(() => expect(sent?.text).toBe("hello there"));
    // Accepted, so the entry clears the way a press in the app clears it.
    await waitFor(() =>
      expect(screen.getByPlaceholderText("Message")).toHaveValue(""),
    );
  });

  it("keeps the draft and says the app's sentence when a send is refused", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "hi" })]);
    installChannel((cmd) =>
      cmd === "chat.send"
        ? {
            ok: false,
            error: "Still working — your message is kept; send again in a moment.",
          }
        : { ok: true, ...snapshot([]) },
    );

    render(<Chat pollMs={10_000} />);
    fireEvent.change(screen.getByPlaceholderText("Message"), {
      target: { value: "kept draft" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText(/your message is kept/)).toBeInTheDocument();
    // A refused press is never a lost draft.
    expect(screen.getByPlaceholderText("Message")).toHaveValue("kept draft");
  });

  it("swaps Send for Stop while a send is in flight and posts chat.stop", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "hi" })], {
      sending: true,
    });
    let stops = 0;
    const commands = installChannel(
      (cmd) => {
        if (cmd === "chat.stop") {
          stops += 1;
          return { ok: true };
        }
        return { ok: true, ...snapshot([], { sending: true }) };
      },
    );

    render(<Chat pollMs={10_000} />);

    // The native card's swap: Stop where Send was, and no separate Send.
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(stops).toBe(1));
    expect(commands).toContain("chat.stop");
  });

  it("says the app's sentence when a stop is refused", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "hi" })], {
      sending: true,
    });
    installChannel((cmd) =>
      cmd === "chat.stop"
        ? { ok: false, error: "Sign in before stopping." }
        : { ok: true, ...snapshot([], { sending: true }) },
    );

    render(<Chat pollMs={10_000} />);
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));

    expect(await screen.findByText("Sign in before stopping.")).toBeInTheDocument();
  });

  it("reports a failed snapshot and keeps the rows already on screen", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "held" })]);
    installChannel(() => ({ ok: false, error: "no account link in this app" }));

    render(<Chat />);

    expect(
      await screen.findByText("Command failed: no account link in this app"),
    ).toBeInTheDocument();
    expect(screen.getByText("held")).toBeInTheDocument();
  });
});
