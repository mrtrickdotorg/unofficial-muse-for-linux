import { useState, type ComponentType } from "react";
// The rail's icons are the native ones, inlined at build time: the same SVGs
// the GTK rail paints, so the two rails cannot drift apart while both exist.
import chatIcon from "../../assets/icons/chat.svg?raw";
import ideasIcon from "../../assets/icons/ideas.svg?raw";
import libraryIcon from "../../assets/icons/library.svg?raw";
import menuIcon from "../../assets/icons/menu.svg?raw";
import monitorIcon from "../../assets/icons/monitor.svg?raw";
import searchIcon from "../../assets/icons/search.svg?raw";
import shapesIcon from "../../assets/icons/shapes.svg?raw";
import tasksIcon from "../../assets/icons/tasks.svg?raw";
import { Chat } from "./Chat";
import { Feed } from "./Feed";
import { Goals } from "./Goals";
import { Ideas } from "./Ideas";
import { Library } from "./Library";
import { LocalHost } from "./LocalHost";
import { Search } from "./Search";
import { Settings } from "./Settings";
import { Tasks } from "./Tasks";
import { Threads } from "./Threads";

/// The page components the shell can show, by the name the app's page list
/// uses. A name without a component still gets its rail button -- the shell
/// says the page is missing rather than hiding it.
const COMPONENTS: Record<string, ComponentType> = {
  chat: Chat,
  threads: Threads,
  tasks: Tasks,
  library: Library,
  feed: Feed,
  search: Search,
  ideas: Ideas,
  goals: Goals,
  settings: Settings,
  localhost: LocalHost,
};

/// Each page's rail icon: the native `rail_glyph` set where it has one, and a
/// sensible sibling for the pages the native rail does not carry.
const ICONS: Record<string, string> = {
  chat: chatIcon,
  threads: libraryIcon,
  tasks: tasksIcon,
  library: shapesIcon,
  feed: libraryIcon,
  search: searchIcon,
  ideas: ideasIcon,
  goals: tasksIcon,
  settings: menuIcon,
  localhost: monitorIcon,
};

/// The web shell: the rail, the page list and one page's content, in one
/// window. It is the native window's shape without the native window -- no
/// page data lives here; each page asks its own snapshot over the same
/// channel, so the shell cannot show a page anything other than what the app
/// holds.
export function Shell() {
  const boot = window.__MUSE_BOOT__;
  const pages = boot && "pages" in boot ? boot.pages : [];
  const [current, setCurrent] = useState(pages[0]?.name ?? "chat");
  const Page = COMPONENTS[current];

  return (
    <div className="shell">
      <nav className="shell-rail" aria-label="Pages">
        {pages.map((page) => (
          <button
            key={page.name}
            type="button"
            className={`shell-rail-page${page.name === current ? " current" : ""}`}
            aria-label={page.title}
            aria-current={page.name === current ? "page" : undefined}
            title={page.title}
            onClick={() => setCurrent(page.name)}
          >
            <span
              className="shell-rail-glyph"
              aria-hidden="true"
              // Static, bundled markup: the app's own icon files, never input.
              dangerouslySetInnerHTML={{ __html: ICONS[page.name] ?? "" }}
            />
          </button>
        ))}
        <span className="shell-rail-spacer" />
      </nav>

      <aside className="shell-drawer" aria-label="Drawer">
        <span className="shell-account">
          {boot && "account" in boot && boot.account ? boot.account : "Sign in"}
        </span>
        {pages.map((page) => (
          <button
            key={page.name}
            type="button"
            className={`shell-drawer-page${page.name === current ? " current" : ""}`}
            aria-current={page.name === current ? "page" : undefined}
            onClick={() => setCurrent(page.name)}
          >
            {page.title}
          </button>
        ))}
        <span className="shell-build">{boot && "build" in boot ? boot.build : ""}</span>
        <span className="shell-drawer-note">
          The same pages the native window carries, in one window of their own.
        </span>
      </aside>

      <main className="shell-main">
        {Page ? (
          <Page key={current} />
        ) : (
          <div className="page">
            <div className="top">
              <h1 className="title">{pages.find((page) => page.name === current)?.title}</h1>
            </div>
            <p className="note">
              This page has no web view yet; the native window still carries it.
            </p>
          </div>
        )}
      </main>
    </div>
  );
}
