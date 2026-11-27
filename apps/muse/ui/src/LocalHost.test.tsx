// S2's gate for the Local host page: the page's behaviour against the same
// channel the app speaks, with no window, no host and no account.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
      id: "a-1",
      operation: "create_text",
      path: "Scratch/notes.txt",
      target: null,
      preview: "hello from the agent",
      deciding: false,
      countdown: "Expires in 42s",
    },
  ],
  log: ["files.read_text Scratch/notes.txt → allowed", "node registered"],
  policy_on: false,
  policy_sentence:
    "Allow writes in the folders I grant without a per-write card: a saved permission, re-applied whenever I grant the same folder again. Turning this off, or Stop & revoke all, ends it.",
  approvals_head: "Pending file approvals · review the exact path and content before allowing",
  activity_head: "Session activity · commands and outcomes only",
  search_head: "Find in your folders",
  search: null,
  searching: false,
  search_summary:
    "No search yet. Names are matched inside the folders you granted, never outside them.",
  content_head: "Search inside your files",
  content: null,
  searching_content: false,
  content_summary:
    "No content search yet. Text is matched inside the folders you granted, in UTF-8 files only.",
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

  it("decides a card by the broker's id, never by its path", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const seen: Record<string, unknown>[] = [];
    installChannel(
      (command) => (command === "localhost.decide" ? { ok: true } : { ok: true, ...held }),
      (request) => seen.push(request),
    );

    render(<LocalHost />);

    expect(screen.getByText("create_text · Scratch/notes.txt")).toBeInTheDocument();
    expect(screen.getByText("hello from the agent")).toBeInTheDocument();
    expect(screen.getByText("Expires in 42s")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Allow once" }));
    await waitFor(() =>
      expect(seen.find((request) => request.cmd === "localhost.decide")).toMatchObject({
        approval_id: "a-1",
        allow: true,
      }),
    );
    // One press at a time: the card's buttons come back when the first write
    // has landed, and the second answer is the same command with allow false.
    const deny = screen.getByRole("button", { name: "Deny" });
    await waitFor(() => expect(deny).toBeEnabled());
    fireEvent.click(deny);
    await waitFor(() =>
      expect(
        seen.filter((request) => request.cmd === "localhost.decide").map((r) => r.allow),
      ).toEqual([true, false]),
    );
    expect(seen.every((request) => request.cmd !== "localhost.decide" || !("path" in request))).toBe(
      true,
    );
  });

  it("saves the write permission and clears the log", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const seen: Record<string, unknown>[] = [];
    installChannel((command) => ({ ok: true, ...held }), (request) => seen.push(request));

    render(<LocalHost />);

    const policy = screen.getByRole("checkbox", {
      name: /Allow writes in the folders I grant/,
    });
    expect(policy).not.toBeChecked();
    fireEvent.click(policy);
    await waitFor(() =>
      expect(seen.find((request) => request.cmd === "localhost.policy")).toMatchObject({
        allow: true,
      }),
    );

    const clear = screen.getByRole("button", { name: "Clear log" });
    await waitFor(() => expect(clear).toBeEnabled());
    fireEvent.click(clear);
    await waitFor(() =>
      expect(seen.some((request) => request.cmd === "localhost.clear_log")).toBe(true),
    );
  });

  it("names a move's destination when the card carries one", async () => {
    const held = bootPayload({
      pending: [
        {
          id: "a-2",
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
          id: "a-3",
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

  it("starts the host with the consent box's own word", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const seen: Record<string, unknown>[] = [];
    installChannel(
      (command) => (command === "localhost.start" ? { ok: true } : { ok: true, ...held }),
      (request) => seen.push(request),
    );

    render(<LocalHost />);

    fireEvent.click(screen.getByRole("button", { name: "Start local host" }));
    await waitFor(() =>
      expect(seen.find((request) => request.cmd === "localhost.start")).toMatchObject({
        consent: false,
      }),
    );

    // With the box checked the same press carries the consent; the page never
    // claims a permission the person did not give.
    const start = await screen.findByRole("button", { name: "Start local host" });
    fireEvent.click(screen.getByRole("checkbox", { name: /I explicitly enable/ }));
    fireEvent.click(start);
    await waitFor(() =>
      expect(
        seen.filter((request) => request.cmd === "localhost.start").map((r) => r.consent),
      ).toEqual([false, true]),
    );
  });

  it("keeps one press one write while a start is in flight", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(
      (command) => (command === "localhost.start" ? { ok: true } : { ok: true, ...held }),
      undefined,
      40,
    );

    render(<LocalHost />);

    fireEvent.click(screen.getByRole("button", { name: "Start local host" }));
    expect(screen.getByRole("button", { name: "Starting…" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Starting…" }));
    // The other boundary presses wait with it: the host is one boundary.
    expect(screen.getByRole("button", { name: "Stop & revoke all" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Grant a folder to Muse…" })).toBeDisabled();

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Start local host" })).toBeEnabled(),
    );
    expect(commands.filter((command) => command === "localhost.start")).toHaveLength(1);
  });

  it("stops and revokes, and drops the consent with the session", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(
      (command) => (command === "localhost.stop" ? { ok: true } : { ok: true, ...held }),
    );

    render(<LocalHost />);
    const consent = screen.getByRole("checkbox", { name: /I explicitly enable/ });
    fireEvent.click(consent);
    expect(consent).toBeChecked();

    fireEvent.click(screen.getByRole("button", { name: "Stop & revoke all" }));
    await waitFor(() => expect(commands).toContain("localhost.stop"));
    // The next start has to be consented to again, as the native page's own
    // stop unchecks the box.
    await waitFor(() => expect(consent).not.toBeChecked());
  });

  it("grants the folder chosen on the page's own sheet, with the read-only choice", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const seen: Record<string, unknown>[] = [];
    installChannel(
      (command) =>
        command === "folders.list"
          ? {
              ok: true,
              path: "/var/home/phendrick",
              parent: "/var/home",
              folders: [
                { name: "Scratch", path: "/var/home/phendrick/Scratch" },
              ],
              note: null,
            }
          : { ok: true, ...held },
      (request) => seen.push(request),
    );

    render(<LocalHost />);

    // The sheet stands where the page already has a grant, and the grant is the
    // app's own press with the folder it returned.
    fireEvent.click(screen.getByRole("button", { name: "Grant a folder to Muse…" }));
    expect(
      await screen.findByRole("dialog", { name: "Grant a folder to Muse" }),
    ).toBeInTheDocument();
    expect(seen.find((request) => request.cmd === "folders.list")).toMatchObject({
      path: held.grants[0].root,
    });
    fireEvent.click(screen.getByRole("button", { name: "Grant this folder" }));
    await waitFor(() =>
      expect(seen.find((request) => request.cmd === "localhost.grant")).toMatchObject({
        folder: "/var/home/phendrick",
        consent: false,
        writable: false,
      }),
    );

    // Read-only is the page's default, as the native check starts on; the
    // unchecked box grants read-write, and the consent travels with it. The
    // press is one at a time, so the button comes back before the second one.
    fireEvent.click(screen.getByRole("checkbox", { name: /I explicitly enable/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Grant read-only/ }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Grant a folder to Muse…" })).toBeEnabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "Grant a folder to Muse…" }));
    await screen.findByRole("dialog", { name: "Grant a folder to Muse" });
    fireEvent.click(screen.getByRole("button", { name: "Grant this folder" }));
    await waitFor(() =>
      expect(
        seen.filter((request) => request.cmd === "localhost.grant").map((r) => r.writable),
      ).toEqual([false, true]),
    );
  });

  it("prints the app's sentence when the host refuses a start", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    installChannel((command) =>
      command === "localhost.start"
        ? { ok: false, error: "Stop the host and explicitly consent before granting a folder." }
        : { ok: true, ...held },
    );

    render(<LocalHost />);

    fireEvent.click(screen.getByRole("button", { name: "Start local host" }));
    expect(
      await screen.findByText(
        /Command failed: Stop the host and explicitly consent before granting a folder\./,
      ),
    ).toBeInTheDocument();
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

// The filename search: the app's own walk over the granted folders, one
// sentence for its outcome, and a hit's text in place. The page never walks a
// folder itself and never invents a result.
describe("the Local host page's filename search", () => {
  it("shows the app's one sentence and opens a hit's text", async () => {
    const held = bootPayload({
      search: {
        query: "report",
        hits: [
          { folder: "Scratch", path: "alpha-report.md" },
          { folder: "Scratch", path: "sub/beta-report.md" },
        ],
        refused: [{ folder: "Notes", code: "io_denied" }],
        scanned: 1204,
        truncated: false,
      },
      search_summary:
        "2 match(es) for “report” in 1204 name(s) scanned. Not searched: Notes (io_denied).",
    });
    window.__MUSE_BOOT__ = held;
    const seen: Record<string, unknown>[] = [];
    installChannel(
      (cmd) =>
        cmd === "localhost.preview"
          ? { ok: true, folder: "Scratch", path: "alpha-report.md", content: "# the report\n" }
          : { ok: true, ...held },
      (request) => seen.push(request),
    );

    render(<LocalHost />);

    expect(screen.getByText(/2 match\(es\) for/)).toBeInTheDocument();
    expect(screen.getByText("Scratch · sub/beta-report.md")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Scratch · alpha-report.md"));
    await waitFor(() => expect(screen.getByRole("dialog")).toBeInTheDocument());
    expect(screen.getByText("# the report")).toBeInTheDocument();
    const asked = seen.find((request) => request.cmd === "localhost.preview");
    expect(asked).toMatchObject({ folder: "Scratch", path: "alpha-report.md" });
  });

  it("sends the query to the app and shows its refusal, not a blank list", async () => {
    const held = bootPayload({ search: null });
    window.__MUSE_BOOT__ = held;
    const seen: Record<string, unknown>[] = [];
    installChannel(
      (cmd) =>
        cmd === "localhost.search"
          ? { ok: false, error: "Grant a folder before searching it." }
          : { ok: true, ...held },
      (request) => seen.push(request),
    );

    render(<LocalHost />);

    fireEvent.change(screen.getByLabelText("Find a filename in your folders"), {
      target: { value: "notes" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Search" }));

    await waitFor(() =>
      expect(screen.getByText(/Grant a folder before searching it\./)).toBeInTheDocument(),
    );
    const asked = seen.find((request) => request.cmd === "localhost.search");
    expect(asked).toMatchObject({ query: "notes" });
  });
});

// The content search: the matching lines inside the granted files, with the
// line numbers the broker reported. A line opens the file it lives in through
// the same bounded read a filename hit uses; a refusal is a refusal.
describe("the Local host page's content search", () => {
  it("lists the matching lines and opens the file a line lives in", async () => {
    const held = bootPayload({
      content: {
        query: "revenue",
        hits: [
          { folder: "Scratch", path: "alpha-report.md", line: 2, text: "revenue up" },
          {
            folder: "Scratch",
            path: "sub/beta-report.md",
            line: 2,
            text: "second revenue line",
          },
        ],
        refused: [{ folder: "Notes", code: "invalid_utf8" }],
        scanned: 88,
        truncated: false,
      },
      content_summary:
        "2 matching line(s) for “revenue” in 88 name(s) scanned. Not searched: Notes (invalid_utf8).",
    });
    window.__MUSE_BOOT__ = held;
    const seen: Record<string, unknown>[] = [];
    installChannel(
      (cmd) =>
        cmd === "localhost.preview"
          ? {
              ok: true,
              folder: "Scratch",
              path: "alpha-report.md",
              content: "# Quarter report\nrevenue up\n",
            }
          : { ok: true, ...held },
      (request) => seen.push(request),
    );

    render(<LocalHost />);

    expect(screen.getByRole("heading", { name: "Search inside your files" })).toBeInTheDocument();
    expect(screen.getByText(/2 matching line\(s\) for/)).toBeInTheDocument();
    expect(
      screen.getByText("Scratch · sub/beta-report.md:2 · second revenue line"),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByText("Scratch · alpha-report.md:2 · revenue up"));
    await waitFor(() => expect(screen.getByRole("dialog")).toBeInTheDocument());
    expect(document.querySelector(".host-preview .preview")?.textContent).toContain(
      "revenue up",
    );
    const asked = seen.find((request) => request.cmd === "localhost.preview");
    expect(asked).toMatchObject({ folder: "Scratch", path: "alpha-report.md" });
  });

  it("sends the query to the app and shows its refusal, not an empty list", async () => {
    const held = bootPayload({ content: null });
    window.__MUSE_BOOT__ = held;
    const seen: Record<string, unknown>[] = [];
    installChannel(
      (cmd) =>
        cmd === "localhost.content"
          ? { ok: false, error: "Grant a folder before searching inside it." }
          : { ok: true, ...held },
      (request) => seen.push(request),
    );

    render(<LocalHost />);

    fireEvent.change(screen.getByLabelText("Search inside the files in your folders"), {
      target: { value: "revenue" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Search inside" }));

    await waitFor(() =>
      expect(screen.getByText(/Grant a folder before searching inside it\./)).toBeInTheDocument(),
    );
    const asked = seen.find((request) => request.cmd === "localhost.content");
    expect(asked).toMatchObject({ query: "revenue" });
  });
});
