// S2's gate for the Feed page: the page's behaviour against the same
// channel the app speaks, with no window and no service.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Feed } from "./Feed";
import { installChannel } from "./testChannel";
import type { FeedCard, FeedSnapshot } from "./types";

const card = (overrides: Partial<FeedCard> = {}): FeedCard => ({
  id: "u-1",
  icon: "🧭",
  meta: "Research",
  title: "A unit title",
  when: "2h",
  body: "A readable body.",
  action: "",
  why: "",
  image_url: "",
  widget_html: "",
  widget_text: "",
  live: false,
  ...overrides,
});

const bootPayload = (overrides: Partial<FeedSnapshot> = {}) => ({
  page: "feed" as const,
  cards: [] as FeedCard[],
  prompt: "",
  status: "",
  total: 0,
  loaded: true,
  error: null,
  ...overrides,
});

const cardsOnPage = () => document.querySelectorAll(".feed-card").length;

describe("the Feed web page", () => {
  it("opens on the held cards under the prompt and the native wording", async () => {
    const held = bootPayload({
      cards: [
        card({ id: "u-1", title: "A unit title", meta: "Research", action: "Open" }),
        card({ id: "u-2", title: "Second unit", icon: "📰", meta: "", when: "1d" }),
      ],
      prompt: "Verdant architecture",
      status: "Generating a fresh edition…",
      total: 2,
    });
    window.__MUSE_BOOT__ = held;
    const commands = installChannel(() => ({ ok: true, ...held }));

    render(<Feed />);

    expect(screen.getByRole("heading", { name: "Feed", level: 1 })).toBeInTheDocument();
    expect(screen.getByText("Your feed prompt")).toBeInTheDocument();
    expect(screen.getByText("Verdant architecture")).toBeInTheDocument();
    expect(screen.getByText("Generating a fresh edition…")).toBeInTheDocument();
    expect(screen.getByText("A unit title")).toBeInTheDocument();
    expect(screen.getByText("Second unit")).toBeInTheDocument();
    expect(screen.getByText("Research")).toBeInTheDocument();
    expect(screen.getByText("2h")).toBeInTheDocument();
    expect(screen.getByText("Open")).toBeInTheDocument();
    expect(screen.getByText("2 units")).toBeInTheDocument();
    expect(cardsOnPage()).toBe(2);
    expect(commands).toEqual(["feed.snapshot"]);
  });

  it("renders a live graphic in a shadow root and the rest as extracted text", () => {
    const held = bootPayload({
      cards: [
        card({
          id: "u-1",
          title: "With a graphic",
          widget_html: "<b>graphic</b>",
          widget_text: "graphic text",
          live: true,
        }),
        card({
          id: "u-2",
          title: "Markup that cannot render",
          widget_html: "<b>graphic</b>",
          widget_text: "extracted words",
          live: false,
        }),
      ],
      total: 2,
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Feed />);

    const hosts = document.querySelectorAll(".feed-graphic");
    expect(hosts).toHaveLength(1);
    const shadow = (hosts[0] as HTMLElement).shadowRoot;
    expect(shadow).not.toBeNull();
    // The service's markup and its own styles live in the shadow tree, so
    // neither can reach this page's DOM or stylesheet.
    expect(shadow?.innerHTML).toContain("<b>graphic</b>");
    expect(document.body.innerHTML).not.toContain("<b>graphic</b>");
    // The non-live card keeps the service's extracted words, as native does.
    expect(screen.getByText("extracted words")).toBeInTheDocument();
    expect(screen.queryByText("graphic text")).not.toBeInTheDocument();
  });

  it("renders the native body markup, links included", () => {
    const held = bootPayload({
      cards: [
        card({
          id: "u-1",
          body: 'Read <a href="https://example.test/a">the advisory</a> now.',
        }),
      ],
      total: 1,
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Feed />);

    const link = screen.getByRole("link", { name: "the advisory" });
    expect(link).toHaveAttribute("href", "https://example.test/a");
  });

  it("asks the channel for a hero and hangs the data URI it answers", async () => {
    const held = bootPayload({
      cards: [
        card({ id: "u-1", title: "With a hero", image_url: "https://example.test/hero.jpg" }),
      ],
      total: 1,
    });
    window.__MUSE_BOOT__ = held;
    const commands = installChannel((command) =>
      command === "feed.hero"
        ? { ok: true, data: "data:image/png;base64,iVBORw0KGgo=" }
        : { ok: true, ...held },
    );

    render(<Feed />);

    await waitFor(() =>
      expect(document.querySelector("img.feed-hero")).not.toBeNull(),
    );
    const hero = document.querySelector("img.feed-hero") as HTMLImageElement;
    expect(hero.getAttribute("src")).toBe("data:image/png;base64,iVBORw0KGgo=");
    // The boot card mounts its hero before the snapshot lands, so only the
    // set of commands matters here, not their order.
    expect([...commands].sort()).toEqual(["feed.hero", "feed.snapshot"]);
  });

  it("names the not-loaded state instead of an empty feed", async () => {
    const commands = installChannel(() => ({ ok: true, ...bootPayload({ loaded: false }) }));

    render(<Feed />);

    expect(await screen.findByText(/has not loaded yet/)).toBeInTheDocument();
    expect(screen.queryByText(/No units in this edition/)).not.toBeInTheDocument();
    expect(commands).toEqual(["feed.snapshot"]);
  });

  it("refreshes over the wire and repaints the cards the channel answers", async () => {
    const held = bootPayload({
      cards: [card({ id: "u-1", title: "Held card" })],
      total: 1,
    });
    window.__MUSE_BOOT__ = held;
    const fresh = {
      ...held,
      cards: [card({ id: "u-2", title: "Read from the service" }), card({ id: "u-3", title: "Second fresh" })],
      total: 4,
    };
    const commands = installChannel((command) =>
      command === "feed.refresh" ? { ok: true, ...fresh } : { ok: true, ...held },
    );

    render(<Feed />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByText("Read from the service")).toBeInTheDocument();
    expect(screen.getByText("Second fresh")).toBeInTheDocument();
    expect(screen.queryByText("Held card")).not.toBeInTheDocument();
    expect(screen.getByText("2 of 4 units")).toBeInTheDocument();
    expect(commands).toEqual(["feed.snapshot", "feed.refresh"]);
  });

  it("keeps the held cards and says so when a refresh is refused", async () => {
    const held = bootPayload({
      cards: [card({ id: "u-1", title: "Held card" })],
      total: 1,
    });
    window.__MUSE_BOOT__ = held;
    installChannel((command) =>
      command === "feed.refresh" ? { ok: true, ...held } : { ok: true, ...held },
    );

    render(<Feed />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Refresh" })).toBeEnabled(),
    );
    expect(screen.getByText("Held card")).toBeInTheDocument();
  });

  it("reports the last read's failure beside the cards it kept", async () => {
    const held = bootPayload({
      cards: [card({ id: "u-1", title: "Held card" })],
      total: 1,
      error: "the gateway went away",
    });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Feed />);

    expect(screen.getByText(/The last read failed: the gateway went away/)).toBeInTheDocument();
    expect(screen.getByText("Held card")).toBeInTheDocument();
  });

  it("opens an empty edition as empty, not as a failed read", async () => {
    const held = bootPayload({ loaded: true, total: 0 });
    window.__MUSE_BOOT__ = held;
    installChannel(() => ({ ok: true, ...held }));

    render(<Feed />);

    expect(await screen.findByText("No units in this edition.")).toBeInTheDocument();
    expect(screen.queryByText(/has not loaded yet/)).not.toBeInTheDocument();
  });
});
