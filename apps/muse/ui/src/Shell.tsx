import { useCallback, useEffect, useRef, useState, type ComponentType } from "react";
// The rail's icons are the native ones, inlined at build time: the same SVGs
// the GTK rail paints, so the two rails cannot drift apart while both exist.
import chatIcon from "../../assets/icons/chat.svg?raw";
import ideasIcon from "../../assets/icons/ideas.svg?raw";
import libraryIcon from "../../assets/icons/library.svg?raw";
import menuIcon from "../../assets/icons/menu.svg?raw";
import monitorIcon from "../../assets/icons/monitor.svg?raw";
import newIcon from "../../assets/icons/new.svg?raw";
import searchIcon from "../../assets/icons/search.svg?raw";
import shapesIcon from "../../assets/icons/shapes.svg?raw";
import tasksIcon from "../../assets/icons/tasks.svg?raw";
import { call, channelAvailable } from "./api";
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
import type { HistoryRow } from "./types";

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

function sentence(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

/// The web shell: the rail, the page list, the History list and one page's
/// content, in one window. It is the native window's shape without the native
/// window -- no page data lives here; each page asks its own snapshot over the
/// same channel, and History asks the panel's own listing, so the shell cannot
/// show anything other than what the app holds.
export function Shell() {
  const boot = window.__MUSE_BOOT__;
  const pages = boot && "pages" in boot ? boot.pages : [];
  const [current, setCurrent] = useState(pages[0]?.name ?? "chat");
  // History is the panel's listing. Null means it has not been read: an unread
  // list is not an empty account, and the section says which it is.
  const [history, setHistory] = useState<HistoryRow[] | null>(
    boot && "history" in boot ? boot.history : null,
  );
  const [historyNote, setHistoryNote] = useState<string | null>(null);
  const [opening, setOpening] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  // The words the Search page opens with: the drawer's entry only points the
  // page at them, and the page's own Run is what reads.
  const [query, setQuery] = useState("");
  const openingRef = useRef<string | null>(null);
  const rail = useRef<HTMLElement | null>(null);
  const drawer = useRef<HTMLElement | null>(null);
  const main = useRef<HTMLElement | null>(null);
  const searchBox = useRef<HTMLInputElement | null>(null);

  const loadHistory = useCallback(async () => {
    if (!channelAvailable()) {
      setHistoryNote("The app's command channel is not available in this window.");
      return;
    }
    try {
      const answer = await call<{ rows: HistoryRow[] }>("history.snapshot");
      setHistory(answer.rows ?? []);
      setHistoryNote(null);
    } catch (failure) {
      setHistoryNote(sentence(failure));
    }
  }, []);

  // Refilled on every page visit, the native sidebar's own rule: the listing's
  // open-thread mark and its unread counts move while the window is open.
  useEffect(() => {
    void loadHistory();
  }, [current, loadHistory]);

  // Opening a thread is the panel's own switch, and a fresh chat is that same
  // press with no id. One at a time: a second press while one is in flight
  // would change the thread twice for one intent.
  const openThread = useCallback(
    async (id: string | null) => {
      if (openingRef.current !== null) return;
      openingRef.current = id ?? "new";
      setOpening(id ?? "new");
      try {
        await call(id ? "history.open" : "chat.new", 15_000, id ? { thread: id } : undefined);
        setCurrent("chat");
        // The listing's open thread mark moved: re-read the panel's own rows.
        await loadHistory();
      } catch (failure) {
        setHistoryNote(sentence(failure));
      } finally {
        openingRef.current = null;
        setOpening(null);
      }
    },
    [loadHistory],
  );

  const runSearch = () => {
    setQuery(search.trim());
    setCurrent("search");
  };

  // The app's keyboard map, on the window so a press lands wherever focus is:
  // Ctrl+K reaches search, Ctrl+N a fresh thread, Ctrl+, settings, and F6 walks
  // the three panes this window has (Ctrl on the press is the browser's own
  // Meta on a Mac keyboard). Escape only leaves the search box: the shell has
  // no hidden drawer for it to fold.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const modifier = event.ctrlKey || event.metaKey;
      if (modifier && event.key.toLowerCase() === "k") {
        event.preventDefault();
        searchBox.current?.focus();
        return;
      }
      if (modifier && event.key.toLowerCase() === "n") {
        event.preventDefault();
        void openThread(null);
        return;
      }
      if (modifier && event.key === ",") {
        event.preventDefault();
        setCurrent("settings");
        return;
      }
      if (event.key === "F6") {
        event.preventDefault();
        const panes = [rail.current, drawer.current, main.current].filter(
          (pane): pane is HTMLElement => pane !== null,
        );
        if (panes.length === 0) return;
        const at = panes.indexOf(document.activeElement as HTMLElement);
        const step = event.shiftKey ? -1 : 1;
        panes[(at + step + panes.length) % panes.length]?.focus();
        return;
      }
      if (event.key === "Escape" && document.activeElement === searchBox.current) {
        event.preventDefault();
        searchBox.current?.blur();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openThread]);

  const renderPage = () => {
    if (current === "search") {
      // Keyed by the words, so the drawer's entry can hand the page a new
      // query without the page having to watch a prop.
      return <Search key={`search:${query}`} initialQuery={query} />;
    }
    const Page = COMPONENTS[current];
    if (Page) return <Page key={current} />;
    return (
      <div className="page">
        <div className="top">
          <h1 className="title">{pages.find((page) => page.name === current)?.title}</h1>
        </div>
        <p className="note">This page has no web view yet; the native window still carries it.</p>
      </div>
    );
  };

  return (
    <div className="shell">
      <nav ref={rail} className="shell-rail" aria-label="Pages" tabIndex={-1}>
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

      <aside ref={drawer} className="shell-drawer" aria-label="Drawer" tabIndex={-1}>
        <span className="shell-account">
          {boot && "account" in boot && boot.account ? boot.account : "Sign in"}
        </span>

        <div className="shell-tools">
          <form
            className="shell-search-form"
            role="search"
            onSubmit={(event) => {
              event.preventDefault();
              runSearch();
            }}
          >
            <input
              ref={searchBox}
              type="search"
              className="shell-search"
              aria-label="Search the account"
              placeholder="Search"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
          </form>
          <button
            type="button"
            className="shell-new"
            aria-label="New chat"
            title="New chat"
            disabled={opening !== null}
            onClick={() => void openThread(null)}
          >
            <span
              className="shell-new-glyph"
              aria-hidden="true"
              dangerouslySetInnerHTML={{ __html: newIcon }}
            />
          </button>
        </div>

        <nav className="shell-pages" aria-label="Page list">
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
        </nav>

        <h2 className="shell-section">History</h2>
        {historyNote && <p className="note shell-history-note">{historyNote}</p>}
        {history === null ? (
          <p className="note shell-history-note">
            {historyNote ? null : "The app's thread listing has not been read in this window."}
          </p>
        ) : history.length === 0 ? (
          <p className="note shell-history-note">
            No threads in the app's listing yet.
          </p>
        ) : (
          <div className="shell-history">
            {history.map((row) => (
              <button
                key={row.id}
                type="button"
                className={`shell-history-row${row.current ? " current" : ""}`}
                aria-current={row.current ? "true" : undefined}
                disabled={opening !== null}
                onClick={() => void openThread(row.id)}
              >
                {row.text}
              </button>
            ))}
          </div>
        )}

        <span className="shell-build">{boot && "build" in boot ? boot.build : ""}</span>
        <span className="shell-drawer-note">
          The same pages the native window carries, in one window of their own.
        </span>
      </aside>

      <main ref={main} className="shell-main" tabIndex={-1}>
        {renderPage()}
      </main>
    </div>
  );
}
