// S2's gate for the Local host page: the page's behaviour against the same
// channel the app speaks, with no window, no host and no account.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { LocalHost } from "./LocalHost";
import { installChannel } from "./testChannel";
import type { LocalHostSnapshot } from "./types";

const bootPayload = (
  overrides: Partial<LocalHostSnapshot> = {},
): LocalHostSnapshot & { page: "localhost" } => ({
  page: "localhost",
  identity: "This machine registers as its own device; nothing leaves it until you start the host.",
  phase: "registered",
  notice: "Listening for the account's own node commands.",
  device: "linux-0123456789abcdef",
  grants: [
    {
      label: "Scratch",
      root: "/var/home/phendrick/Scratch",
      writable: true,
      policy: false,
      text: "Scratch · /var/home/phendrick/Scratch · read-write",
    },
  ],
  pending: [
    {
      operation: "create_text",
      path: "Scratch/notes.txt",
      target: null,
      preview: "hello from the agent",
      deciding: false,
      countdown: "Expires in 42s",
    },
  ],
  log: ["files.read_text Scratch/notes.txt → allowed", "node registered"],
  ...overrides,
});

const grantRows = () => document.querySelectorAll(".grant-row").length;
const approvalCards = () => document.querySelectorAll(".approval-card").length;

describe("the Local host web page", () => {
  it("opens on the read model the controller last painted", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<LocalHost />);

    expect(screen.getByRole("heading", { name: "Local host", level: 1 })).toBeInTheDocument();
    expect(screen.getByText("registered with your account")).toBeInTheDocument();
    expect(screen.getByText(/registers as its own device/)).toBeInTheDocument();
    expect(screen.getByText("Device linux-0123456789abcdef")).toBeInTheDocument();
    expect(screen.getByText("Listening for the account's own node commands.")).toBeInTheDocument();
    // The grant's line is the native helper's own wording, not a second copy.
    expect(
      screen.getByText("Scratch · /var/home/phendrick/Scratch · read-write"),
    ).toBeInTheDocument();
    expect(grantRows()).toBe(1);
    expect(approvalCards()).toBe(1);
    expect(screen.getByText("files.read_text Scratch/notes.txt → allowed")).toBeInTheDocument();
    expect(commands[0]).toBe("localhost.snapshot");
  });

  it("names the empty states instead of a blank page", async () => {
    const held = bootPayload({ grants: [], pending: [], log: [], device: null });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<LocalHost />);

    expect(screen.getByText("No folders granted.")).toBeInTheDocument();
    expect(screen.getByText("No pending approvals.")).toBeInTheDocument();
    expect(screen.getByText("Nothing yet.")).toBeInTheDocument();
    expect(screen.queryByText(/^Device /)).not.toBeInTheDocument();
    expect(grantRows()).toBe(0);
  });

  it("shows a card's exact path and content and leaves the decision to the app", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<LocalHost />);

    expect(screen.getByText("create_text · Scratch/notes.txt")).toBeInTheDocument();
    expect(screen.getByText("hello from the agent")).toBeInTheDocument();
    expect(screen.getByText(/Expires in 42s · Allow once and Deny stay in the app\./)).toBeInTheDocument();
    // No press is offered: deciding here would bypass the native card's own
    // one-time approval, which is the whole boundary.
    expect(screen.queryByRole("button", { name: /allow once/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /deny/i })).not.toBeInTheDocument();
  });

  it("names a move's destination when the card carries one", async () => {
    const held = bootPayload({
      pending: [
        {
          operation: "rename",
          path: "Scratch/notes.txt",
          target: "Archive/notes.txt",
          preview: null,
          deciding: true,
          countdown: "Expires in 12s",
        },
      ],
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<LocalHost />);

    expect(screen.getByText("rename · Scratch/notes.txt")).toBeInTheDocument();
    expect(screen.getByText("→ Archive/notes.txt")).toBeInTheDocument();
    expect(screen.getByText(/Deciding in the app…/)).toBeInTheDocument();
  });

  it("follows the host's own pump: a card that arrives later appears", async () => {
    const first = bootPayload({ pending: [], log: [] });
    const second = bootPayload({
      pending: [
        {
          operation: "edit_text",
          path: "Scratch/notes.txt",
          target: null,
          preview: "the edited text",
          deciding: false,
          countdown: "Expires in 85s",
        },
      ],
      log: ["files.edit_text Scratch/notes.txt → awaiting your Allow"],
    });
    window.__MUSE_BOOT__ = undefined;
    let answers = 0;
    installChannel(() => {
      answers += 1;
      return { ok: true, ...(answers === 1 ? first : second) };
    });

    render(<LocalHost />);

    expect(await screen.findByText("No pending approvals.")).toBeInTheDocument();
    // The page re-asks on its own: the native paint projects the new card, and
    // the page shows it without a reload.
    expect(
      await screen.findByText("edit_text · Scratch/notes.txt", {}, { timeout: 3000 }),
    ).toBeInTheDocument();
    expect(screen.getByText("the edited text")).toBeInTheDocument();
    expect(
      screen.getByText("files.edit_text Scratch/notes.txt → awaiting your Allow"),
    ).toBeInTheDocument();
    expect(approvalCards()).toBe(1);
  });

  it("reports a refused snapshot instead of painting a stale host", async () => {
    window.__MUSE_BOOT__ = undefined;
    installChannel(() => ({ ok: false, error: "the host controller is busy" }));

    render(<LocalHost />);

    expect(
      await screen.findByText(/Command failed: the host controller is busy/),
    ).toBeInTheDocument();
    expect(screen.getByText("No folders granted.")).toBeInTheDocument();
  });
});
