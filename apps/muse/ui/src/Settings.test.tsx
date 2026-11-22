// S2's gate for the Settings page: the page's behaviour against the same
// channel the app speaks, with no window and no account.
import { act, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Settings } from "./Settings";
import { installChannel } from "./testChannel";
import type { SettingsSnapshot } from "./types";

const bootPayload = (overrides: Partial<SettingsSnapshot> = {}): SettingsSnapshot & {
  page: "settings";
} => ({
  page: "settings",
  link: {
    linked: true,
    interpreter: "/home/phendrick/.local/share/muse/account-bridge/.venv/bin/python",
    session_folder: "/run/user/1002/muse-session",
  },
  updates: {
    appimage: true,
    status: "",
    busy: false,
    checked: false,
    staged: false,
  },
  policy: {
    loaded: true,
    rows: [
      { label: "connector_default", value: "always_ask", flip: "auto_allow" },
      { label: "network_default", value: "auto_allow", flip: "always_ask" },
    ],
    connectors: ["calendar"],
    withheld: 2,
    note: null,
  },
  ...overrides,
});

const policyRows = () => document.querySelectorAll(".policy-row").length;

describe("the Settings web page", () => {
  it("opens on the link, updater and policy the native pages hold", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Settings />);

    expect(screen.getByRole("heading", { name: "Settings", level: 1 })).toBeInTheDocument();
    expect(
      screen.getByText("/home/phendrick/.local/share/muse/account-bridge/.venv/bin/python"),
    ).toBeInTheDocument();
    expect(screen.getByText("/run/user/1002/muse-session")).toBeInTheDocument();
    expect(
      screen.getByText("connector_default"),
    ).toBeInTheDocument();
    expect(screen.getByText("always_ask")).toBeInTheDocument();
    expect(screen.getByText("· set to auto_allow in the app")).toBeInTheDocument();
    expect(screen.getByText("2 setting(s) the service withheld.")).toBeInTheDocument();
    // Two settings rows plus the connector row the panel names.
    expect(policyRows()).toBe(3);
    expect(commands).toEqual(["settings.snapshot"]);
  });

  it("says where updates come from and never offers the presses", async () => {
    const held = bootPayload();
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Settings />);

    expect(
      screen.getByText("Updates come from the rolling AppImage release."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /check for updates/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /sign out/i })).not.toBeInTheDocument();
  });

  it("names a staged update and the restart it is waiting for", async () => {
    const held = bootPayload({
      updates: { appimage: true, status: "", busy: false, checked: true, staged: true },
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Settings />);

    expect(
      screen.getByText(
        "Updates come from the rolling AppImage release. An update is downloaded; restart from the native page to take it.",
      ),
    ).toBeInTheDocument();
  });

  it("keeps the updater's own words when it has a status", async () => {
    const held = bootPayload({
      updates: {
        appimage: false,
        status: "Up to date (build 62ce6e3).",
        busy: false,
        checked: true,
        staged: false,
      },
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Settings />);

    expect(screen.getByText("Up to date (build 62ce6e3).")).toBeInTheDocument();
  });

  it("does not read an unlinked clone as a failed one", async () => {
    const held = bootPayload({
      link: { linked: false, interpreter: "", session_folder: "" },
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Settings />);

    expect(screen.getByText(/No saved link\. Sign in from the native page/)).toBeInTheDocument();
    expect(screen.queryByText("Interpreter")).not.toBeInTheDocument();
  });

  it("names an unread policy instead of claiming there is none", async () => {
    const held = bootPayload({
      policy: { loaded: false, rows: [], connectors: [], withheld: 0, note: null },
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Settings />);

    expect(
      screen.getByText(/has not read policy settings in this session/),
    ).toBeInTheDocument();
    expect(screen.queryByText("No policy settings.")).not.toBeInTheDocument();
    expect(policyRows()).toBe(0);
  });

  it("re-asks when the app's paint says the policy landed", async () => {
    window.__MUSE_BOOT__ = undefined;
    const unread = bootPayload({
      policy: { loaded: false, rows: [], connectors: [], withheld: 0, note: null },
    });
    const loaded = bootPayload();
    let answers = 0;
    const commands = installChannel(() => {
      answers += 1;
      return answers === 1 ? { ok: true, ...unread } : { ok: true, ...loaded };
    });

    render(<Settings />);

    expect(await screen.findByText(/has not read policy settings/)).toBeInTheDocument();
    // One read on open: an unread policy is a state to show, not a reason to
    // poll, so the page registers the hook the app's paint calls instead.
    await waitFor(() => expect(typeof window.musePolicyReady).toBe("function"));
    expect(commands.filter((command) => command === "settings.snapshot")).toHaveLength(1);

    // The chat panel's read lands; the native paint says so, and only now does
    // the page ask again -- no timer decided this.
    act(() => window.musePolicyReady?.());
    expect(await screen.findByText("connector_default")).toBeInTheDocument();
    expect(screen.queryByText(/has not read policy settings/)).not.toBeInTheDocument();
    expect(commands.filter((command) => command === "settings.snapshot")).toHaveLength(2);
  });

  it("takes its nudge hook away when the window closes", async () => {
    window.__MUSE_BOOT__ = undefined;
    installChannel(() => ({ ok: true, ...bootPayload() }));

    const { unmount } = render(<Settings />);
    await waitFor(() => expect(typeof window.musePolicyReady).toBe("function"));

    unmount();
    // A closed page leaves nothing for a later paint to call.
    expect(window.musePolicyReady).toBeUndefined();
  });

  it("reports the policy read's own failure rather than an empty panel", async () => {
    const held = bootPayload({
      policy: {
        loaded: false,
        rows: [],
        connectors: [],
        withheld: 0,
        note: "Link your account first.",
      },
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Settings />);

    expect(screen.getByText("Link your account first.")).toBeInTheDocument();
  });

  it("reports a refused snapshot instead of painting a stale one", async () => {
    window.__MUSE_BOOT__ = undefined;
    installChannel(() => ({ ok: false, error: "the settings read went away" }));

    render(<Settings />);

    expect(
      await screen.findByText(/Command failed: the settings read went away/),
    ).toBeInTheDocument();
    expect(screen.queryByText("Interpreter")).not.toBeInTheDocument();
  });
});
