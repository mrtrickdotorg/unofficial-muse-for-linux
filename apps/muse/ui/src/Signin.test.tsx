// The sign-in page's gate: the same channel the app speaks, the app's own
// labels, and one write per press.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Signin } from "./Signin";
import { installChannel } from "./testChannel";
import type { SigninSnapshot } from "./types";

const bootPayload = (overrides: Partial<SigninSnapshot> = {}): SigninSnapshot & {
  page: "signin";
} => ({
  page: "signin",
  python: "/home/phendrick/.local/share/muse/account-bridge/.venv/bin/python",
  folder: "/run/user/1002/muse-session",
  consent: false,
  status: "",
  error: "",
  browser_open: false,
  title: "Sign in",
  hint: "Point the app at the Python the bridge setup printed.",
  consent_sentence:
    "I consent to the app using the session this sign-in creates.",
  connect_label: "Connect",
  browser_label: "Sign in with browser",
  cancel_label: "Cancel",
  choose_label: "Choose folder…",
  ...overrides,
});

describe("the sign-in web page", () => {
  it("opens on the app's own defaults and labels", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Signin />);

    expect(screen.getByRole("heading", { name: "Sign in", level: 1 })).toBeInTheDocument();
    expect(
      screen.getByDisplayValue("/home/phendrick/.local/share/muse/account-bridge/.venv/bin/python"),
    ).toBeInTheDocument();
    expect(screen.getByDisplayValue("/run/user/1002/muse-session")).toBeInTheDocument();
    expect(screen.getByText(held.consent_sentence)).toBeInTheDocument();
    expect(commands[0]).toBe("signin.snapshot");
    // No browser flow running: Cancel is not offered, because there is nothing
    // for it to stop.
    expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
  });

  it("sends one press with the values on screen, then reads the answer", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const seen: Record<string, unknown>[] = [];
    const commands = installChannel(
      () => ({ ok: true, ...held }),
      (request) => seen.push(request),
    );

    render(<Signin />);

    fireEvent.change(screen.getByDisplayValue(held.python), {
      target: { value: "/usr/bin/python3" },
    });
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));

    await waitFor(() =>
      expect(seen.find((request) => request.cmd === "signin.press")).toMatchObject({
        press: "connect",
        python: "/usr/bin/python3",
        folder: held.folder,
        consent: true,
      }),
    );
    // One write, and then the page reads again: the snapshot is the app's
    // answer, not a state this page keeps for itself.
    expect(commands.filter((command) => command === "signin.press")).toHaveLength(1);
    expect(commands.filter((command) => command === "signin.snapshot")).toHaveLength(2);
  });

  it("chooses the session folder on the app's own sheet, never a native dialog", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const seen: Record<string, unknown>[] = [];
    installChannel(
      (command) =>
        command === "folders.list"
          ? {
              ok: true,
              path: held.folder,
              parent: "/run/user/1002",
              folders: [
                { name: "bridge", path: `${held.folder}/bridge` },
              ],
              note: null,
            }
          : { ok: true, ...held },
      (request) => seen.push(request),
    );

    render(<Signin />);

    fireEvent.click(screen.getByRole("button", { name: "Choose folder…" }));

    // The sheet is the page's own, standing on the folder the app reports.
    expect(
      await screen.findByRole("dialog", { name: "Choose a session folder" }),
    ).toBeInTheDocument();
    expect(seen.find((request) => request.cmd === "folders.list")).toMatchObject({
      path: held.folder,
    });

    // Stepping in is the app's read again, never a list the page kept.
    fireEvent.click(screen.getByRole("button", { name: "bridge" }));
    await waitFor(() =>
      expect(
        seen.filter((request) => request.cmd === "folders.list").map((r) => r.path),
      ).toContain(`${held.folder}/bridge`),
    );

    // Choosing is the app's own press, with the path the sheet stands in; the
    // page returns to the form with the app's field value.
    fireEvent.click(screen.getByRole("button", { name: "Use this folder" }));
    await waitFor(() =>
      expect(seen.find((request) => request.cmd === "signin.press")).toMatchObject({
        press: "choose",
        folder: held.folder,
      }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.getByDisplayValue(held.folder)).toBeInTheDocument();
  });

  it("offers Cancel only while the app's browser flow runs", async () => {
    const held = bootPayload({ browser_open: true, status: "Waiting for the browser…" });
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Signin />);

    expect(screen.getByText("Waiting for the browser…")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(commands).toContain("signin.press"));
  });

  it("prints the app's refusal instead of claiming a sign-in", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    installChannel((command) =>
      command === "signin.press"
        ? { ok: false, error: "Tick the consent box first." }
        : { ok: true, ...held },
    );

    render(<Signin />);

    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    expect(await screen.findByText(/Tick the consent box first\./)).toBeInTheDocument();
    // The page keeps what was typed: a refusal is not an answer to clear.
    expect(screen.getByDisplayValue(held.folder)).toBeInTheDocument();
  });
});
