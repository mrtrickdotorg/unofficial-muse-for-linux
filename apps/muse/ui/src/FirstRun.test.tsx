// The first-run page's gate: it shows the step's two lines, offers no press,
// and stops asking once the app says the setup returned.
import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { FirstRun } from "./FirstRun";
import { installChannel } from "./testChannel";

const running = {
  page: "firstrun" as const,
  head: "Setting up Muse",
  sub: "The app is preparing its folders. Nothing leaves this machine.",
  done: false,
};

describe("the first-run web page", () => {
  it("opens on the step and says in words that it has no presses", async () => {
    window.__MUSE_BOOT__ = running;
    const commands = installChannel(() => ({ ok: true, ...running }));

    render(<FirstRun />);

    expect(
      screen.getByRole("heading", { name: "Setting up Muse", level: 1 }),
    ).toBeInTheDocument();
    expect(screen.getByText(/Nothing leaves this machine/)).toBeInTheDocument();
    expect(screen.getByText("Working… this step has no presses.")).toBeInTheDocument();
    expect(screen.queryAllByRole("button")).toHaveLength(0);
    expect(commands[0]).toBe("firstrun.snapshot");
  });

  it("stops asking once the app's setup has returned", async () => {
    window.__MUSE_BOOT__ = running;
    const done = { ...running, done: true };
    let answers = 0;
    const commands = installChannel(() => {
      answers += 1;
      return answers < 2 ? { ok: true, ...running } : { ok: true, ...done };
    });

    render(<FirstRun />);

    expect(await screen.findByText("Done. The app window opens on its own.")).toBeInTheDocument();
    const asked = commands.filter((command) => command === "firstrun.snapshot").length;
    await new Promise((resolve) => setTimeout(resolve, 900));
    // A finished step is not a reason to keep polling: the count stands.
    expect(commands.filter((command) => command === "firstrun.snapshot").length).toBe(asked);
  });

  it("reports a refused read instead of an endless spinner", async () => {
    window.__MUSE_BOOT__ = undefined;
    installChannel(() => ({ ok: false, error: "the step's answer went away" }));

    render(<FirstRun />);

    await waitFor(() =>
      expect(screen.getByText(/Command failed: the step's answer went away/)).toBeInTheDocument(),
    );
  });
});
