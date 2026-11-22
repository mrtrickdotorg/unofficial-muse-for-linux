// The Chat page's DOM tests: the transcript the native list built, the states
// that keep an unread thread from reading as an empty one, and -- the reason
// this page exists -- a reply that grows keeping its element.
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
  staged: [],
  unread: 0,
  approvals: { cards: [], loaded: true, note: null },
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

  it("stages a quoted turn, sends it with the draft, and clears it", async () => {
    window.__MUSE_BOOT__ = snapshot([
      message({ id: "m-1", role: "user", text: "hi" }),
      message({
        id: "m-2",
        text: "A teapot beats a kettle\nfor reasons we will not go into.",
      }),
    ]);
    let sent: { text?: string; quote?: unknown } | null = null;
    installChannel(
      (cmd) => (cmd === "chat.send" ? { ok: true } : { ok: true, ...snapshot([]) }),
      (request) => {
        if (request.cmd === "chat.send") sent = request as { text?: string; quote?: unknown };
      },
    );

    render(<Chat pollMs={10_000} />);

    // Reply stages the row's first line, capped and trimmed, and does not arm
    // a send by itself.
    fireEvent.click(
      screen.getByRole("button", { name: "Reply to Muse's message" }),
    );
    expect(screen.getByText(/Replying to: A teapot beats a kettle/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();

    fireEvent.change(screen.getByPlaceholderText("Message"), {
      target: { value: "because it does not scream" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() =>
      expect(sent).toMatchObject({
        text: "because it does not scream",
        quote: { id: "m-2", excerpt: "A teapot beats a kettle" },
      }),
    );
    // A send that goes takes its quote with it.
    await waitFor(() =>
      expect(screen.queryByText(/Replying to:/)).toBeNull(),
    );
  });

  it("names the turn a message answers in the transcript", async () => {
    window.__MUSE_BOOT__ = snapshot([
      message({ id: "m-1", text: "the quoted turn" }),
      message({
        id: "m-2",
        role: "user",
        text: "the answer",
        reply_to: "the quoted turn",
      }),
    ]);
    installChannel(() => ({ ok: true, ...snapshot([]) }));

    render(<Chat pollMs={10_000} />);

    expect(screen.getByText("Replying to: the quoted turn")).toBeInTheDocument();
    // An ordinary turn names nothing.
    expect(screen.getAllByText(/Replying to:/)).toHaveLength(1);
  });

  it("cancels a staged quote without sending anything", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-2", text: "quoted line" })]);
    const commands = installChannel(() => ({ ok: true, ...snapshot([]) }));

    render(<Chat pollMs={10_000} />);
    fireEvent.click(
      screen.getByRole("button", { name: "Reply to Muse's message" }),
    );
    expect(screen.getByText(/Replying to: quoted line/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Cancel reply" }));
    expect(screen.queryByText(/Replying to:/)).toBeNull();
    // Cancel is local: no command, no send.
    expect(commands).not.toContain("chat.send");
  });

  it("stages a clipboard picture through the app and drops it by name", async () => {
    const boot = snapshot([message({ id: "m-1", text: "hi" })], {
      staged: [{ name: "pasted-1.png", width: 10, height: 20, bytes: 33 }],
    });
    window.__MUSE_BOOT__ = boot;
    let dropped: string | undefined;
    const commands = installChannel(
      // The chip stays staged across the paste, as the app's would.
      (cmd) => (cmd === "chat.unstage" ? { ok: true } : { ok: true, ...boot }),
      (request) => {
        if (request.cmd === "chat.unstage") dropped = request.name as string;
      },
    );

    render(<Chat pollMs={10_000} />);

    // The chip names the app's own staged picture, in the native chip's words.
    expect(screen.getByText(/pasted-1\.png/)).toBeInTheDocument();
    expect(screen.getByText("10 × 20 · 33 bytes")).toBeInTheDocument();

    // The `+` is the app's clipboard read, not a page-side paste.
    fireEvent.click(
      screen.getByRole("button", { name: "Attach a picture from the clipboard" }),
    );
    await waitFor(() => expect(commands).toContain("chat.attach"));

    // The chip's × drops that one picture, by name.
    fireEvent.click(screen.getByRole("button", { name: "Drop pasted-1.png" }));
    await waitFor(() => expect(dropped).toBe("pasted-1.png"));
  });

  it("posts one paste per press and drops a second while the first is in flight", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "hi" })]);
    const commands = installChannel(
      (cmd) => (cmd === "chat.attach" ? { ok: true } : { ok: true, ...snapshot([]) }),
      undefined,
      60,
    );

    render(<Chat pollMs={10_000} />);
    const attach = screen.getByRole("button", {
      name: "Attach a picture from the clipboard",
    });

    fireEvent.click(attach);
    // The app has not answered yet: the control is claimed, so a second press
    // is not a second clipboard read.
    expect(attach).toBeDisabled();
    fireEvent.click(attach);
    expect(commands.filter((cmd) => cmd === "chat.attach")).toHaveLength(1);
    // The answer lands and the `+` comes back, ready for the next picture.
    await waitFor(() => expect(attach).toBeEnabled());
    fireEvent.click(attach);
    await waitFor(() =>
      expect(commands.filter((cmd) => cmd === "chat.attach")).toHaveLength(2),
    );
  });

  it("does not offer a paste while a send is in flight", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "hi" })], {
      sending: true,
    });
    const commands = installChannel(() => ({ ok: true, ...snapshot([], { sending: true }) }));

    render(<Chat pollMs={10_000} />);
    const attach = screen.getByRole("button", {
      name: "Attach a picture from the clipboard",
    });

    // The app refuses a paste mid-send, so the press is not offered rather
    // than sent and dropped.
    expect(attach).toBeDisabled();
    fireEvent.click(attach);
    expect(commands).not.toContain("chat.attach");
  });

  it("posts one drop per press, by name, and holds the chips while it lands", async () => {
    window.__MUSE_BOOT__ = snapshot([], {
      staged: [
        { name: "a.png", width: 1, height: 1, bytes: 1 },
        { name: "b.png", width: 2, height: 2, bytes: 2 },
      ],
    });
    const dropped: string[] = [];
    const commands = installChannel(
      (cmd) => (cmd === "chat.unstage" ? { ok: true } : { ok: true, ...window.__MUSE_BOOT__ }),
      (request) => {
        if (request.cmd === "chat.unstage") dropped.push(request.name as string);
      },
      60,
    );

    render(<Chat pollMs={10_000} />);
    fireEvent.click(screen.getByRole("button", { name: "Drop a.png" }));

    // One press, one name, and no second drop while it is unanswered.
    expect(dropped).toEqual(["a.png"]);
    fireEvent.click(screen.getByRole("button", { name: "Drop b.png" }));
    expect(dropped).toEqual(["a.png"]);
    expect(commands.filter((cmd) => cmd === "chat.unstage")).toHaveLength(1);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Drop b.png" })).toBeEnabled(),
    );
  });

  it("posts one stop per press, and a later press still stops", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "hi" })], {
      sending: true,
    });
    const commands = installChannel(
      (cmd) => (cmd === "chat.stop" ? { ok: true } : { ok: true, ...snapshot([], { sending: true }) }),
      undefined,
      60,
    );

    render(<Chat pollMs={10_000} />);
    const stop = screen.getByRole("button", { name: "Stop" });

    // A double press is one cancel, not two.
    fireEvent.click(stop);
    expect(stop).toBeDisabled();
    fireEvent.click(stop);
    expect(commands.filter((cmd) => cmd === "chat.stop")).toHaveLength(1);

    // The app answered: a new press is a new stop, as on the native card.
    await waitFor(() => expect(stop).toBeEnabled());
    fireEvent.click(stop);
    await waitFor(() =>
      expect(commands.filter((cmd) => cmd === "chat.stop")).toHaveLength(2),
    );
  });

  it("posts one approvals read per press", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "hi" })]);
    const commands = installChannel(
      (cmd) =>
        cmd === "chat.approvals.refresh" ? { ok: true } : { ok: true, ...snapshot([]) },
      undefined,
      60,
    );

    render(<Chat pollMs={10_000} />);
    const check = screen.getByRole("button", { name: "Check approvals" });

    fireEvent.click(check);
    expect(check).toBeDisabled();
    fireEvent.click(check);
    expect(commands.filter((cmd) => cmd === "chat.approvals.refresh")).toHaveLength(1);
    await waitFor(() => expect(check).toBeEnabled());
  });

  it("decides one card at a time and posts nothing for a second card's press", async () => {
    window.__MUSE_BOOT__ = snapshot([], {
      approvals: {
        loaded: true,
        note: null,
        cards: [
          {
            id: "a-1",
            headline: "Reach example.com",
            summary: "",
            details: ["Host: example.com"],
            choices: [{ word: "deny", label: "Deny" }],
          },
          {
            id: "a-2",
            headline: "Reach example.org",
            summary: "",
            details: ["Host: example.org"],
            choices: [{ word: "deny", label: "Deny" }],
          },
        ],
      },
    });
    const decided: string[] = [];
    const commands = installChannel(
      (cmd) => (cmd === "chat.decide" ? { ok: true } : { ok: true, ...window.__MUSE_BOOT__ }),
      (request) => {
        if (request.cmd === "chat.decide") decided.push(request.approval_id as string);
      },
      60,
    );

    render(<Chat pollMs={10_000} />);
    const deny = screen.getAllByRole("button", { name: "Deny" });

    fireEvent.click(deny[0]);
    // Both cards are held while the one decision is unanswered: one press, one
    // decision, and no second card racing the first.
    expect(deny[0]).toBeDisabled();
    expect(deny[1]).toBeDisabled();
    fireEvent.click(deny[1]);
    expect(decided).toEqual(["a-1"]);
    expect(commands.filter((cmd) => cmd === "chat.decide")).toHaveLength(1);
  });

  it("arms Send for a staged picture alone and sends with no words", async () => {
    window.__MUSE_BOOT__ = snapshot([], {
      staged: [{ name: "pasted-2.png", width: 4, height: 5, bytes: 7 }],
    });
    let sent: { text?: string } | null = null;
    installChannel(
      (cmd) => (cmd === "chat.send" ? { ok: true } : { ok: true, ...snapshot([]) }),
      (request) => {
        if (request.cmd === "chat.send") sent = request as { text?: string };
      },
    );

    render(<Chat pollMs={10_000} />);

    // A picture alone sends, exactly as the app's own card allows.
    const send = screen.getByRole("button", { name: "Send" });
    expect(send).toBeEnabled();
    fireEvent.click(send);
    await waitFor(() => {
      expect(sent).not.toBeNull();
      expect(sent?.text).toBe("");
    });
  });

  it("shows the pending approvals and decides one through the app", async () => {
    window.__MUSE_BOOT__ = snapshot([], {
      approvals: {
        loaded: true,
        note: null,
        cards: [
          {
            id: "a-1",
            headline: "Reach example.com",
            summary: "The agent wants to fetch a page.",
            details: ["Host: example.com"],
            choices: [
              { word: "allow_once", label: "Allow once" },
              { word: "deny", label: "Deny" },
            ],
          },
        ],
      },
    });
    let decided: { approval_id?: string; decision?: string } | null = null;
    const commands = installChannel(
      (cmd) =>
        cmd === "chat.decide" || cmd === "chat.approvals.refresh"
          ? { ok: true }
          : { ok: true, ...snapshot([]) },
      (request) => {
        if (request.cmd === "chat.decide") {
          decided = request as { approval_id?: string; decision?: string };
        }
      },
    );

    render(<Chat pollMs={10_000} />);

    expect(screen.getByText("Reach example.com")).toBeInTheDocument();
    expect(screen.getByText("Host: example.com")).toBeInTheDocument();

    // One press, one decision, with the service's own word and the card's id --
    // and the channel's own `id` left alone, or the reply would never match.
    fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    await waitFor(() =>
      expect(decided).toMatchObject({ approval_id: "a-1", decision: "deny" }),
    );

    // "Check approvals" is the panel's own read.
    fireEvent.click(screen.getByRole("button", { name: "Check approvals" }));
    await waitFor(() => expect(commands).toContain("chat.approvals.refresh"));
  });

  it("keeps the card and says why when the app drops a decision press", async () => {
    const boot = snapshot([], {
      approvals: {
        loaded: true,
        note: null,
        cards: [
          {
            id: "a-1",
            headline: "Reach example.com",
            summary: "The agent wants to fetch a page.",
            details: ["Host: example.com"],
            choices: [{ word: "deny", label: "Deny" }],
          },
        ],
      },
    });
    window.__MUSE_BOOT__ = boot;
    installChannel((cmd) =>
      cmd === "chat.decide"
        ? {
            ok: false,
            error:
              "The approvals panel is still finishing the last press; try again in a moment.",
          }
        : { ok: true, ...boot },
    );

    render(<Chat pollMs={10_000} />);
    fireEvent.click(screen.getByRole("button", { name: "Deny" }));

    // The app's own sentence, beside the card that is still waiting on the
    // reader -- not an assumption that the press decided anything.
    const panel = screen.getByLabelText("Approvals");
    expect(
      await within(panel).findByText(/still finishing the last press/),
    ).toBeInTheDocument();
    expect(within(panel).getByText("Reach example.com")).toBeInTheDocument();
    expect(screen.queryByText(/^Decided/)).toBeNull();
  });

  it("says the panel's unlinked sentence instead of an empty list", async () => {
    window.__MUSE_BOOT__ = snapshot([], {
      approvals: { cards: [], loaded: true, note: "Link your account first." },
    });
    installChannel(() => ({ ok: true, ...snapshot([]) }));

    render(<Chat pollMs={10_000} />);

    expect(screen.getByText("Link your account first.")).toBeInTheDocument();
    expect(screen.queryByText("No pending approvals.")).toBeNull();
    expect(screen.queryByText("No approvals read yet.")).toBeNull();
  });

  it("badges the open thread with the listing's unread count", async () => {
    window.__MUSE_BOOT__ = snapshot([message({ id: "m-1", text: "hi" })], { unread: 3 });
    installChannel(() => ({ ok: true, ...snapshot([]) }));

    render(<Chat pollMs={10_000} />);

    expect(screen.getByLabelText("3 unread")).toHaveTextContent("3");
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
