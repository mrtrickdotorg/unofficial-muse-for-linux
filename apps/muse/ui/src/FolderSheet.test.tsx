// The folder sheet's own gate: one level at a time, and the create press that
// makes the folder a person has not got yet. The channel is the app's, stubbed.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { FolderSheet } from "./FolderSheet";
import { installChannel } from "./testChannel";
import type { FolderListing } from "./types";

const listing = (overrides: Partial<FolderListing> = {}): FolderListing => ({
  path: "/home/person/muse",
  parent: "/home/person",
  folders: [{ name: "existing", path: "/home/person/muse/existing" }],
  ...overrides,
});

const noop = () => "";

describe("the folder sheet", () => {
  it("lists the folder it stands in and answers the one chosen", async () => {
    const chosen: string[] = [];
    const seen: Record<string, unknown>[] = [];
    installChannel(
      (command) =>
        command === "folders.list" ? { ok: true, ...listing() } : { ok: true },
      (request) => seen.push(request),
    );
    render(
      <FolderSheet
        title="Choose a session folder"
        start="/home/person/muse"
        confirm="Use this folder"
        onChoose={(path) => chosen.push(path)}
        onCancel={noop}
      />,
    );
    await waitFor(() => expect(screen.getByText("existing")).toBeTruthy());
    expect(seen).toEqual([
      { id: 1, cmd: "folders.list", path: "/home/person/muse" },
    ]);
    fireEvent.click(screen.getByText("Use this folder"));
    expect(chosen).toEqual(["/home/person/muse"]);
  });

  it("creates a folder in the one it stands in and steps into it", async () => {
    const seen: Record<string, unknown>[] = [];
    installChannel(
      (command) =>
        command === "folders.create"
          ? { ok: true, path: "/home/person/muse/parity-session" }
          : { ok: true, ...listing() },
      (request) => seen.push(request),
    );
    render(
      <FolderSheet
        title="Choose a session folder"
        start="/home/person/muse"
        confirm="Use this folder"
        onChoose={noop}
        onCancel={noop}
      />,
    );
    await waitFor(() => expect(screen.getByText("existing")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("New folder name"), {
      target: { value: "parity-session" },
    });
    fireEvent.click(screen.getByText("Create folder"));
    await waitFor(() =>
      expect(
        seen.filter((request) => request.cmd === "folders.create"),
      ).toEqual([
        {
          id: expect.any(Number),
          cmd: "folders.create",
          path: "/home/person/muse",
          name: "parity-session",
        },
      ]),
    );
    // The sheet re-reads what it made, so the new folder is where it now is.
    await waitFor(() =>
      expect(
        seen.filter((request) => request.cmd === "folders.list").slice(-1)[0],
      ).toEqual({
        id: expect.any(Number),
        cmd: "folders.list",
        path: "/home/person/muse/parity-session",
      }),
    );
    // The name field is cleared by the same press that made the folder.
    expect(
      (screen.getByLabelText("New folder name") as HTMLInputElement).value,
    ).toBe("");
  });

  it("says why a folder could not be made, without leaving the sheet", async () => {
    installChannel((command) =>
      command === "folders.create"
        ? { ok: false, error: "A folder named existing is already in this folder." }
        : { ok: true, ...listing() },
    );
    render(
      <FolderSheet
        title="Choose a session folder"
        start="/home/person/muse"
        confirm="Use this folder"
        onChoose={noop}
        onCancel={noop}
      />,
    );
    await waitFor(() => expect(screen.getByText("existing")).toBeTruthy());
    fireEvent.change(screen.getByLabelText("New folder name"), {
      target: { value: "existing" },
    });
    fireEvent.click(screen.getByText("Create folder"));
    await waitFor(() =>
      expect(
        screen.getByText(
          "Command failed: A folder named existing is already in this folder.",
        ),
      ).toBeTruthy(),
    );
  });
});
