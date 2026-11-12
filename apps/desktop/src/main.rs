use gtk::prelude::*;
use gtk4 as gtk;
mod account_login;
mod account_ui;
mod build_identity;
mod calendar_ui;
pub mod computer_control;
mod computer_control_ui;
mod desktop_ui;
mod feed_ui;
mod files_write_ui;
mod library_ui;
mod local_host;
mod mail_ui;
mod rich_text;
mod service_ui;
mod tasks_ui;
mod updater;
use muse_agent_runtime::{AgentResponse, AgentSession, ToolPolicy};
use muse_linux_core::filesystem::{FileEntry, FilesystemGrant};
use muse_meta_api::MetaModelClient;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

const APP_ID: &str = "org.kythos.UnofficialMuseForLinux";

type SharedGrant = Rc<RefCell<Option<FilesystemGrant>>>;
type SharedDirectory = Rc<RefCell<PathBuf>>;
type SharedApiKey = Rc<RefCell<Option<String>>>;
type SharedSession = Rc<RefCell<AgentSession>>;

enum AgentUiMessage {
    Progress(String),
    Finished(AgentSession, Result<AgentResponse, String>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChatRole {
    User,
    Assistant,
    Notice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BubbleAlignment {
    Start,
    End,
}

fn bubble_alignment(role: ChatRole) -> BubbleAlignment {
    match role {
        ChatRole::User => BubbleAlignment::End,
        ChatRole::Assistant | ChatRole::Notice => BubbleAlignment::Start,
    }
}

const DARK_THEME_CSS: &str = r#"
/* Visual language follows the Mac/Windows Muse client: near-black neutral
   surfaces, hairline borders instead of boxes, one quiet accent, generous
   padding, and assistant replies as plain text rather than bubbles.
   Sizing is calibrated from measurement, not web CSS values: in
   Patrick's Nov 11 side-by-side screenshots the web client's text lines
   measured 24 device px tall against the app's 17 (the browser applies
   his display scaling, the app renders at nominal px), so the theme
   was scaled by that measured factor (~1.4 over the first pass). His
   verdict that evening — "it's nice to have it larger, but it's a
   little too large" — stepped it back one notch to ~1.2 over the
   first pass (base text 21px), proportions unchanged. His eye is the
   calibration instrument; do not "correct" these back to nominal. */
window, .app-shell { background-color: #0d0d10; color: #f4f4f6; font-family: "Inter", "Segoe UI", "Cantarell", "Noto Sans", system-ui, sans-serif; font-size: 21px; }
window.dark-window { background-color: #0d0d10; }
scrolledwindow viewport { background-color: transparent; }
.app-sidebar { background-color: #131318; }
.icon-rail { background-color: #131318; border-right: 1px solid rgba(255, 255, 255, 0.07); padding: 15px 12px; }
.side-panel-wrap { background-color: #131318; border-right: 1px solid rgba(255, 255, 255, 0.07); padding: 19px 17px; }
.side-panel { padding: 0 2px; }
.side-panel-right { background: rgba(255, 255, 255, 0.03); border-left: 1px solid rgba(255, 255, 255, 0.08); padding: 12px; }
.panel-list { background: transparent; }
.panel-list row { padding: 13px 17px; border-radius: 12px; background: transparent; color: #d7d7dd; }
.panel-list row:hover { background-color: rgba(255, 255, 255, 0.05); }
.panel-list row:selected { background-color: rgba(255, 255, 255, 0.10); color: #ffffff; }
.panel-item-label { font-size: 21px; }
.panel-heading { color: #9a9aa5; font-size: 17px; font-weight: 600; padding: 19px 15px 7px; }
.panel-title { color: #f4f4f6; font-size: 21px; font-weight: 700; padding: 3px 6px 6px; }
.panel-copy { color: #b3b3bc; font-size: 17px; padding: 0 6px; }
.search-pill { background-color: #26262d; border: 1px solid rgba(255, 255, 255, 0.06); border-radius: 27px; min-height: 53px; padding: 0 17px; font-size: 20px; color: #f4f4f6; }
.search-pill:focus { border-color: rgba(255, 255, 255, 0.25); }
.rail-separator { background-color: rgba(255, 255, 255, 0.08); min-height: 1px; margin: 6px 8px; }
.brand-mark { background-image: linear-gradient(135deg, #5b8cff, #8b5cf6); color: #ffffff; border-radius: 13px; font-weight: 700; min-width: 48px; min-height: 48px; }
.nav-item { min-width: 67px; min-height: 67px; padding: 0; border-radius: 17px; background: transparent; color: #a7a7b1; border-color: transparent; box-shadow: none; }
.nav-item:hover { background-color: rgba(255, 255, 255, 0.05); color: #f4f4f6; }
.nav-item.active { background-color: rgba(255, 255, 255, 0.09); color: #ffffff; }
.nav-item:focus { outline-width: 2px; outline-style: solid; outline-color: rgba(255, 255, 255, 0.35); outline-offset: 1px; }
.nav-icon { min-width: 34px; }
.rail-button { min-width: 63px; min-height: 63px; padding: 0; border-radius: 31px; background: transparent; color: #a7a7b1; border-color: transparent; transition: background-color 120ms ease-in-out; }
.rail-button:hover { background-color: rgba(255, 255, 255, 0.05); color: #ffffff; }
.rail-button.active { background-color: rgba(255, 255, 255, 0.09); color: #ffffff; }
.rail-button:focus { outline-width: 2px; outline-style: solid; outline-color: rgba(255, 255, 255, 0.35); outline-offset: 1px; }
.main-column { background-color: #0d0d10; }
.topbar { padding: 15px 27px; border-bottom: 1px solid rgba(255, 255, 255, 0.07); }
.topbar-button { min-width: 48px; min-height: 48px; padding: 0; border-radius: 24px; background: transparent; border-color: transparent; box-shadow: none; }
.topbar-button:hover { background-color: rgba(255, 255, 255, 0.06); }
.page-title { background-color: transparent; border-radius: 0; padding: 6px 3px; font-size: 21px; font-weight: 600; color: #f4f4f6; }
.status-chip { color: #a7a7b1; background-color: rgba(255, 255, 255, 0.04); border: 1px solid rgba(255, 255, 255, 0.07); border-radius: 18px; padding: 7px 15px; font-size: 16px; }
.page-content { padding: 31px 39px; }
.page-heading { font-size: 29px; font-weight: 700; color: #f4f4f6; padding-bottom: 2px; }
.page-subtitle { color: #b3b3bc; }
.chat-column { }
.chat-scroll { background: transparent; }
.assistant-identity { padding: 0 0 6px; }
.assistant-avatar { background-image: linear-gradient(135deg, #5b8cff, #8b5cf6); color: #ffffff; border-radius: 27px; min-width: 53px; min-height: 53px; font-weight: 700; }
.message-avatar { background-color: #26262d; color: #d7d7de; border-radius: 21px; min-width: 43px; min-height: 43px; font-weight: 700; margin-top: 3px; }
.assistant-name { font-weight: 600; }
.message-meta { color: #8e8e99; font-size: 15px; padding: 3px 8px; }
.day-divider { color: #6f6f7a; font-size: smaller; padding: 8px 0 2px; }
.section-heading { font-weight: 600; font-size: 19px; color: #f4f4f6; padding-top: 11px; }
.title-2 { font-weight: 600; font-size: 19px; color: #f4f4f6; }
.settings-disclosure { background-color: #17171c; border: 1px solid rgba(255, 255, 255, 0.07); border-radius: 17px; padding: 12px 17px; }
.welcome-card { background-image: linear-gradient(180deg, #1e1e28, #17171d); border: 1px solid rgba(255, 255, 255, 0.08); border-radius: 19px; padding: 24px; }
.welcome-title { color: #f4f4f6; font-size: 30px; font-weight: 700; }
.welcome-copy { color: #c9c9d1; }
.settings-content { padding: 10px 2px 4px; }
.toolbar { padding: 2px 0 0; }
.toolbar button { font-size: 17px; min-height: 39px; padding: 4px 13px; }
.toolbar button:not(.destructive-action):not(.suggested-action):not(.primary-action) { background-color: transparent; background-image: none; border-color: transparent; box-shadow: none; }
.toolbar button:not(.destructive-action):not(.suggested-action):not(.primary-action):hover { background-color: rgba(255, 255, 255, 0.06); }
.chat-bubble { padding: 15px 21px; border-radius: 21px; font-size: 21px; }
.chat-bubble.user { background-color: #0a84ff; color: #ffffff; }
.chat-bubble.user .message-meta { color: #dbeeff; }
.chat-bubble.assistant { background-color: transparent; color: #ececf1; padding-left: 2px; padding-right: 2px; }
.chat-bubble.notice { background-color: #2e2716; color: #f4d99b; }
.chat-status { color: #b3b3bc; padding: 2px 6px; }
.code-block { background-color: #101014; border: 1px solid rgba(255, 255, 255, 0.07); border-radius: 12px; padding: 13px 15px; font-family: "JetBrains Mono", "Cascadia Code", "DejaVu Sans Mono", monospace; font-size: 17px; color: #d7d7dd; }
.quote-block { border-left: 4px solid rgba(255, 255, 255, 0.22); padding-left: 15px; color: #c9c9d1; }
selection { background-color: rgba(110, 168, 255, 0.35); color: #ffffff; }
.composer { background-color: #17171c; border: 1px solid rgba(255, 255, 255, 0.10); border-radius: 29px; padding: 9px 9px 9px 17px; }
.composer:focus-within { border-color: rgba(255, 255, 255, 0.28); }
.composer entry { background: transparent; border: 0; box-shadow: none; font-size: 21px; }
.composer button.suggested-action { border-radius: 25px; padding: 9px 21px; font-weight: 600; }
.composer-pill { background-color: #1e1e24; }
.send-round { min-width: 48px; min-height: 48px; padding: 0; border-radius: 24px; background-color: #f4f4f6; background-image: none; color: #131316; font-size: 23px; font-weight: 700; border-color: transparent; box-shadow: none; }
.send-round:hover { background-color: #ffffff; }
.send-round:disabled { background-color: rgba(255, 255, 255, 0.14); color: rgba(244, 244, 246, 0.45); }
entry, searchentry, textview { color: #f4f4f6; background-color: #17171c; border: 1px solid rgba(255, 255, 255, 0.09); border-radius: 12px; padding: 9px 15px; font-size: 19px; }
entry:focus, searchentry:focus, textview:focus { border-color: rgba(255, 255, 255, 0.28); }
button { color: #e9e9ee; background-color: #1d1d23; border: 1px solid rgba(255, 255, 255, 0.09); border-radius: 12px; padding: 9px 17px; box-shadow: none; font-size: 18px; }
button:hover { background-color: #26262e; }
button.suggested-action, button.primary-action { color: #101014; background-color: #f4f4f6; background-image: none; border-color: transparent; }
button.suggested-action:hover, button.primary-action:hover { background-color: #ffffff; background-image: none; }
button.destructive-action { color: #ffb4ab; background-color: #332024; border-color: rgba(255, 180, 171, 0.25); }
button.destructive-action:hover { background-color: #42262a; }
.dim-label { color: #b3b3bc; }
.build-identity { color: #6f6f7a; font-size: 15px; padding: 14px 8px 3px; }
notebook header { background-color: #0d0d10; border-bottom: 1px solid rgba(255, 255, 255, 0.07); }
notebook tab { color: #a7a7b1; background-color: transparent; border-radius: 9px; padding: 11px 17px; font-size: 19px; }
notebook tab:checked { color: #f4f4f6; background-color: rgba(255, 255, 255, 0.08); }
notebook stack { background-color: #0d0d10; }
combobox box, combobox cellview { color: #f4f4f6; }
popover contents { background-color: #1a1a20; border: 1px solid rgba(255, 255, 255, 0.09); border-radius: 12px; }
popover modelbutton { color: #e9e9ee; font-size: 19px; padding: 7px 12px; }
popover modelbutton:hover { background-color: rgba(255, 255, 255, 0.06); }
list { background-color: transparent; }
list row { color: #e9e9ee; padding: 9px 12px; border-radius: 9px; }
list row:selected { background-color: rgba(255, 255, 255, 0.09); }
.card { color: #c9c9d1; background-color: #17171c; border: 1px solid rgba(255, 255, 255, 0.07); border-radius: 17px; padding: 19px; }
.feed-title { font-size: 21px; font-weight: 700; color: #f4f4f6; }
.feed-icon { font-size: 30px; }
.feed-image { border-radius: 14px; background-color: #101014; }
.feed-widget { background-color: #101014; border: 1px solid rgba(255, 255, 255, 0.07); border-radius: 14px; padding: 14px; }
.approval-card { border-left: 3px solid #c98a2b; }
.approval-icon { color: #d8b25c; font-weight: 700; }
.approval-heading { font-weight: 700; color: #f4f4f6; }
.approval-details { background-color: #101014; border: 1px solid rgba(255, 255, 255, 0.07); border-radius: 10px; padding: 8px; }
.approval-details textview { background: transparent; border: 0; }
.approval-scroll { }
.approval-countdown { color: #d8b25c; }
.approval-actions { }
.file-row { border-radius: 10px; }
notebook { background-color: transparent; }
notebook > header { background-color: transparent; border-bottom: 1px solid rgba(255, 255, 255, 0.07); padding: 4px; }
notebook > header > tabs > tab { background-color: transparent; color: #a7a7b1; border-radius: 10px; padding: 9px 15px; border: 1px solid transparent; }
notebook > header > tabs > tab:hover { background-color: rgba(255, 255, 255, 0.05); color: #f4f4f6; }
notebook > header > tabs > tab:checked { background-color: rgba(255, 255, 255, 0.09); color: #ffffff; border-color: transparent; }
notebook > stack { background-color: transparent; padding-top: 12px; }
scrollbar slider { background-color: rgba(255, 255, 255, 0.16); border-radius: 8px; min-width: 8px; min-height: 8px; }
tooltip { background-color: #26262e; color: #f4f4f6; border: 1px solid rgba(255, 255, 255, 0.12); border-radius: 8px; padding: 6px 10px; }
checkbutton { color: #d7d7de; padding: 2px; }
"#;

/// Rail glyphs drawn to match muse.ai's (bubble, magnifier, book, bulb,
/// check-square, shapes, bars, plus, download tray, menu), shipped as
/// strings in the binary: the system icon theme cannot be trusted with
/// these names -- on Patrick's KDE the theme names fell back to generic
/// document and picture glyphs (his Nov 10 side-by-side screenshot). The
/// SVGs are written once into the app's cache dir and registered as
/// unthemed icons, so the theme can never substitute a different glyph.
/// Fixed light-gray stroke: the rail button's highlight pill carries the
/// active state, as on the web.
const BUNDLED_ICONS: &[(&str, &str)] = &[
    (
        "umfl-chat",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#d9d9e0\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"M12 4.4c4.6 0 8.2 3 8.2 6.8 0 3.8-3.6 6.8-8.2 6.8-1 0-1.9-.14-2.8-.4L5 19.7l.66-2.5C4.55 15.9 3.8 13.7 3.8 11.2 3.8 7.4 7.4 4.4 12 4.4Z\"/></svg>",
    ),
    (
        "umfl-search",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#d9d9e0\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><circle cx=\"11\" cy=\"11\" r=\"6.4\"/><path d=\"M15.9 15.9l4.3 4.3\"/></svg>",
    ),
    (
        "umfl-library",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#d9d9e0\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"M12 6.4C10.2 5 7.4 4.5 4.6 5v13.6c2.8-.5 5.6 0 7.4 1.4 1.8-1.4 4.6-1.9 7.4-1.4V5C16.6 4.5 13.8 5 12 6.4Z\"/><path d=\"M12 6.4v13.6\"/></svg>",
    ),
    (
        "umfl-ideas",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#d9d9e0\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"M12 3.9a5.7 5.7 0 0 1 3.25 10.4c-.6.44-.87.97-.87 1.85H9.62c0-.88-.27-1.41-.87-1.85A5.7 5.7 0 0 1 12 3.9Z\"/><path d=\"M9.7 19.3h4.6\"/><path d=\"M10.4 21.5h3.2\"/></svg>",
    ),
    (
        "umfl-goals",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#d9d9e0\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><rect x=\"4.2\" y=\"4.2\" width=\"15.6\" height=\"15.6\" rx=\"3.6\"/><path d=\"M8.4 12.5l2.5 2.5 4.8-5.3\"/></svg>",
    ),
    (
        "umfl-create",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#d9d9e0\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><circle cx=\"9\" cy=\"9\" r=\"4.5\"/><rect x=\"12.9\" y=\"12.9\" width=\"6.8\" height=\"6.8\" rx=\"1.2\"/><path d=\"M16.9 3.8l2.7 4.7h-5.4z\"/></svg>",
    ),
    (
        "umfl-feed",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><rect x=\"5.6\" y=\"13\" width=\"3\" height=\"7\" rx=\"1\" fill=\"#d9d9e0\"/><rect x=\"10.5\" y=\"4.5\" width=\"3\" height=\"15.5\" rx=\"1\" fill=\"#d9d9e0\"/><rect x=\"15.4\" y=\"9.5\" width=\"3\" height=\"10.5\" rx=\"1\" fill=\"#d9d9e0\"/></svg>",
    ),
    (
        "umfl-new",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#d9d9e0\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"M12 5.5v13M5.5 12h13\"/></svg>",
    ),
    (
        "umfl-download",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#d9d9e0\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"M12 4.2v10.4\"/><path d=\"M6.9 10.3L12 15.4l5.1-5.1\"/><path d=\"M4.6 19.6h14.8\"/></svg>",
    ),
    (
        "umfl-menu",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#d9d9e0\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"M4.6 7h14.8M4.6 12h14.8M4.6 17h14.8\"/></svg>",
    ),
];

fn install_bundled_icons() {
    let dir = gtk::glib::user_cache_dir()
        .join("unofficial-muse-for-linux")
        .join("icons");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (name, svg) in BUNDLED_ICONS {
        let path = dir.join(format!("{name}.svg"));
        let stale = std::fs::read_to_string(&path)
            .map(|existing| existing != *svg)
            .unwrap_or(true);
        if stale {
            let _ = std::fs::write(&path, svg);
        }
    }
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(&dir);
    }
}

/// Inter (SIL Open Font License 1.1, license shipped alongside in
/// assets/fonts/Inter-LICENSE.txt) is the face the theme's font stack
/// names first, matching the web client's typography — but it is not
/// installed on most systems, so the stack silently fell back to whatever
/// sans the distro chose (Noto Sans on Patrick's KDE). The four static
/// faces the UI uses are bundled and written into the user's fontconfig
/// font directory before any widget exists, so the first Pango font map
/// already sees them. Best-effort like the bundled icons: a failed write
/// just leaves the fallback stack in charge.
const BUNDLED_FONTS: &[(&str, &[u8])] = &[
    (
        "Inter-Regular.ttf",
        include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    ),
    (
        "Inter-Medium.ttf",
        include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    ),
    (
        "Inter-SemiBold.ttf",
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    ),
    (
        "Inter-Bold.ttf",
        include_bytes!("../assets/fonts/Inter-Bold.ttf"),
    ),
];

fn install_bundled_fonts() {
    let dir = gtk::glib::user_data_dir().join("fonts");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (name, bytes) in BUNDLED_FONTS {
        let path = dir.join(name);
        let stale = std::fs::read(&path)
            .map(|existing| existing != *bytes)
            .unwrap_or(true);
        if stale {
            let _ = std::fs::write(&path, bytes);
        }
    }
}

fn install_dark_theme() {
    install_bundled_fonts();
    install_bundled_icons();
    if let Some(display) = gtk::gdk::Display::default() {
        // Unstyled widgets (notebook tabs, combo popups, menus) must also
        // default to the dark variant: without a settings daemon they would
        // render light-on-dark, e.g. white strips behind button rows.
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_application_prefer_dark_theme(true);
        }
        let provider = gtk::CssProvider::new();
        provider.load_from_data(DARK_THEME_CSS);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

#[derive(Clone)]
struct DialogLease<T> {
    active: Rc<RefCell<Option<T>>>,
}

impl<T> Default for DialogLease<T> {
    fn default() -> Self {
        Self {
            active: Rc::new(RefCell::new(None)),
        }
    }
}

impl<T> DialogLease<T> {
    fn keep(&self, dialog: T) {
        *self.active.borrow_mut() = Some(dialog);
    }

    fn is_active(&self) -> bool {
        self.active.borrow().is_some()
    }

    fn weak(&self) -> std::rc::Weak<RefCell<Option<T>>> {
        Rc::downgrade(&self.active)
    }
}

fn main() {
    // WebKit's bubblewrap sandbox cannot run inside an AppImage, so
    // from one, opt out before any WebView can exist. The feed's
    // widget frames stay navigation-locked either way; outside an
    // AppImage the sandbox stands.
    if std::env::var_os("APPIMAGE").is_some() {
        // SAFETY: this runs before the application object exists —
        // no other thread has started, so nothing can be reading
        // the environment concurrently.
        unsafe {
            std::env::set_var("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS", "1");
        }
    }
    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run();
}

/// Default window size. Kept inside a 1366x768 laptop work area: a larger
/// default opened partially off-screen and forced a maximize cycle.
/// Bounds are enforced at compile time on every build, not just tests.
const DEFAULT_WINDOW_WIDTH: i32 = 1100;
const DEFAULT_WINDOW_HEIGHT: i32 = 700;
const DEFAULT_WINDOW_SIZE_PIN: () = {
    assert!(DEFAULT_WINDOW_WIDTH >= 900);
    assert!(DEFAULT_WINDOW_WIDTH <= 1150);
    assert!(DEFAULT_WINDOW_HEIGHT >= 600);
    assert!(DEFAULT_WINDOW_HEIGHT <= 750);
};

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

enum UpdateMsg {
    Checked(Result<(updater::ReleaseInfo, bool), String>),
    Progress(u64, u64),
    Installed(Result<(), String>),
}

struct UpdateUi {
    release: Option<updater::ReleaseInfo>,
    available: bool,
    busy: bool,
    installed: bool,
    /// A download is in flight, and when its last progress landed —
    /// the pair the drain needs to tell a slow download from a
    /// stalled one instead of sitting silent at a full-looking bar.
    downloading: bool,
    last_progress: Option<std::time::Instant>,
}

/// The self-update dialog behind the rail's Download button: check,
/// download with progress, install in place, restart. All network and
/// file work runs on worker threads; this window only ever renders
/// their reports.
fn show_update_window(parent: &gtk::ApplicationWindow, target: std::path::PathBuf) {
    let window = gtk::Window::builder()
        .title("App updates")
        .transient_for(parent)
        .modal(true)
        .default_width(480)
        .default_height(300)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let heading = gtk::Label::new(Some("Update Unofficial Muse for Linux"));
    heading.add_css_class("panel-title");
    heading.set_halign(gtk::Align::Start);
    content.append(&heading);
    let current = gtk::Label::new(Some(&format!("This build: {}", build_identity::label())));
    current.add_css_class("panel-copy");
    current.set_halign(gtk::Align::Start);
    current.set_wrap(true);
    content.append(&current);
    let status = gtk::Label::new(Some(
        "The app can update itself in place: the new build replaces this \
         file, and your sign-in and settings are untouched — no fresh \
         download to find, and no signing in again.",
    ));
    status.add_css_class("dim-label");
    status.set_halign(gtk::Align::Start);
    status.set_wrap(true);
    content.append(&status);
    let progress = gtk::ProgressBar::new();
    progress.set_visible(false);
    content.append(&progress);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let check_button = gtk::Button::with_label("Check for updates");
    let install_button = gtk::Button::with_label("Download and install");
    install_button.set_sensitive(false);
    let restart_button = gtk::Button::with_label("Restart the app");
    restart_button.set_sensitive(false);
    let close_button = gtk::Button::with_label("Close");
    buttons.append(&check_button);
    buttons.append(&install_button);
    buttons.append(&restart_button);
    buttons.append(&close_button);
    content.append(&buttons);
    window.set_child(Some(&content));

    let state = std::rc::Rc::new(std::cell::RefCell::new(UpdateUi {
        release: None,
        available: false,
        busy: false,
        installed: false,
        downloading: false,
        last_progress: None,
    }));
    let (sender, receiver) = std::sync::mpsc::channel::<UpdateMsg>();
    let alive = std::rc::Rc::new(std::cell::Cell::new(true));
    {
        let alive = alive.clone();
        window.connect_destroy(move |_| alive.set(false));
    }
    {
        let state = state.clone();
        let status = status.clone();
        let progress = progress.clone();
        let check_button = check_button.clone();
        let install_button = install_button.clone();
        let restart_button = restart_button.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
            if !alive.get() {
                return gtk::glib::ControlFlow::Break;
            }
            let mut latest_progress: Option<(u64, u64)> = None;
            let mut settled = false;
            while let Ok(msg) = receiver.try_recv() {
                match msg {
                    UpdateMsg::Progress(done, total) => {
                        latest_progress = Some((done, total));
                        state.borrow_mut().last_progress = Some(std::time::Instant::now());
                    }
                    UpdateMsg::Checked(result) => {
                        settled = true;
                        let mut state = state.borrow_mut();
                        state.busy = false;
                        progress.set_visible(false);
                        check_button.set_sensitive(true);
                        match result {
                            Ok((release, available)) => {
                                state.available = available;
                                state.release = Some(release.clone());
                                if available {
                                    status.set_text(&format!(
                                        "A newer build is available (uploaded {}). \
                                         Downloading replaces this app's file in place.",
                                        build_identity::utc_stamp(release.updated_epoch)
                                    ));
                                    install_button.set_sensitive(true);
                                } else {
                                    status.set_text("You're on the latest build.");
                                }
                            }
                            Err(error) => status.set_text(&error),
                        }
                    }
                    UpdateMsg::Installed(result) => {
                        settled = true;
                        let mut state = state.borrow_mut();
                        state.busy = false;
                        state.downloading = false;
                        progress.set_visible(false);
                        match result {
                            Ok(()) => {
                                state.installed = true;
                                status.set_text(
                                    "Update installed. Restart the app to use it — \
                                     your sign-in carries over.",
                                );
                                restart_button.set_sensitive(true);
                                install_button.set_sensitive(false);
                            }
                            Err(error) => {
                                status.set_text(&error);
                                install_button.set_sensitive(state.available);
                            }
                        }
                    }
                }
            }
            // A terminal message in this batch owns the status line:
            // a progress report queued behind it must not paint the
            // "downloading" text back over "Update installed".
            if !settled {
                let (downloading, last_progress) = {
                    let state = state.borrow();
                    (state.downloading, state.last_progress)
                };
                let stalled = downloading
                    && last_progress
                        .map(|at| at.elapsed() > Duration::from_secs(45))
                        .unwrap_or(false);
                if stalled {
                    status.set_text(
                        "No download data for a while — the connection looks stalled. \
                         Closing this window is safe: nothing is installed until a \
                         download finishes and verifies, and you can try again.",
                    );
                } else if let Some((done, total)) = latest_progress
                    && total > 0
                {
                    progress.set_fraction(done as f64 / total as f64);
                    status.set_text(&format!(
                        "Downloading the update… {:.1} of {:.1} MB",
                        done as f64 / 1_000_000.0,
                        total as f64 / 1_000_000.0
                    ));
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }
    {
        let state = state.clone();
        let sender = sender.clone();
        let status = status.clone();
        let progress = progress.clone();
        let check_button = check_button.clone();
        let inner = check_button.clone();
        check_button.connect_clicked(move |_| {
            if state.borrow().busy {
                return;
            }
            state.borrow_mut().busy = true;
            inner.set_sensitive(false);
            progress.set_visible(true);
            progress.pulse();
            status.set_text("Checking GitHub for the latest build…");
            let sender = sender.clone();
            std::thread::spawn(move || {
                let result = updater::fetch_latest().map(|release| {
                    let available = updater::update_available(
                        build_identity::commit(),
                        build_identity::built_epoch(),
                        &release,
                    );
                    (release, available)
                });
                updater::note_check_done(now_epoch());
                let _ = sender.send(UpdateMsg::Checked(result));
            });
        });
    }
    {
        let state = state.clone();
        let sender = sender.clone();
        let status = status.clone();
        let progress = progress.clone();
        let install_button = install_button.clone();
        let target = target.clone();
        let inner = install_button.clone();
        install_button.connect_clicked(move |_| {
            let release = {
                let mut state = state.borrow_mut();
                if state.busy || state.installed {
                    return;
                }
                let Some(release) = state.release.clone() else {
                    return;
                };
                state.busy = true;
                state.downloading = true;
                state.last_progress = Some(std::time::Instant::now());
                release
            };
            inner.set_sensitive(false);
            progress.set_visible(true);
            progress.set_fraction(0.0);
            status.set_text("Downloading the update…");
            let sender = sender.clone();
            let target = target.clone();
            std::thread::spawn(move || {
                let progress_sender = sender.clone();
                let mut report = move |done: u64, total: u64| {
                    let _ = progress_sender.send(UpdateMsg::Progress(done, total));
                };
                let result = updater::download_and_install(&release, &target, &mut report);
                let _ = sender.send(UpdateMsg::Installed(result));
            });
        });
    }
    {
        let target = target.clone();
        let status = status.clone();
        restart_button.connect_clicked(move |_| match updater::relaunch(&target) {
            Ok(()) => std::process::exit(0),
            Err(error) => status.set_text(&error),
        });
    }
    let window_for_close = window.clone();
    close_button.connect_clicked(move |_| {
        window_for_close.close();
    });
    window.present();
    // Open ready to answer: a check starts with the dialog.
    check_button.emit_clicked();
}

fn build_ui(app: &gtk::Application) {
    const _: () = DEFAULT_WINDOW_SIZE_PIN;
    install_dark_theme();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Unofficial Muse for Linux")
        .default_width(DEFAULT_WINDOW_WIDTH)
        .default_height(DEFAULT_WINDOW_HEIGHT)
        .build();
    window.add_css_class("dark-window");

    let grant: SharedGrant = Rc::new(RefCell::new(None));
    let current_directory: SharedDirectory = Rc::new(RefCell::new(PathBuf::new()));
    let active_dialog = DialogLease::<gtk::FileChooserNative>::default();
    let api_key: SharedApiKey = Rc::new(RefCell::new(None));
    let session: SharedSession = Rc::new(RefCell::new(AgentSession::new()));
    let agent_busy = Rc::new(Cell::new(false));

    // A paned shell makes the rail drag-resizable instead of a fixed 196px
    // column that eats the chat area on small windows.
    let shell = gtk::Paned::new(gtk::Orientation::Horizontal);
    shell.add_css_class("app-shell");
    shell.set_position(372);
    shell.set_resize_start_child(false);
    shell.set_shrink_start_child(false);
    shell.set_wide_handle(false);

    // muse.ai-shaped shell: a slim icon rail for the sections, then a
    // sidebar panel whose content follows the selected section — for
    // Chat, the conversation list itself (Main chat + Side chats).
    let sidebar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    sidebar.add_css_class("app-sidebar");
    // The rail and panel keep their own widths; the split must not
    // stretch them back out.
    sidebar.set_hexpand(false);

    let rail = gtk::Box::new(gtk::Orientation::Vertical, 6);
    rail.add_css_class("icon-rail");
    rail.set_size_request(64, -1);
    let chats_nav = nav_button("umfl-chat", "Chat", "Main chat and your side chats");
    chats_nav.add_css_class("active");
    rail.append(&chats_nav);
    let search_button = nav_button("umfl-search", "Search", "Search your chats");
    rail.append(&search_button);
    let library_nav = nav_button("umfl-library", "Library", "Your spaces and saved artifacts");
    rail.append(&library_nav);
    let ideas_nav = nav_button("umfl-ideas", "Ideas", "Ideas your Muse saved for you");
    rail.append(&ideas_nav);
    let goals_nav = nav_button(
        "umfl-goals",
        "Goals",
        "What you and your Muse are working toward",
    );
    rail.append(&goals_nav);
    let create_nav = nav_button(
        "umfl-create",
        "Create",
        "Start something new with your Muse",
    );
    rail.append(&create_nav);
    let feed_nav = nav_button("umfl-feed", "Feed", "Briefings and updates from your Muse");
    rail.append(&feed_nav);
    let rail_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rail_spacer.set_vexpand(true);
    rail.append(&rail_spacer);
    let new_button = nav_button(
        "umfl-new",
        "New chat",
        "Go to the main chat and start typing",
    );
    rail.append(&new_button);
    let download_button = nav_button(
        "umfl-download",
        "Download",
        "Get the latest build of this app",
    );
    rail.append(&download_button);
    let menu_button = nav_button("umfl-menu", "Menu", "Account connection and sign-in");
    rail.append(&menu_button);
    sidebar.append(&rail);

    let panel = gtk::Box::new(gtk::Orientation::Vertical, 6);
    panel.add_css_class("side-panel-wrap");
    panel.set_size_request(403, -1);
    let panel_stack = gtk::Stack::new();
    panel_stack.set_vexpand(true);
    panel_stack.set_vhomogeneous(false);
    panel_stack.set_hhomogeneous(false);
    // Chat's panel (the conversation list) is added once the account
    // page exists; the rest start as titled panels.
    for (name, title, copy) in [
        (
            "library",
            "Library",
            "Your spaces and saved artifacts for the connected account.",
        ),
        ("ideas", "Ideas", "Ideas your Muse saved for you."),
        (
            "goals",
            "Goals",
            "What you and your Muse are working toward.",
        ),
        ("create", "Create", "Start something new with your Muse."),
        ("feed", "Feed", "Briefings and updates from your Muse."),
        (
            "host",
            "Local host",
            "Consent, folder grants and local approvals for this machine.",
        ),
    ] {
        panel_stack.add_named(&panel_placeholder(title, copy), Some(name));
    }
    panel.append(&panel_stack);

    // Panel-bottom build identity: a stale AppImage is otherwise
    // impossible to tell apart from the build that fixed the thing
    // being reported.
    let build_label = gtk::Label::new(Some(&build_identity::label()));
    build_label.add_css_class("build-identity");
    build_label.set_halign(gtk::Align::Start);
    build_label.set_wrap(true);
    build_label.set_tooltip_text(Some("Version and when this build was produced (UTC)"));
    panel.append(&build_label);
    sidebar.append(&panel);

    let main_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main_column.add_css_class("main-column");
    main_column.set_hexpand(true);
    main_column.set_vexpand(true);
    let topbar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    topbar.add_css_class("topbar");
    let page_title = gtk::Label::new(Some("Chat"));
    page_title.add_css_class("page-title");
    topbar.append(&page_title);
    // The Connected pill came out of the topbar on Patrick's order
    // (Nov 11): connection state lives in the account page's own
    // notices and the right panel's status line now. The label itself
    // stays — the account page writes its state into it.
    let service_status = gtk::Label::new(Some("Not connected"));
    service_status.add_css_class("status-chip");
    main_column.append(&topbar);

    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_vhomogeneous(false);
    stack.set_hhomogeneous(false);
    let account_page: account_ui::AccountPage = account_ui::build(&service_status);
    stack.add_named(&account_page.root, Some("chats"));
    let host_page = local_host::build(
        account_page.host_context.clone(),
        account_page.host_device.clone(),
    );
    *account_page.host_boundary.borrow_mut() = Some(host_page.shutdown.clone());
    stack.add_named(&host_page.root, Some("host"));
    let mail_page: mail_ui::MailPage = mail_ui::build();
    let calendar_page: calendar_ui::CalendarPage = calendar_ui::build();
    let desktop_page: desktop_ui::DesktopPage = desktop_ui::build();
    let computer_control_page: computer_control_ui::ComputerControlPage =
        computer_control_ui::build();
    let tasks_page: tasks_ui::TasksPage = tasks_ui::build();
    let library_page: library_ui::LibraryPage =
        library_ui::build(account_page.host_context.clone());
    stack.add_named(&library_page.root, Some("library"));
    let ideas_page = service_ui::ideas(account_page.host_context.clone());
    stack.add_named(&ideas_page.root, Some("ideas"));
    let goals_page = service_ui::goals(account_page.host_context.clone());
    stack.add_named(&goals_page.root, Some("goals"));
    stack.add_named(
        &placeholder_page(
            "Create",
            "Create is where muse.ai starts new pages, images and media with your Muse. The surface isn't connected in the Linux client yet.",
        ),
        Some("create"),
    );

    // Pages from the old navigation (mail, calendar, desktop, computer
    // control, reminders, the developer experiment, the files browser)
    // are still built above so their lifecycle stays identical, but
    // they are no longer part of the app Patrick asked for: the menu is
    // muse.ai's, and nothing else is reachable.
    let cancel_login = Rc::clone(&account_page.cancel_login);
    let stop_host = host_page.shutdown.clone();
    let stop_calendar = calendar_page.shutdown.clone();
    let stop_computer_control = computer_control_page.shutdown.clone();
    window.connect_close_request(move |_| {
        stop_host();
        stop_calendar();
        stop_computer_control();
        cancel_login();
        gtk::glib::Propagation::Proceed
    });
    // Referenced so the built-but-unreachable pages above are plainly
    // deliberate; their roots are simply never shown.
    let _ = (
        &mail_page.root,
        &calendar_page.root,
        &desktop_page.root,
        &computer_control_page.root,
        &tasks_page.root,
    );
    stack.set_visible_child_name("chats");
    panel_stack.add_named(&account_page.threads_panel, Some("chats"));
    panel_stack.set_visible_child_name("chats");
    // The sidebar panel follows whichever page the stack shows, whoever
    // asked for the switch (rail, settings popover, Open files).
    let panel_follow = panel_stack.clone();
    stack.connect_notify_local(Some("visible-child-name"), move |stack, _| {
        if let Some(name) = stack.visible_child_name() {
            panel_follow.set_visible_child_name(&name);
        }
    });
    let chat_page = gtk::Box::new(gtk::Orientation::Vertical, 14);
    chat_page.add_css_class("page-content");

    // Mac-style centered reading column: conversation content is capped
    // at ~921px so bubbles keep messenger rhythm on wide windows instead
    // of stretching edge to edge — a cap, never a minimum (see
    // bind_reading_column). Setup stays full width above it.
    let chat_column = gtk::Box::new(gtk::Orientation::Vertical, 10);
    chat_column.add_css_class("chat-column");
    chat_column.set_halign(gtk::Align::Center);
    chat_column.set_vexpand(true);
    chat_column.set_size_request(READING_COLUMN_START_PX, -1);

    let assistant_identity = gtk::Box::new(gtk::Orientation::Vertical, 5);
    assistant_identity.set_halign(gtk::Align::Center);
    assistant_identity.add_css_class("assistant-identity");
    let assistant_avatar = gtk::Label::new(Some("M"));
    assistant_avatar.add_css_class("assistant-avatar");
    let assistant_name = gtk::Label::new(Some("Developer experiment · Meta Model API"));
    assistant_name.add_css_class("assistant-name");
    assistant_identity.append(&assistant_avatar);
    assistant_identity.append(&assistant_name);
    chat_column.append(&assistant_identity);

    let setup_expander = gtk::Expander::new(Some("Connection & permissions"));
    setup_expander.add_css_class("settings-disclosure");
    setup_expander.set_expanded(false);
    let setup_content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    setup_content.add_css_class("settings-content");
    let disclaimer = gtk::Label::new(Some(
        "Independent, open-source Linux client; not affiliated with Meta. This is a native GTK app, not an embedded Muse website.",
    ));
    disclaimer.set_wrap(true);
    disclaimer.set_xalign(0.0);
    disclaimer.add_css_class("dim-label");
    setup_content.append(&disclaimer);

    let connection = gtk::Label::new(Some(
        "Meta Model API uses a developer key and is separate from consumer Muse sign-in. Prompts are sent only after you submit with paid-use consent enabled; charges may apply.",
    ));
    connection.set_xalign(0.0);
    connection.set_wrap(true);
    connection.add_css_class("card");
    setup_content.append(&connection);

    let credential_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let key_entry = gtk::Entry::new();
    key_entry.set_visibility(false);
    key_entry.set_hexpand(true);
    key_entry.set_placeholder_text(Some("Meta Model API key (kept in memory only)"));
    let use_key = gtk::Button::with_label("Use key this session");
    let clear_session = gtk::Button::with_label("Forget key & chat");
    credential_row.append(&key_entry);
    credential_row.append(&use_key);
    credential_row.append(&clear_session);
    setup_content.append(&credential_row);

    let billing_consent = gtk::CheckButton::with_label(
        "Allow paid Meta Model API requests; prompts leave this device and may be billed.",
    );
    billing_consent.set_halign(gtk::Align::Start);
    setup_content.append(&billing_consent);

    let file_search_consent = gtk::CheckButton::with_label(
        "Allow searching the granted folder; matching file names and paths may be sent to Meta.",
    );
    file_search_consent.set_sensitive(false);
    file_search_consent.set_halign(gtk::Align::Start);
    setup_content.append(&file_search_consent);

    let file_content_consent = gtk::CheckButton::with_label(
        "Allow reading granted text files; up to 16 KiB per read may be sent to Meta.",
    );
    file_content_consent.set_sensitive(false);
    file_content_consent.set_halign(gtk::Align::Start);
    setup_content.append(&file_content_consent);
    setup_expander.set_child(Some(&setup_content));
    chat_page.append(&setup_expander);

    let chat_history = gtk::Box::new(gtk::Orientation::Vertical, 10);
    chat_history.set_valign(gtk::Align::Start);
    append_day_divider(&chat_history);
    append_chat(
        &chat_history,
        ChatRole::Assistant,
        "Developer experiment only. This paid API-key conversation is separate from your personal Muse account and shared history. Use Chats in the sidebar for your existing Muse agent. The key and experimental chat stay in memory for this session.",
    );
    let chat_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_height(160)
        .build();
    chat_scroll.add_css_class("chat-scroll");
    chat_scroll.set_child(Some(&chat_history));
    chat_column.append(&chat_scroll);

    let chat_status = gtk::Label::new(Some("No API key is active."));
    chat_status.set_xalign(0.0);
    chat_status.set_wrap(true);
    chat_status.add_css_class("chat-status");
    chat_column.append(&chat_status);

    let prompt_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    prompt_row.add_css_class("composer");
    let open_files = gtk::Button::from_icon_name("list-add-symbolic");
    open_files.set_tooltip_text(Some("Open local read-only file browser"));
    prompt_row.append(&open_files);
    let prompt_entry = gtk::Entry::new();
    prompt_entry.set_hexpand(true);
    prompt_entry.set_placeholder_text(Some("Developer experiment: prompt the Meta Model API…"));
    prompt_row.append(&prompt_entry);
    let send_prompt = gtk::Button::with_label("Send");
    send_prompt.add_css_class("suggested-action");
    prompt_row.append(&send_prompt);
    chat_column.append(&prompt_row);
    chat_page.append(&chat_column);
    bind_reading_column(chat_page.upcast_ref(), &chat_column);

    main_column.append(&stack);
    shell.set_start_child(Some(&sidebar));
    // The account page's right-hand panel (identity, scheduled tasks,
    // recent runs) rides beside the page column. It hides itself until
    // the account links, and the chat column's binder adapts to
    // whatever room that leaves.
    let content_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    main_column.set_hexpand(true);
    content_row.append(&main_column);
    content_row.append(&account_page.side_panel);
    shell.set_end_child(Some(&content_row));

    let api_key_for_set = Rc::clone(&api_key);
    let session_for_set = Rc::clone(&session);
    let history_for_set = chat_history.clone();
    let status_for_set = chat_status.clone();
    let entry_for_set = key_entry.clone();
    let billing_for_set = billing_consent.clone();
    let search_consent_for_set = file_search_consent.clone();
    let content_consent_for_set = file_content_consent.clone();
    use_key.connect_clicked(move |_| {
        let key = entry_for_set.text().to_string();
        if key.trim().is_empty() {
            status_for_set.set_text("Enter a developer key. It is held only in memory and is never saved to disk.");
            return;
        }
        *api_key_for_set.borrow_mut() = Some(key);
        *session_for_set.borrow_mut() = AgentSession::new();
        billing_for_set.set_active(false);
        search_consent_for_set.set_active(false);
        content_consent_for_set.set_active(false);
        clear_chat(&history_for_set);
        entry_for_set.set_text("");
        status_for_set.set_text("Key held in memory only. No request is made until you send a prompt with paid-use consent enabled.");
    });

    let api_key_for_clear = Rc::clone(&api_key);
    let session_for_clear = Rc::clone(&session);
    let history_for_clear = chat_history.clone();
    let status_for_clear = chat_status.clone();
    let entry_for_clear = key_entry.clone();
    let billing_for_clear = billing_consent.clone();
    let search_consent_for_clear = file_search_consent.clone();
    let content_consent_for_clear = file_content_consent.clone();
    clear_session.connect_clicked(move |_| {
        api_key_for_clear.borrow_mut().take();
        *session_for_clear.borrow_mut() = AgentSession::new();
        billing_for_clear.set_active(false);
        search_consent_for_clear.set_active(false);
        content_consent_for_clear.set_active(false);
        clear_chat(&history_for_clear);
        entry_for_clear.set_text("");
        status_for_clear.set_text("API key and conversation cleared from this session.");
    });

    let api_key_for_send = Rc::clone(&api_key);
    let session_for_send = Rc::clone(&session);
    let busy_for_send = Rc::clone(&agent_busy);
    let grant_for_send = Rc::clone(&grant);
    let billing_for_send = billing_consent.clone();
    let search_consent_for_send = file_search_consent.clone();
    let content_consent_for_send = file_content_consent.clone();
    let prompt_for_send = prompt_entry.clone();
    let history_for_send = chat_history.clone();
    let chat_status_for_send = chat_status.clone();
    let send_button_for_send = send_prompt.clone();
    let use_key_for_send = use_key.clone();
    let clear_session_for_send = clear_session.clone();
    let billing_control_for_send = billing_consent.clone();
    let search_control_for_send = file_search_consent.clone();
    let content_control_for_send = file_content_consent.clone();
    let grant_for_ui = Rc::clone(&grant_for_send);
    send_prompt.connect_clicked(move |_| {
        if busy_for_send.get() {
            return;
        }
        let prompt = prompt_for_send.text().to_string();
        if prompt.trim().is_empty() {
            chat_status_for_send.set_text("Enter a prompt first.");
            return;
        }
        let Some(api_key) = api_key_for_send.borrow().clone() else {
            chat_status_for_send.set_text("Enter and activate a Meta Model API developer key first.");
            return;
        };
        if !billing_for_send.is_active() {
            chat_status_for_send.set_text("Enable paid-use consent before sending. No request was made.");
            return;
        }

        let file_grant = grant_for_send.borrow().clone();
        let policy = ToolPolicy {
            search_names: search_consent_for_send.is_active(),
            read_file_contents: content_consent_for_send.is_active(),
        };
        let mut worker_session = session_for_send.borrow().clone();
        append_chat(&history_for_send, ChatRole::User, &prompt);
        prompt_for_send.set_text("");
        busy_for_send.set(true);
        prompt_for_send.set_sensitive(false);
        send_button_for_send.set_sensitive(false);
        use_key_for_send.set_sensitive(false);
        clear_session_for_send.set_sensitive(false);
        billing_control_for_send.set_sensitive(false);
        search_control_for_send.set_sensitive(false);
        content_control_for_send.set_sensitive(false);
        chat_status_for_send.set_text("Sending prompt to Meta Model API…");

        let (sender, receiver) = mpsc::channel();
        let progress_sender = sender.clone();
        let submitted_prompt = prompt.clone();
        std::thread::spawn(move || {
            let result = match MetaModelClient::new(api_key) {
                Ok(client) => worker_session
                    .run_turn(
                        &client,
                        &prompt,
                        file_grant.as_ref(),
                        policy,
                        |event| {
                            let _ = progress_sender.send(AgentUiMessage::Progress(format!(
                                "{}: {}",
                                event.tool_name, event.summary
                            )));
                        },
                    )
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = sender.send(AgentUiMessage::Finished(worker_session, result));
        });

        let session_state = Rc::clone(&session_for_send);
        let busy_state = Rc::clone(&busy_for_send);
        let prompt_entry = prompt_for_send.clone();
        let send_button = send_button_for_send.clone();
        let history = history_for_send.clone();
        let status = chat_status_for_send.clone();
        let use_key_button = use_key_for_send.clone();
        let clear_session_button = clear_session_for_send.clone();
        let billing_control = billing_control_for_send.clone();
        let search_control = search_control_for_send.clone();
        let content_control = content_control_for_send.clone();
        let grant_state = Rc::clone(&grant_for_ui);
        gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
            let mut finished = false;
            loop {
                match receiver.try_recv() {
                    Ok(AgentUiMessage::Progress(progress)) => status.set_text(&progress),
                    Ok(AgentUiMessage::Finished(updated_session, result)) => {
                        *session_state.borrow_mut() = updated_session;
                        match result {
                            Ok(response) => {
                                if let Some(reason) = response.context_reset.as_deref() {
                                    clear_chat(&history);
                                    append_chat(&history, ChatRole::Notice, reason);
                                    append_chat(&history, ChatRole::User, &submitted_prompt);
                                }
                                append_chat(&history, ChatRole::Assistant, &response.text);
                                status.set_text("Response complete. Conversation remains in memory until cleared or the app closes.");
                            }
                            Err(error) => {
                                append_chat(&history, ChatRole::Notice, &error);
                                status.set_text("Request failed. The API key and chat remain session-only.");
                            }
                        }
                        busy_state.set(false);
                        prompt_entry.set_sensitive(true);
                        send_button.set_sensitive(true);
                        use_key_button.set_sensitive(true);
                        clear_session_button.set_sensitive(true);
                        billing_control.set_sensitive(true);
                        let has_grant = grant_state.borrow().is_some();
                        search_control.set_sensitive(has_grant);
                        content_control.set_sensitive(has_grant);
                        finished = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        status.set_text("Agent worker stopped unexpectedly.");
                        busy_state.set(false);
                        prompt_entry.set_sensitive(true);
                        send_button.set_sensitive(true);
                        use_key_button.set_sensitive(true);
                        clear_session_button.set_sensitive(true);
                        billing_control.set_sensitive(true);
                        let has_grant = grant_state.borrow().is_some();
                        search_control.set_sensitive(has_grant);
                        content_control.set_sensitive(has_grant);
                        finished = true;
                        break;
                    }
                }
            }
            if finished {
                gtk::glib::ControlFlow::Break
            } else {
                gtk::glib::ControlFlow::Continue
            }
        });
    });

    let files_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    files_page.add_css_class("page-content");
    let files_heading = gtk::Label::new(Some("Local files"));
    files_heading.set_xalign(0.0);
    files_heading.add_css_class("title-2");
    files_page.append(&files_heading);
    let files_explanation = gtk::Label::new(Some(
        "Browse and preview locally. Folder grants are read-only, session-scoped, and separate from sharing consent.",
    ));
    files_explanation.set_xalign(0.0);
    files_explanation.set_wrap(true);
    files_explanation.add_css_class("dim-label");
    files_page.append(&files_explanation);

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    toolbar.add_css_class("toolbar");
    let choose_folder = gtk::Button::with_label("Grant read access to a folder…");
    choose_folder.add_css_class("suggested-action");
    let location = gtk::Label::new(Some("No folder granted"));
    location.set_xalign(0.0);
    location.set_selectable(true);
    toolbar.append(&choose_folder);
    toolbar.append(&location);
    files_page.append(&toolbar);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search file names in the granted folder")
        .sensitive(false)
        .build();
    files_page.append(&search);

    let status = gtk::Label::new(Some(
        "Choose a folder to browse it. Access is read-only and remains in memory for this session.",
    ));
    status.set_xalign(0.0);
    status.set_wrap(true);
    files_page.append(&status);

    let panes = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    panes.set_vexpand(true);

    let file_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_width(340)
        .build();
    let file_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    file_list.set_margin_top(8);
    file_list.set_margin_bottom(8);
    file_list.set_margin_start(8);
    file_list.set_margin_end(8);
    file_scroll.set_child(Some(&file_list));

    let preview_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_width(420)
        .build();
    let preview = gtk::TextView::new();
    preview.set_editable(false);
    preview.set_cursor_visible(false);
    preview.set_monospace(true);
    preview.set_wrap_mode(gtk::WrapMode::WordChar);
    preview
        .buffer()
        .set_text("Select a text file to preview it here.");
    preview_scroll.set_child(Some(&preview));

    panes.append(&file_scroll);
    panes.append(&preview_scroll);
    files_page.append(&panes);

    // Confined file writes: separate grant + per-op confirmation, owned by
    // the write module; the host only wires its dialog lease and list refresh.
    {
        let list = file_list.clone();
        let preview = preview.clone();
        let status_label = status.clone();
        let grant_state = Rc::clone(&grant);
        let directory_state = Rc::clone(&current_directory);
        let refresh: Rc<dyn Fn()> = Rc::new(move || {
            render_directory(
                &list,
                &preview,
                &status_label,
                &grant_state,
                &directory_state,
            );
        });
        let lease_for_active = active_dialog.active.clone();
        let lease_for_acquire = active_dialog.active.clone();
        let lease_for_release = active_dialog.active.clone();
        let guard = files_write_ui::DialogGuard {
            is_active: Rc::new(move || lease_for_active.borrow().is_some()),
            acquire: Rc::new(move |dialog: gtk::FileChooserNative| {
                *lease_for_acquire.borrow_mut() = Some(dialog);
            }),
            release: Rc::new(move || {
                *lease_for_release.borrow_mut() = None;
            }),
        };
        files_page.append(&files_write_ui::build_section(&window, guard, refresh));
    }

    let grant_for_dialog = Rc::clone(&grant);
    let directory_for_dialog = Rc::clone(&current_directory);
    let status_for_dialog = status.clone();
    let location_for_dialog = location.clone();
    let list_for_dialog = file_list.clone();
    let preview_for_dialog = preview.clone();
    let search_for_dialog = search.clone();
    let search_consent_for_dialog = file_search_consent.clone();
    let content_consent_for_dialog = file_content_consent.clone();
    let parent_for_dialog = window.clone();
    let active_dialog_for_click = active_dialog.clone();
    let busy_for_dialog = Rc::clone(&agent_busy);
    choose_folder.connect_clicked(move |_| {
        if busy_for_dialog.get() {
            status_for_dialog
                .set_text("Wait for the current assistant task before changing the folder grant.");
            return;
        }
        if active_dialog_for_click.is_active() {
            return;
        }

        let dialog = gtk::FileChooserNative::new(
            Some("Choose a folder to grant read access"),
            Some(&parent_for_dialog),
            gtk::FileChooserAction::SelectFolder,
            Some("Grant read access"),
            Some("Cancel"),
        );

        let grant_state = Rc::clone(&grant_for_dialog);
        let directory_state = Rc::clone(&directory_for_dialog);
        let status_label = status_for_dialog.clone();
        let location_label = location_for_dialog.clone();
        let list = list_for_dialog.clone();
        let preview = preview_for_dialog.clone();
        let search_entry = search_for_dialog.clone();
        let search_consent = search_consent_for_dialog.clone();
        let content_consent = content_consent_for_dialog.clone();
        let weak_dialog_lease = active_dialog_for_click.weak();
        dialog.connect_response(move |chooser, response| {
            if response == gtk::ResponseType::Accept {
                match chooser.file().and_then(|file| file.path()) {
                    Some(path) => match FilesystemGrant::new(&path) {
                        Ok(new_grant) => {
                            location_label.set_text(&path.display().to_string());
                            *grant_state.borrow_mut() = Some(new_grant);
                            *directory_state.borrow_mut() = PathBuf::new();
                            search_entry.set_sensitive(true);
                            search_entry.set_text("");
                            search_consent.set_active(false);
                            search_consent.set_sensitive(true);
                            content_consent.set_active(false);
                            content_consent.set_sensitive(true);
                            preview
                                .buffer()
                                .set_text("Select a text file to preview it here.");
                            render_directory(
                                &list,
                                &preview,
                                &status_label,
                                &grant_state,
                                &directory_state,
                            );
                        }
                        Err(error) => {
                            status_label.set_text(&format!("Could not grant folder: {error}"))
                        }
                    },
                    None => status_label
                        .set_text("The selected location is not a local filesystem path."),
                }
            }
            chooser.hide();
            if let Some(active) = weak_dialog_lease.upgrade() {
                active.borrow_mut().take();
            }
        });
        active_dialog_for_click.keep(dialog.clone());
        dialog.show();
    });

    let list_for_search = file_list.clone();
    let preview_for_search = preview.clone();
    let status_for_search = status.clone();
    let grant_for_search = Rc::clone(&grant);
    let directory_for_search = Rc::clone(&current_directory);
    search.connect_search_changed(move |entry| {
        let query = entry.text();
        if query.trim().is_empty() {
            render_directory(
                &list_for_search,
                &preview_for_search,
                &status_for_search,
                &grant_for_search,
                &directory_for_search,
            );
        } else {
            render_search_results(
                &list_for_search,
                &preview_for_search,
                &status_for_search,
                &grant_for_search,
                &directory_for_search,
                query.as_str(),
            );
        }
    });

    // The Feed page is built here rather than with the other pages:
    // its Discuss action needs the finished navigation — it lands on
    // the chat page the way the rail's New action does, with the post
    // quoted in the composer.
    let feed_page = {
        let stack = stack.clone();
        let title = page_title.clone();
        let chats = chats_nav.clone();
        let library = library_nav.clone();
        let ideas = ideas_nav.clone();
        let goals = goals_nav.clone();
        let create = create_nav.clone();
        let feed = feed_nav.clone();
        let discuss = Rc::clone(&account_page.discuss);
        feed_ui::feed(
            account_page.host_context.clone(),
            Rc::new(move |text: String| {
                select_page(
                    &stack,
                    &title,
                    &[
                        (&chats, "chats"),
                        (&library, "library"),
                        (&ideas, "ideas"),
                        (&goals, "goals"),
                        (&create, "create"),
                        (&feed, "feed"),
                    ],
                    "chats",
                );
                discuss(text);
            }),
        )
    };
    stack.add_named(&feed_page.root, Some("feed"));

    // Opening a data tab loads it: the shell fires each page's load
    // hook as its stack page becomes visible, so Ideas, Goals, Feed
    // and Library show their contents on the click that opens them
    // instead of waiting for a Refresh press. The hook is a no-op
    // while a read is already running; signed out, the page says so
    // itself.
    {
        let ideas_load = ideas_page.load.clone();
        let goals_load = goals_page.load.clone();
        let feed_load = feed_page.load.clone();
        let library_load = library_page.load.clone();
        stack.connect_notify_local(Some("visible-child-name"), move |stack, _| {
            if let Some(name) = stack.visible_child_name() {
                match name.as_str() {
                    "ideas" => ideas_load(),
                    "goals" => goals_load(),
                    "feed" => feed_load(),
                    "library" => library_load(),
                    _ => {}
                }
            }
        });
    }

    // The rail is the muse.ai menu and nothing else: Chat, Search,
    // Library, Ideas, Goals, Create and Feed, then New, Download and
    // Menu as actions along the bottom.
    let navigation = [
        (&chats_nav, "chats"),
        (&library_nav, "library"),
        (&ideas_nav, "ideas"),
        (&goals_nav, "goals"),
        (&create_nav, "create"),
        (&feed_nav, "feed"),
    ];
    for (button, page) in navigation {
        let stack = stack.clone();
        let title = page_title.clone();
        let chats = chats_nav.clone();
        let library = library_nav.clone();
        let ideas = ideas_nav.clone();
        let goals = goals_nav.clone();
        let create = create_nav.clone();
        let feed = feed_nav.clone();
        button.connect_clicked(move |_| {
            select_page(
                &stack,
                &title,
                &[
                    (&chats, "chats"),
                    (&library, "library"),
                    (&ideas, "ideas"),
                    (&goals, "goals"),
                    (&create, "create"),
                    (&feed, "feed"),
                ],
                page,
            );
        });
    }
    {
        // Search: land on Chat with the sidebar search focused.
        let stack = stack.clone();
        let title = page_title.clone();
        let chats = chats_nav.clone();
        let library = library_nav.clone();
        let ideas = ideas_nav.clone();
        let goals = goals_nav.clone();
        let create = create_nav.clone();
        let feed = feed_nav.clone();
        let focus_search = Rc::clone(&account_page.focus_search);
        search_button.connect_clicked(move |_| {
            select_page(
                &stack,
                &title,
                &[
                    (&chats, "chats"),
                    (&library, "library"),
                    (&ideas, "ideas"),
                    (&goals, "goals"),
                    (&create, "create"),
                    (&feed, "feed"),
                ],
                "chats",
            );
            focus_search();
        });
    }
    {
        // New: the main conversation, composer focused, ready to type.
        let stack = stack.clone();
        let title = page_title.clone();
        let chats = chats_nav.clone();
        let library = library_nav.clone();
        let ideas = ideas_nav.clone();
        let goals = goals_nav.clone();
        let create = create_nav.clone();
        let feed = feed_nav.clone();
        let new_chat = Rc::clone(&account_page.new_chat);
        new_button.connect_clicked(move |_| {
            select_page(
                &stack,
                &title,
                &[
                    (&chats, "chats"),
                    (&library, "library"),
                    (&ideas, "ideas"),
                    (&goals, "goals"),
                    (&create, "create"),
                    (&feed, "feed"),
                ],
                "chats",
            );
            new_chat();
        });
    }
    {
        // On the web this icon gets you the app; here it gets the
        // latest build of this one. Running from an AppImage, that is
        // an in-place self-update (see updater.rs); a source build
        // keeps the releases page.
        let window_for_download = window.clone();
        download_button.connect_clicked(move |_| {
            if let Some(path) = updater::appimage_path() {
                show_update_window(&window_for_download, path);
            } else {
                let _ = gtk::gio::AppInfo::launch_default_for_uri(
                    updater::RELEASES_PAGE,
                    None::<&gtk::gio::AppLaunchContext>,
                );
            }
        });
    }
    {
        // Menu: the account connection, on the Chat page where the
        // sign-in surface lives (the old topbar gear's exact behavior).
        let stack = stack.clone();
        let title = page_title.clone();
        let chats = chats_nav.clone();
        let library = library_nav.clone();
        let ideas = ideas_nav.clone();
        let goals = goals_nav.clone();
        let create = create_nav.clone();
        let feed = feed_nav.clone();
        let setup_for_menu = account_page.settings.clone();
        menu_button.connect_clicked(move |_| {
            select_page(
                &stack,
                &title,
                &[
                    (&chats, "chats"),
                    (&library, "library"),
                    (&ideas, "ideas"),
                    (&goals, "goals"),
                    (&create, "create"),
                    (&feed, "feed"),
                ],
                "chats",
            );
            setup_for_menu.set_expanded(true);
        });
    }

    {
        // Launch-time update check, at most once per TTL and only for
        // the AppImage build: a silent GitHub read whose only visible
        // effect, when a newer build exists, is a note on the panel's
        // build line and the Download button's tooltip.
        if updater::appimage_path().is_some() && updater::check_due(now_epoch()) {
            let (sender, receiver) = std::sync::mpsc::channel::<bool>();
            std::thread::spawn(move || {
                let available = updater::fetch_latest()
                    .map(|release| {
                        updater::update_available(
                            build_identity::commit(),
                            build_identity::built_epoch(),
                            &release,
                        )
                    })
                    .unwrap_or(false);
                updater::note_check_done(now_epoch());
                let _ = sender.send(available);
            });
            let build_label = build_label.clone();
            let download_button = download_button.clone();
            let mut tries = 0;
            gtk::glib::timeout_add_local(Duration::from_millis(250), move || {
                tries += 1;
                match receiver.try_recv() {
                    Ok(true) => {
                        build_label.set_text(&format!(
                            "{} · update available — use Download",
                            build_identity::label()
                        ));
                        download_button
                            .set_tooltip_text(Some("An update is available — click to install it"));
                        gtk::glib::ControlFlow::Break
                    }
                    Ok(false) => gtk::glib::ControlFlow::Break,
                    Err(std::sync::mpsc::TryRecvError::Empty) if tries < 60 => {
                        gtk::glib::ControlFlow::Continue
                    }
                    Err(_) => gtk::glib::ControlFlow::Break,
                }
            });
        }
    }
    window.set_child(Some(&shell));
    window.present();
}

fn nav_button(icon_name: &str, label: &str, tooltip: &str) -> gtk::Button {
    // Icon rail button, muse.ai-shaped: the section name lives in the
    // tooltip and the sidebar panel title, not beside the icon.
    let button = gtk::Button::new();
    button.set_tooltip_text(Some(&format!("{label} — {tooltip}")));
    button.add_css_class("nav-item");
    button.set_halign(gtk::Align::Center);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("nav-icon");
    icon.set_pixel_size(40);
    button.set_child(Some(&icon));
    button
}

fn panel_placeholder(title: &str, copy: &str) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 6);
    panel.add_css_class("side-panel");
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("panel-title");
    heading.set_halign(gtk::Align::Start);
    let text = gtk::Label::new(Some(copy));
    text.add_css_class("panel-copy");
    text.set_wrap(true);
    text.set_xalign(0.0);
    text.set_halign(gtk::Align::Start);
    panel.append(&heading);
    panel.append(&text);
    panel
}

/// The widest a centered reading column may grow, in app px (the
/// calibrated scale's counterpart of the web's ~768 CSS px column).
pub(crate) const READING_COLUMN_MAX_PX: i32 = 921;

/// The width a reading column starts at, before its binder adapts it:
/// small enough that the shell's static minimum fits a laptop screen.
pub(crate) const READING_COLUMN_START_PX: i32 = 480;

/// Drive a reading column's width request from its parent's real
/// allocation: min(parent width, READING_COLUMN_MAX_PX). A bare
/// set_size_request(1075) made 1075 the column's MINIMUM — rail (106) +
/// panel (470) + column (1075) + page padding exceeded a 1680px laptop
/// screen, and the composer's send button and the Connected pill slid
/// off the right edge (Patrick's Nov 11 screenshot). Every declarative
/// alternative was tried and measured first: the request poisons the
/// minimum through any container (a scrolled window's viewport reports
/// a non-scrollable child's minimum as its own), and a max-content-
/// width cap alone follows the content's natural width, so an empty
/// conversation collapsed the column to ~400px on wide screens. The
/// parent's allocation is truthful in the cases that matter — a
/// maximised or clamped window, any manual resize — so the request
/// tracks it: full cap on wide screens, exactly what fits on small
/// ones. GTK4 has no size-allocate signal; a frame tick watching the
/// parent's width is the supported way to follow it. Call AFTER
/// appending the column to the parent.
pub(crate) fn bind_reading_column(parent: &gtk::Widget, column: &gtk::Box) {
    let col = column.clone();
    parent.add_tick_callback(move |widget, _| {
        let target = widget.width().clamp(320, READING_COLUMN_MAX_PX);
        if col.width_request() != target {
            col.set_size_request(target, -1);
        }
        gtk::glib::ControlFlow::Continue
    });
}

fn placeholder_page(title: &str, copy: &str) -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 10);
    page.add_css_class("page-content");
    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.set_halign(gtk::Align::Center);
    column.set_vexpand(true);
    column.set_size_request(READING_COLUMN_START_PX, -1);
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("page-heading");
    heading.set_halign(gtk::Align::Start);
    let text = gtk::Label::new(Some(copy));
    text.add_css_class("dim-label");
    text.set_wrap(true);
    text.set_xalign(0.0);
    text.set_halign(gtk::Align::Start);
    column.append(&heading);
    column.append(&text);
    page.append(&column);
    bind_reading_column(page.upcast_ref(), &column);
    page
}

fn nav_css_classes(active: bool) -> &'static [&'static str] {
    if active {
        &["nav-item", "active"]
    } else {
        &["nav-item"]
    }
}

fn select_page(
    stack: &gtk::Stack,
    title: &gtk::Label,
    navigation: &[(&gtk::Button, &str)],
    page: &str,
) {
    stack.set_visible_child_name(page);
    title.set_text(match page {
        "chats" => "Chat",
        "files" => "Files",
        "host" => "Local host",
        "mail" => "Mail",
        "calendar" => "Calendar",
        "desktop" => "Desktop",
        "computer-control" => "Computer Control",
        "tasks" => "Reminders",
        "library" => "Library",
        "ideas" => "Ideas",
        "goals" => "Goals",
        "create" => "Create",
        "feed" => "Feed",
        "developer" => "Developer API",
        _ => "Unknown page",
    });
    for (button, name) in navigation {
        button.set_css_classes(nav_css_classes(*name == page));
    }
}

fn append_day_divider(container: &gtk::Box) {
    let text = gtk::glib::DateTime::now_local()
        .and_then(|now| now.format("%A, %B %-d"))
        .map(|dated| dated.to_string())
        .unwrap_or_else(|_| "Today".into());
    let divider = gtk::Label::new(Some(&text));
    divider.set_halign(gtk::Align::Center);
    divider.add_css_class("day-divider");
    container.append(&divider);
}

fn current_timestamp() -> String {
    gtk::glib::DateTime::now_local()
        .and_then(|now| now.format("%H:%M"))
        .map(|stamped| stamped.to_string())
        .unwrap_or_default()
}

fn append_chat(container: &gtk::Box, role: ChatRole, message: &str) {
    append_chat_stamped(container, role, message, Some(current_timestamp()));
}

/// `stamp` is the time shown under the bubble. Callers rendering stored
/// history pass the message's own time (or None when the service supplied
/// none) — never the render time, which would stamp a whole refreshed
/// transcript with the current minute.
fn append_chat_stamped(container: &gtk::Box, role: ChatRole, message: &str, stamp: Option<String>) {
    append_chat_grouped(container, role, message, stamp, "M", false);
}

/// One transcript row. `avatar_initial` is the letter in the assistant's
/// circle — the account identity's initial when the caller knows it.
/// `grouped` marks a row continuing the previous row's turn: tighter
/// margins and no repeated avatar (an equal-width spacer keeps the text
/// column aligned), the way the web transcript groups a turn's rows.
#[allow(clippy::too_many_arguments)]
fn append_chat_grouped(
    container: &gtk::Box,
    role: ChatRole,
    message: &str,
    stamp: Option<String>,
    avatar_initial: &str,
    grouped: bool,
) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_hexpand(true);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.set_margin_top(if grouped { 1 } else { 6 });
    row.set_margin_bottom(if grouped { 1 } else { 6 });

    if role == ChatRole::Assistant {
        if grouped {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_width_request(50);
            row.append(&spacer);
        } else {
            let avatar = gtk::Label::new(Some(avatar_initial));
            avatar.add_css_class("message-avatar");
            avatar.set_valign(gtk::Align::Start);
            row.append(&avatar);
        }
    }
    if role == ChatRole::User {
        // bubble_alignment() says End, but halign alone cannot move a
        // natural-width child inside a horizontal box: the row packs from
        // the start. An expanding spacer is what actually right-aligns the
        // user's bubble, as every messenger client does.
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        row.append(&spacer);
    }

    let bubble = gtk::Box::new(gtk::Orientation::Vertical, 2);
    bubble.set_halign(match bubble_alignment(role) {
        BubbleAlignment::Start => gtk::Align::Start,
        BubbleAlignment::End => gtk::Align::End,
    });
    bubble.add_css_class("chat-bubble");
    bubble.add_css_class(match role {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Notice => "notice",
    });
    // Assistant replies render rich when they carry markdown (the web
    // client renders them; a raw label shows the machinery). Anything the
    // rich path cannot validate falls back to the plain label, unchanged.
    let rich = if role == ChatRole::Assistant && rich_text::looks_like_markdown(message) {
        rich_bubble_content(message)
    } else {
        None
    };
    if let Some(rich) = rich {
        bubble.append(&rich);
    } else {
        let label = gtk::Label::new(Some(message));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_selectable(true);
        label.set_max_width_chars(88);
        bubble.append(&label);
    }
    if let Some(stamp_text) = stamp {
        let stamp = gtk::Label::new(Some(&stamp_text));
        stamp.set_xalign(0.0);
        stamp.set_halign(match bubble_alignment(role) {
            BubbleAlignment::Start => gtk::Align::Start,
            BubbleAlignment::End => gtk::Align::End,
        });
        stamp.add_css_class("message-meta");
        bubble.append(&stamp);
    }
    row.append(&bubble);
    container.append(&row);
}

/// Build the rich form of an assistant reply, or `None` when any block's
/// markup fails Pango validation — the caller then uses the plain label,
/// so a formatting surprise can never blank out the reply text.
fn rich_bubble_content(message: &str) -> Option<gtk::Box> {
    let blocks = rich_text::markdown_blocks(message);
    if blocks.is_empty() {
        return None;
    }
    for block in &blocks {
        let payloads: Vec<&String> = match block {
            rich_text::Block::Paragraph(text)
            | rich_text::Block::Heading(_, text)
            | rich_text::Block::Quote(text) => vec![text],
            rich_text::Block::Bullets(items) | rich_text::Block::Numbers(items) => {
                items.iter().collect()
            }
            rich_text::Block::Code(_) | rich_text::Block::Rule => Vec::new(),
        };
        for payload in payloads {
            gtk::pango::parse_markup(payload, '\0').ok()?;
        }
    }
    let column = gtk::Box::new(gtk::Orientation::Vertical, 7);
    column.set_hexpand(true);
    for block in &blocks {
        column.append(&rich_block_widget(block));
    }
    Some(column)
}

fn markup_label(markup: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_markup(markup);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_selectable(true);
    label.set_max_width_chars(88);
    label
}

fn rich_block_widget(block: &rich_text::Block) -> gtk::Widget {
    match block {
        rich_text::Block::Paragraph(text) => markup_label(text).upcast(),
        rich_text::Block::Heading(level, text) => {
            let size = match level {
                1 => "145%",
                2 => "128%",
                _ => "115%",
            };
            markup_label(&format!(
                "<span size=\"{size}\" weight=\"bold\">{text}</span>"
            ))
            .upcast()
        }
        rich_text::Block::Bullets(items) => rich_list_widget(items, None).upcast(),
        rich_text::Block::Numbers(items) => rich_list_widget(items, Some(1)).upcast(),
        rich_text::Block::Code(raw) => {
            let label = gtk::Label::new(Some(raw));
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            label.set_selectable(true);
            label.add_css_class("code-block");
            label.set_hexpand(true);
            label.upcast()
        }
        rich_text::Block::Quote(text) => {
            let label = markup_label(text);
            label.add_css_class("quote-block");
            label.upcast()
        }
        rich_text::Block::Rule => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
    }
}

fn rich_list_widget(items: &[String], numbering: Option<usize>) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 3);
    for (index, item) in items.iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let glyph = gtk::Label::new(Some(&match numbering {
            Some(start) => format!("{}.", start + index),
            None => "\u{2022}".to_string(),
        }));
        glyph.set_xalign(0.0);
        glyph.set_valign(gtk::Align::Start);
        glyph.add_css_class("dim-label");
        row.append(&glyph);
        let text = markup_label(item);
        text.set_hexpand(true);
        row.append(&text);
        column.append(&row);
    }
    column
}

fn clear_chat(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn render_search_results(
    list: &gtk::Box,
    preview: &gtk::TextView,
    status: &gtk::Label,
    grant: &SharedGrant,
    current_directory: &SharedDirectory,
    query: &str,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    preview
        .buffer()
        .set_text("Select a matching text file to preview it here.");

    let grant_borrow = grant.borrow();
    let Some(granted_root) = grant_borrow.as_ref() else {
        status.set_text("Choose a folder before searching.");
        return;
    };

    match granted_root.search_names(query, 100) {
        Ok(result) => {
            let truncated = result.truncated;
            let files: Vec<FileEntry> = result
                .entries
                .into_iter()
                .filter(|entry| !entry.is_directory)
                .collect();
            if truncated {
                status.set_text(&format!(
                    "At least {} file-name matches for “{query}”; search limits were reached. Results are scoped to the granted folder.",
                    files.len()
                ));
            } else {
                status.set_text(&format!(
                    "{} file-name match{} for “{query}” in the granted folder.",
                    files.len(),
                    if files.len() == 1 { "" } else { "es" }
                ));
            }
            if files.is_empty() {
                list.append(&gtk::Label::new(Some("No matching files found.")));
            } else {
                for entry in files {
                    append_entry(list, preview, status, grant, current_directory, entry);
                }
            }
        }
        Err(error) => status.set_text(&format!("Could not search this folder: {error}")),
    }
}

fn render_directory(
    list: &gtk::Box,
    preview: &gtk::TextView,
    status: &gtk::Label,
    grant: &SharedGrant,
    current_directory: &SharedDirectory,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let grant_borrow = grant.borrow();
    let Some(granted_root) = grant_borrow.as_ref() else {
        status.set_text("Choose a folder to browse it.");
        return;
    };

    let relative_directory = current_directory.borrow().clone();
    if !relative_directory.as_os_str().is_empty() {
        let up_button = gtk::Button::with_label("← Parent folder");
        let list_again = list.clone();
        let preview_again = preview.clone();
        let status_again = status.clone();
        let grant_again = Rc::clone(grant);
        let directory_again = Rc::clone(current_directory);
        up_button.connect_clicked(move |_| {
            directory_again.borrow_mut().pop();
            render_directory(
                &list_again,
                &preview_again,
                &status_again,
                &grant_again,
                &directory_again,
            );
        });
        list.append(&up_button);
    }

    match granted_root.list(&relative_directory) {
        Ok(entries) => {
            status.set_text(&format!(
                "Read-only grant active • {} entr{} • browsing stays local; agent sharing is separately opt-in.",
                entries.len(),
                if entries.len() == 1 { "y" } else { "ies" }
            ));
            for entry in entries {
                append_entry(list, preview, status, grant, current_directory, entry);
            }
        }
        Err(error) => status.set_text(&format!("Could not list this folder: {error}")),
    }
}

fn append_entry(
    list: &gtk::Box,
    preview: &gtk::TextView,
    status: &gtk::Label,
    grant: &SharedGrant,
    current_directory: &SharedDirectory,
    entry: FileEntry,
) {
    let label = if entry.is_directory {
        format!("▸  {}/", entry.name)
    } else {
        format!("▤  {}  ·  {} bytes", entry.name, entry.size_bytes)
    };
    let button = gtk::Button::with_label(&label);
    button.set_halign(gtk::Align::Fill);
    button.add_css_class("file-row");

    if entry.is_directory {
        let path = entry.relative_path;
        let list_again = list.clone();
        let preview_again = preview.clone();
        let status_again = status.clone();
        let grant_again = Rc::clone(grant);
        let directory_again = Rc::clone(current_directory);
        button.connect_clicked(move |_| {
            *directory_again.borrow_mut() = path.clone();
            render_directory(
                &list_again,
                &preview_again,
                &status_again,
                &grant_again,
                &directory_again,
            );
        });
    } else {
        let path = entry.relative_path;
        let status_label = status.clone();
        let preview_widget = preview.clone();
        let grant_state = Rc::clone(grant);
        button.connect_clicked(move |_| {
            let grant_borrow = grant_state.borrow();
            let Some(granted_root) = grant_borrow.as_ref() else {
                status_label.set_text("Choose a folder before reading files.");
                return;
            };
            match granted_root.read_text(&path) {
                Ok(text) => {
                    preview_widget.buffer().set_text(&text);
                    status_label.set_text(&format!(
                        "Previewed {} locally; agent sharing requires separate consent.",
                        path.display()
                    ));
                }
                Err(error) => {
                    preview_widget.buffer().set_text("");
                    status_label.set_text(&format!("Could not read file: {error}"));
                }
            }
        });
    }
    list.append(&button);
}

#[cfg(test)]
mod tests {
    use super::DialogLease;
    use std::cell::Cell;
    use std::rc::Rc;

    struct DropProbe(Rc<Cell<usize>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn navigation_selection_preserves_labeled_item_style() {
        assert_eq!(super::nav_css_classes(true), &["nav-item", "active"]);
        assert_eq!(super::nav_css_classes(false), &["nav-item"]);
    }

    #[test]
    fn chat_message_roles_place_user_on_right_and_assistant_on_left() {
        assert_eq!(
            super::bubble_alignment(super::ChatRole::User),
            super::BubbleAlignment::End
        );
        assert_eq!(
            super::bubble_alignment(super::ChatRole::Assistant),
            super::BubbleAlignment::Start
        );
        assert_eq!(
            super::bubble_alignment(super::ChatRole::Notice),
            super::BubbleAlignment::Start
        );
    }

    #[test]
    fn dark_theme_css_covers_every_custom_widget_class() {
        // Every add_css_class name in the desktop UI must have a matching rule
        // so polish classes never silently stop applying.
        for class in [
            "app-shell",
            "app-sidebar",
            "icon-rail",
            "side-panel-wrap",
            "side-panel",
            "panel-list",
            "panel-item-label",
            "panel-heading",
            "panel-title",
            "panel-copy",
            "search-pill",
            "rail-separator",
            "composer-pill",
            "send-round",
            "brand-mark",
            "nav-item",
            "nav-icon",
            "rail-button",
            "main-column",
            "topbar",
            "topbar-button",
            "page-title",
            "status-chip",
            "page-content",
            "chat-column",
            "chat-scroll",
            "assistant-identity",
            "assistant-avatar",
            "message-avatar",
            "assistant-name",
            "message-meta",
            "day-divider",
            "section-heading",
            "settings-disclosure",
            "welcome-card",
            "welcome-title",
            "welcome-copy",
            "settings-content",
            "toolbar",
            "chat-bubble",
            "chat-status",
            "composer",
            "dim-label",
            "card",
            "approval-card",
            "approval-icon",
            "approval-heading",
            "approval-details",
            "approval-scroll",
            "approval-countdown",
            "approval-actions",
            "file-row",
            "dark-window",
        ] {
            assert!(
                super::DARK_THEME_CSS.contains(&format!(".{class}"))
                    || super::DARK_THEME_CSS.contains(&format!("window.{class}")),
                "DARK_THEME_CSS is missing a rule for .{class}"
            );
        }
    }

    #[test]
    fn dialog_lease_keeps_dialog_alive_until_response_release() {
        let lease = DialogLease::<DropProbe>::default();
        let drops = Rc::new(Cell::new(0));
        lease.keep(DropProbe(Rc::clone(&drops)));

        assert!(lease.is_active());
        assert_eq!(drops.get(), 0);

        if let Some(active) = lease.weak().upgrade() {
            active.borrow_mut().take();
        }

        assert!(!lease.is_active());
        assert_eq!(drops.get(), 1);
    }
}
