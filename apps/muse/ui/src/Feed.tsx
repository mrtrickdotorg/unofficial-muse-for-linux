import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { call, channelAvailable } from "./api";
import { useHeldRead } from "./held";
import type { FeedCard as Card, FeedSnapshot } from "./types";

/// Elements that can carry or load code, a document, or a form: none of
/// them belong in a graphic, and removing the element removes its subtree
/// with it. `<template>` goes too — its content is inert while it sits in
/// a template, but it lives in a separate tree the attribute walk below
/// cannot see into, so it does not get to stay.
const WIDGET_BANNED_ELEMENTS =
  "script, iframe, object, embed, form, base, meta, link, title, frame, frameset, applet, xmp, noembed, noframes, template";

/// Attributes whose value is a URL the element dereferences or navigates
/// to: the places a `javascript:` (or kin) address can hide.
const WIDGET_URL_ATTRIBUTES = [
  "href",
  "src",
  "xlink:href",
  "action",
  "formaction",
  "poster",
];

/// True when a URL value names a script-bearing scheme. Browsers ignore
/// ASCII whitespace and controls inside a scheme (`java\tscript:` still
/// runs), so the probe strips them everywhere before comparing; the
/// attribute itself is left untouched unless the probe objects.
function isScriptUrl(value: string): boolean {
  const probe = value.replace(/[\u0000-\u0020]+/g, "").toLowerCase();
  return (
    probe.startsWith("javascript:") ||
    probe.startsWith("vbscript:") ||
    probe.startsWith("data:text/html")
  );
}

/// Parse service markup into a fragment with everything script-capable
/// removed: banned elements, every `on*` event-handler attribute, and
/// every script URL in a URL-bearing attribute. Parsing into a
/// `<template>` keeps the walk in fragment context — a leading `<style>`
/// stays with the graphic instead of being hoisted into a document head
/// the way a full-document parse would hoist it — and what survives is
/// what a graphic needs: elements, classes, the service's own `<style>`,
/// and inert attributes.
function sanitizeWidgetHtml(html: string): DocumentFragment {
  const template = document.createElement("template");
  template.innerHTML = html;
  const content = template.content;
  content
    .querySelectorAll(WIDGET_BANNED_ELEMENTS)
    .forEach((el) => el.remove());
  content.querySelectorAll("*").forEach((el) => {
    for (const attr of Array.from(el.attributes)) {
      const name = attr.name.toLowerCase();
      if (name.startsWith("on")) {
        el.removeAttribute(attr.name);
      } else if (
        WIDGET_URL_ATTRIBUTES.includes(name) &&
        isScriptUrl(attr.value)
      ) {
        el.removeAttribute(attr.name);
      }
    }
  });
  return content;
}

/// One graphic's markup, in a shadow root: the service's own `<style>` stays
/// inside the widget (no leak into this page), and link presses are
/// cancelled so a card cannot navigate the window away. The markup is
/// sanitized before it is inserted (see `sanitizeWidgetHtml`): a
/// `<script>` element set through `innerHTML` would not run, but inline
/// event handlers would — in this page's realm, where the command bridge
/// answers — so "does not execute" was never the whole promise. The
/// native frame keeps its promise with a caged WebKit view in another
/// process; this page has no second process, so the markup itself is
/// made inert instead.
function WidgetGraphic({ html }: { html: string }) {
  const host = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const node = host.current;
    if (!node) return;
    const root = node.shadowRoot ?? node.attachShadow({ mode: "open" });
    root.innerHTML = "";
    root.appendChild(sanitizeWidgetHtml(html));
    const onClick = (event: Event) => {
      const target = event.target as Element | null;
      if (target && typeof target.closest === "function" && target.closest("a")) {
        event.preventDefault();
      }
    };
    root.addEventListener("click", onClick);
    return () => root.removeEventListener("click", onClick);
  }, [html]);

  return <div className="feed-graphic" ref={host} />;
}

/// The card's hero. A document loaded from `about:blank` cannot read the
/// native cache's `file://` images, so the bytes travel through the command
/// channel as a data URI -- fetched only when the card asks, and never part
/// of the boot payload.
function Hero({ url }: { url: string }) {
  const [src, setSrc] = useState<string | null>(null);
  const [missing, setMissing] = useState(false);

  useEffect(() => {
    if (!url) return;
    if (!channelAvailable()) {
      setMissing(true);
      return;
    }
    let alive = true;
    call<{ data: string | null }>("feed.hero", 30_000, { url })
      .then((answer) => {
        if (!alive) return;
        if (answer.data) setSrc(answer.data);
        else setMissing(true);
      })
      .catch(() => {
        if (alive) setMissing(true);
      });
    return () => {
      alive = false;
    };
  }, [url]);

  if (missing) return null;
  if (!src) return <div className="feed-hero pending" aria-hidden="true" />;
  return <img className="feed-hero" src={src} alt="" />;
}

/// One feed card: the web's shape -- source icon, bold title, relative
/// time, body, hero, and the unit's graphic. Graphics render live only where
/// the native paint proved they load; everywhere else the service's
/// extracted text carries the card.
function FeedCard({ card }: { card: Card }) {
  return (
    <article className="feed-card">
      {card.meta && <div className="feed-meta">{card.meta}</div>}
      <div className="feed-head">
        <span className="feed-icon" aria-hidden="true">
          {card.icon}
        </span>
        <h3 className="feed-title">{card.title}</h3>
        {card.when && <span className="feed-when">{card.when}</span>}
      </div>
      {/* The native body markup: escaped text with its links already
          resolved on the Rust side, the same string the GTK label shows. */}
      {card.body && (
        <p className="feed-body" dangerouslySetInnerHTML={{ __html: card.body }} />
      )}
      {card.image_url && <Hero url={card.image_url} />}
      {card.live && card.widget_html ? (
        <WidgetGraphic html={card.widget_html} />
      ) : (
        card.widget_text && <p className="feed-graphic-text">{card.widget_text}</p>
      )}
      {/* A label, not a control: this page cannot run the service action,
          so it must not offer a button that does nothing. */}
      {card.action && (
        <span className="feed-action" title="Read-only in the web slice">
          <span className="feed-action-name">Action</span>
          <span className="feed-action-value">{card.action}</span>
        </span>
      )}
      {card.why && <div className="feed-why">{card.why}</div>}
    </article>
  );
}

export function Feed() {
  const boot = window.__MUSE_BOOT__;
  const [data, setData] = useState<FeedSnapshot | null>(
    boot && "cards" in boot ? boot : null,
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Opening the page costs nothing on the wire: the native side answers
  // from the cards it already painted.
  const loadSnapshot = useCallback(async (): Promise<FeedSnapshot | null> => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return null;
    }
    try {
      const answer = await call<FeedSnapshot>("feed.snapshot");
      setData(answer);
      setError(null);
      return answer;
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      return null;
    }
  }, []);

  // Refresh reads the feed again, which is what the label promises; the
  // account read can spend seconds on the feed listing.
  const refresh = useCallback(async () => {
    if (!channelAvailable()) {
      setError("The app's command channel is not available in this window.");
      return;
    }
    setBusy(true);
    try {
      setData(await call<FeedSnapshot>("feed.refresh", 180_000));
      setError(null);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  const reading = useHeldRead("feed", loadSnapshot, refresh);

  const cards = useMemo(() => data?.cards ?? [], [data]);
  const loaded = data?.loaded ?? false;

  // The native probe reads this: how many cards the page holds.
  (window as unknown as { __MUSE_ROWS__?: number }).__MUSE_ROWS__ = cards.length;

  const count =
    cards.length === 0
      ? ""
      : data && data.total > cards.length
        ? `${cards.length} of ${data.total} units`
        : `${cards.length} units`;

  return (
    <div className="page">
      <div className="top">
        <h1 className="title">Feed</h1>
        <span className="count">{count}</span>
        <button className="refresh" onClick={() => void refresh()} disabled={busy}>
          {busy ? "Refreshing…" : "Refresh"}
        </button>
      </div>

      {error && <div className="banner">Command failed: {error}</div>}
      {data?.error && <div className="banner">The last read failed: {data.error}</div>}

      {loaded && data?.prompt && (
        <section className="feed-prompt">
          <div className="feed-prompt-label">Your feed prompt</div>
          <p className="feed-prompt-text">{data.prompt}</p>
        </section>
      )}
      {loaded && data?.status && (
        <div className="feed-status">{data.status}</div>
      )}

      {!loaded ? (
        <div className="empty">
          {reading
            ? "Reading your feed now. It will appear here when the answer lands."
            : "The native Feed page has not loaded yet, so there is nothing to show here. Open Feed in the app, then press Refresh — this window is not claiming your feed is empty."}
        </div>
      ) : cards.length === 0 ? (
        <div className="empty">No units in this edition.</div>
      ) : (
        <div className="feed-cards">
          {cards.map((card, index) => (
            <FeedCard key={card.id || `card-${index}`} card={card} />
          ))}
        </div>
      )}

      <div className="foot">
        Web UI slice of the port: read-only, rendered from the same cards the
        native page holds.
      </div>
    </div>
  );
}
