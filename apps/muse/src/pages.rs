//! Secondary surfaces for the rewrite: threads, settings, and the
//! lazy page builders behind the sidebar. Each page builds its
//! structure once and refills on every visit, so a revisit never
//! shows a stale list. Service reads ride the chat's ticket slot;
//! pages never dispatch directly.
//!
//! The surfaces land in route order across this milestone; this file
//! grows one page at a time.

use std::rc::{Rc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::local_host::Phase;

use super::widget::describe;
use adw::prelude::*;

use super::app::{Chat, dispatch};
use super::session::{Op, account_dir};
use muse_account::{ArtifactSummary, ListingItem, Schedule, ServiceListing, TaskRun};

/// What a page needs: the chat for reads, the stack to navigate, and
/// a way back to the sign-in screen when the link goes away.
#[derive(Clone)]
pub struct Ctx {
    pub chat: Weak<Chat>,
    pub stack: gtk4::Stack,
    pub sidebar: gtk4::ListBox,
    pub history: gtk4::ListBox,
    pub profile_name: gtk4::Label,
    /// The icon rail: (page, button) pairs so the open page's icon
    /// reads selected beside the sidebar row.
    pub rail: Vec<(String, gtk4::Button)>,
    pub show_signin: Rc<dyn Fn()>,
    pub updates: Rc<std::cell::RefCell<super::update::UpdateCheck>>,
}

/// The rail glyph for a page, if it has one: the bundled SVG name
/// backing the web-shaped menu. The book stands for Feed, the check
/// for Goals, shapes for Library — the web's glyphs land on the
/// sections they open. The rest live in the sidebar list only.
pub fn rail_glyph(page: &str) -> Option<&'static str> {
    match page {
        "chat" => Some("chat"),
        "search" => Some("search"),
        "feed" => Some("library"),
        "ideas" => Some("ideas"),
        "goals" => Some("tasks"),
        "library" => Some("shapes"),
        _ => None,
    }
}

/// Page names in sidebar order.
pub const PAGES: &[&str] = &[
    "chat",
    "threads",
    "tasks",
    "library",
    "feed",
    "search",
    "ideas",
    "goals",
    "settings",
    "localhost",
];

/// Human titles for the sidebar rows.
pub fn title(name: &str) -> &str {
    match name {
        "chat" => "Chat",
        "threads" => "Threads",
        "tasks" => "Tasks",
        "library" => "Library",
        "feed" => "Feed",
        "localhost" => "Local host",
        "search" => "Search",
        "ideas" => "Ideas",
        "goals" => "Goals",
        "settings" => "Settings",
        _ => name,
    }
}

/// Whether showing a page needs a sidebar selection change. Selecting
/// the already-selected row re-fires row_selected, whose handler calls
/// straight back into show(): an unconditional select recurses until
/// the stack overflows.
pub fn needs_select(selected: Option<i32>, target: usize) -> bool {
    selected != Some(target as i32)
}

/// Whether a rail press folds the page drawer away instead of
/// navigating: pressing the icon of the page already on screen while
/// the drawer is open closes the drawer, the way muse.ai's rail does.
pub fn rail_closes(current: Option<&str>, page: &str, drawer_open: bool) -> bool {
    drawer_open && current == Some(page)
}

/// Show a page: build its structure on first visit, refill on every
/// visit, then navigate the stack and the sidebar together. A visit
/// the ticket slot refuses does not leave a blank page behind:
/// while the page stays visible the fill retries a few times, so a
/// skipped visit still lands once the in-flight read clears.
pub fn show(ctx: &Ctx, name: &str) {
    let existing = ctx.stack.child_by_name(name);
    if existing.is_none() {
        let page = build(ctx, name);
        ctx.stack.add_named(&page, Some(name));
    }
    let dispatched = fill(ctx, name);
    ctx.stack.set_visible_child_name(name);
    fill_history(ctx);
    // The rail echoes the page beside the sidebar row.
    for (page, button) in &ctx.rail {
        if page == name {
            button.add_css_class("muse-rail-active");
        } else {
            button.remove_css_class("muse-rail-active");
        }
    }
    // Selecting the row re-fires row_selected, whose handler calls
    // straight back into show(): selecting an already-selected row
    // would recurse until the stack overflows. Only select on change.
    let selected = ctx.sidebar.selected_row().map(|row| row.index());
    for (index, page) in PAGES.iter().enumerate() {
        if *page == name
            && needs_select(selected, index)
            && let Some(row) = ctx.sidebar.row_at_index(index as i32)
        {
            ctx.sidebar.select_row(Some(&row));
        }
    }
    if name == "chat"
        && let Some(chat) = ctx.chat.upgrade()
    {
        chat.scroll_to_latest();
    }
    if !dispatched {
        retry_fill_while_visible(ctx, name, 0);
    }
}

/// A skipped visit never leaves a blank page behind: while the page
/// stays visible the fill retries a few times, so a visit that lost
/// the ticket race still lands once the in-flight read clears the
/// slot. Moving to another page stops the retries.
fn retry_fill_while_visible(ctx: &Ctx, name: &str, attempt: u32) {
    const ATTEMPTS: u32 = 5;
    if attempt >= ATTEMPTS {
        return;
    }
    let ctx = ctx.clone();
    let name = name.to_owned();
    gtk4::glib::timeout_add_local(Duration::from_secs(3), move || {
        if ctx.stack.visible_child_name().as_deref() != Some(name.as_str()) {
            return gtk4::glib::ControlFlow::Break;
        }
        if fill(&ctx, &name) {
            return gtk4::glib::ControlFlow::Break;
        }
        retry_fill_while_visible(&ctx, &name, attempt + 1);
        gtk4::glib::ControlFlow::Break
    });
}

fn build(ctx: &Ctx, name: &str) -> gtk4::Box {
    let inner = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    inner.set_margin_start(16);
    inner.set_margin_end(16);
    inner.set_margin_top(12);
    match name {
        "threads" => {
            // The fifth page of the web UI port, beside the native one:
            // the same rows, groups and side-chat split. Insensitive until
            // the native fill has a listing, so a window cannot open
            // claiming the account has no threads.
            let head_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let heading = gtk4::Label::new(Some("Threads"));
            heading.add_css_class("muse-page-title");
            heading.set_halign(gtk4::Align::Start);
            heading.set_hexpand(true);
            head_row.append(&heading);
            let web_view = gtk4::Button::with_label("Web view");
            web_view.set_widget_name("threads-web");
            web_view.set_valign(gtk4::Align::Center);
            web_view.set_sensitive(false);
            web_view.set_tooltip_text(Some(
                "Open these Threads in the web UI slice of the port (read-only)",
            ));
            if !crate::webui::available() {
                web_view.set_tooltip_text(Some("WebKit helpers are unavailable in this build"));
            }
            head_row.append(&web_view);
            {
                // Resolved per press: the snapshot is the rows the native
                // fill last held.
                let chat = ctx.chat.clone();
                web_view.connect_clicked(move |_| {
                    let held = threads_snapshot();
                    let client = chat.upgrade().and_then(|chat| chat.client_handle());
                    if !crate::webui::open_threads_window(held, client) {
                        eprintln!("muse: web view unavailable in this build");
                    }
                });
            }
            inner.append(&head_row);
            let list = gtk4::ListBox::new();
            list.set_widget_name("threads-list");
            inner.append(&list);
            let weak = ctx.chat.clone();
            let stack = ctx.stack.clone();
            let sidebar = ctx.sidebar.clone();
            // Same rule as the drawer's History: a thread opens on
            // activation, so the arrow keys only move the highlight.
            list.connect_row_activated(move |list, row| {
                let name = row.widget_name();
                let Some(id) = name.as_str().strip_prefix("thread:") else {
                    return;
                };
                let id = id.to_owned();
                if let Some(chat) = weak.upgrade() {
                    chat.switch_thread(Some(id));
                }
                list.select_row(None::<&gtk4::ListBoxRow>);
                stack.set_visible_child_name("chat");
                if let Some(chat_row) = sidebar.row_at_index(0) {
                    sidebar.select_row(Some(&chat_row));
                }
            });
        }
        "tasks" => {
            // The third page of the web UI port, beside the native one:
            // the same schedules, runs and local reminders. Insensitive
            // until a read lands, like the other pages' entry points.
            let head_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let heading = gtk4::Label::new(Some("Tasks"));
            heading.add_css_class("muse-page-title");
            heading.set_halign(gtk4::Align::Start);
            heading.set_hexpand(true);
            head_row.append(&heading);
            let web_view = gtk4::Button::with_label("Web view");
            web_view.set_widget_name("tasks-web");
            web_view.set_valign(gtk4::Align::Center);
            web_view.set_sensitive(false);
            web_view.set_tooltip_text(Some(
                "Open these Tasks in the web UI slice of the port (read-only)",
            ));
            if !crate::webui::available() {
                web_view.set_tooltip_text(Some("WebKit helpers are unavailable in this build"));
            }
            head_row.append(&web_view);
            {
                let chat = ctx.chat.clone();
                web_view.connect_clicked(move |_| {
                    let held = tasks_snapshot();
                    let client = chat.upgrade().and_then(|chat| chat.client_handle());
                    if !crate::webui::open_tasks_window(held, client) {
                        eprintln!("muse: web view unavailable in this build");
                    }
                });
            }
            inner.append(&head_row);
            let schedules = gtk4::ListBox::new();
            schedules.set_widget_name("tasks-schedules");
            inner.append(&schedules);
            let runs = gtk4::ListBox::new();
            runs.set_widget_name("tasks-runs");
            inner.append(&runs);
            // Local reminders: the durable store the runner
            // notifies from, created and cancelled here. Server
            // schedules above are read-only plus delete; these
            // rows are the Linux half the P4 gate asks for.
            let local_head = gtk4::Label::new(Some("Local reminders"));
            local_head.set_halign(gtk4::Align::Start);
            local_head.add_css_class("muse-section");
            inner.append(&local_head);
            let local_new = gtk4::Button::with_label("New reminder");
            local_new.set_widget_name("tasks-local-new");
            local_new.set_halign(gtk4::Align::Start);
            inner.append(&local_new);
            let local = gtk4::ListBox::new();
            local.set_widget_name("tasks-local");
            local.set_selection_mode(gtk4::SelectionMode::None);
            inner.append(&local);
            let page_new = inner.downgrade();
            local_new.connect_clicked(move |_| {
                if let Some(page) = page_new.upgrade() {
                    show_reminder_dialog(&page);
                }
            });
            // The runner claims without us: re-read the store on a
            // slow tick so a fired reminder shows as notified and
            // a newly due one appears, all without polling the
            // service. The tick ends with the page.
            let page_tick = inner.downgrade();
            gtk4::glib::timeout_add_local(Duration::from_secs(15), move || {
                let Some(page) = page_tick.upgrade() else {
                    return gtk4::glib::ControlFlow::Break;
                };
                refresh_local_tasks(&page);
                gtk4::glib::ControlFlow::Continue
            });
        }
        "feed" => {
            // The web opens on its prompt card above the units: the
            // prompt fills on every visit beside the list, with Edit
            // and the generation state beside it.
            let head_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let heading = gtk4::Label::new(Some("Feed"));
            heading.add_css_class("muse-page-title");
            heading.set_halign(gtk4::Align::Start);
            heading.set_hexpand(true);
            head_row.append(&heading);
            // The sixth page of the web UI port, beside the native one.
            // Insensitive until a feed lands, so a window cannot open
            // claiming the feed is empty.
            let web_view = gtk4::Button::with_label("Web view");
            web_view.set_widget_name("feed-web");
            web_view.set_valign(gtk4::Align::Center);
            web_view.set_sensitive(false);
            web_view.set_tooltip_text(Some(
                "Open this Feed in the web UI slice of the port (read-only)",
            ));
            if !crate::webui::available() {
                web_view.set_tooltip_text(Some("WebKit helpers are unavailable in this build"));
            }
            head_row.append(&web_view);
            {
                // Resolved per press: the snapshot is the cards the
                // native paint last held.
                let chat = ctx.chat.clone();
                web_view.connect_clicked(move |_| {
                    let held = feed_snapshot();
                    let client = chat.upgrade().and_then(|chat| chat.client_handle());
                    if !crate::webui::open_feed_window(held, client) {
                        eprintln!("muse: web view unavailable in this build");
                    }
                });
            }
            inner.append(&head_row);
            let prompt_head = gtk4::Label::new(Some("YOUR FEED PROMPT"));
            prompt_head.add_css_class("muse-section");
            prompt_head.set_halign(gtk4::Align::Start);
            inner.append(&prompt_head);
            let prompt_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let prompt = gtk4::Label::new(None);
            prompt.set_widget_name("feed-prompt");
            prompt.set_wrap(true);
            prompt.set_xalign(0.0);
            prompt.set_hexpand(true);
            prompt.add_css_class("muse-feed-card");
            prompt_row.append(&prompt);
            let edit = gtk4::Button::with_label("Edit");
            edit.add_css_class("flat");
            edit.set_valign(gtk4::Align::Start);
            edit.set_tooltip_text(Some("Edit the feed prompt"));
            prompt_row.append(&edit);
            inner.append(&prompt_row);
            // Editing dispatches the live-probed prompt replacement,
            // then re-reads the feed so the card proves the new words.
            let prompt_text = prompt.clone();
            let chat_weak = ctx.chat.clone();
            let ctx_owned = ctx.clone();
            let page_weak = inner.downgrade();
            edit.connect_clicked(move |edit| {
                let window = gtk4::Window::new();
                window.set_modal(true);
                window.set_default_size(480, -1);
                window.set_title(Some("Feed prompt"));
                if let Some(root) = edit
                    .root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok())
                {
                    window.set_transient_for(Some(&root));
                }
                let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
                body.set_margin_start(24);
                body.set_margin_end(24);
                body.set_margin_top(20);
                body.set_margin_bottom(20);
                let entry = gtk4::Entry::new();
                entry.set_text(&prompt_text.text());
                entry.set_hexpand(true);
                body.append(&entry);
                let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                let cancel = gtk4::Button::with_label("Cancel");
                cancel.add_css_class("flat");
                buttons.append(&cancel);
                let save = gtk4::Button::with_label("Save");
                save.add_css_class("muse-cta");
                buttons.append(&save);
                body.append(&buttons);
                window.set_child(Some(&body));
                {
                    let window = window.clone();
                    cancel.connect_clicked(move |_| {
                        window.close();
                    });
                }
                let chat_weak = chat_weak.clone();
                let ctx_owned = ctx_owned.clone();
                let page_weak = page_weak.clone();
                {
                    let window = window.clone();
                    save.connect_clicked(move |_| {
                        let words = entry.text().to_string();
                        if words.trim().is_empty() {
                            entry.set_placeholder_text(Some("The prompt needs non-blank words."));
                            return;
                        }
                        let Some(chat) = chat_weak.upgrade() else {
                            return;
                        };
                        let Some(client) = chat.client_handle() else {
                            return;
                        };
                        window.close();
                        let ctx_done = ctx_owned.clone();
                        let page_done = page_weak.clone();
                        super::app::dispatch(
                            &chat,
                            {
                                let client = client.clone();
                                move || client.feed_prompt_update(&words)
                            },
                            move |chat, answer| {
                                let accepted =
                                    answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                                if accepted {
                                    show(&ctx_done, "feed");
                                } else if let Some(inner) = page_done.upgrade() {
                                    inner.append(&card_note(
                                        "The service did not change it. Nothing changed.",
                                    ));
                                }
                                let _ = chat;
                            },
                        );
                    });
                }
                window.present();
            });
            let status = gtk4::Button::with_label("");
            status.set_widget_name("feed-status");
            status.set_halign(gtk4::Align::Start);
            status.add_css_class("flat");
            status.add_css_class("dim-label");
            status.set_tooltip_text(Some("Re-read the feed now"));
            inner.append(&status);
            // The web's Generating control re-reads: a press refills
            // the page from the wire, same as a fresh visit.
            {
                let ctx_owned = ctx.clone();
                let page_weak = inner.downgrade();
                status.connect_clicked(move |_| {
                    if page_weak.upgrade().is_none() {
                        return;
                    }
                    show(&ctx_owned, "feed");
                });
            }
            let list = gtk4::ListBox::new();
            list.set_widget_name("feed-list");
            inner.append(&list);
        }
        "search" => {
            // The seventh page of the web UI port. Search is the first page
            // whose native side is a query rather than a listing, so the web
            // slice carries its own query box and its own read: the button is
            // live from the start, and the page says plainly that no search
            // has run yet when the cache is empty.
            let head_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let heading = gtk4::Label::new(Some("Search"));
            heading.add_css_class("muse-page-title");
            heading.set_halign(gtk4::Align::Start);
            heading.set_hexpand(true);
            head_row.append(&heading);
            let web_view = gtk4::Button::with_label("Web view");
            web_view.set_widget_name("search-web");
            web_view.set_valign(gtk4::Align::Center);
            web_view.set_sensitive(crate::webui::available());
            web_view.set_tooltip_text(Some(
                "Open Search in the web UI slice of the port (read-only)",
            ));
            if !crate::webui::available() {
                web_view.set_tooltip_text(Some("WebKit helpers are unavailable in this build"));
            }
            head_row.append(&web_view);
            {
                // Resolved per press: the window opens on the query and rows
                // the native search last painted.
                let chat = ctx.chat.clone();
                web_view.connect_clicked(move |_| {
                    let held = search_snapshot();
                    let client = chat.upgrade().and_then(|chat| chat.client_handle());
                    if !crate::webui::open_search_window(held, client) {
                        eprintln!("muse: web view unavailable in this build");
                    }
                });
            }
            inner.append(&head_row);
            let query = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let entry = gtk4::Entry::new();
            entry.set_widget_name("search-query");
            entry.set_hexpand(true);
            entry.set_placeholder_text(Some("Search every conversation"));
            query.append(&entry);
            let go = gtk4::Button::with_label("Search");
            query.append(&go);
            inner.append(&query);
            let list = gtk4::ListBox::new();
            list.set_widget_name("search-list");
            inner.append(&list);
            let weak = ctx.chat.clone();
            let ctx_search = ctx.clone();
            let page_weak = inner.downgrade();
            go.connect_clicked(move |_| {
                let Some(inner) = page_weak.upgrade() else {
                    return;
                };
                let Some(chat) = weak.upgrade() else { return };
                do_search(&ctx_search, &chat, &inner);
            });
        }
        "ideas" | "goals" => {
            let heading = gtk4::Label::new(Some(if name == "ideas" { "Ideas" } else { "Goals" }));
            heading.add_css_class("muse-page-title");
            heading.set_halign(gtk4::Align::Start);
            if name == "goals" {
                // The web's page opens with creation and the
                // Tracking region above the roster.
                let head_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                heading.set_hexpand(true);
                head_row.append(&heading);
                let create = gtk4::Button::with_label("＋ Create a goal");
                create.add_css_class("muse-cta");
                head_row.append(&create);
                // The second page of the web UI port, beside the native
                // one: the same roster and Tracking rows, so the two can
                // be compared on the same data. Insensitive until a fill
                // lands, for the same reason the Library's is: a window
                // opened on nothing would claim the page is empty.
                let web_view = gtk4::Button::with_label("Web view");
                web_view.set_widget_name("goals-web");
                web_view.set_valign(gtk4::Align::Center);
                web_view.set_sensitive(false);
                web_view.set_tooltip_text(Some(
                    "Open these Goals in the web UI slice of the port (read-only)",
                ));
                if !crate::webui::available() {
                    web_view.set_tooltip_text(Some("WebKit helpers are unavailable in this build"));
                }
                head_row.append(&web_view);
                {
                    // The weak chat is resolved per press: the window opens
                    // late enough that the account link is settled, and the
                    // snapshot is the rows the native fill last held.
                    let chat = ctx.chat.clone();
                    web_view.connect_clicked(move |_| {
                        let held = goals_snapshot();
                        let client = chat.upgrade().and_then(|chat| chat.client_handle());
                        if !crate::webui::open_goals_window(held, client) {
                            eprintln!("muse: web view unavailable in this build");
                        }
                    });
                }
                inner.append(&head_row);
                {
                    let ctx = ctx.clone();
                    let inner_weak = inner.downgrade();
                    create.connect_clicked(move |_| {
                        let Some(inner) = inner_weak.upgrade() else {
                            return;
                        };
                        open_goal_create_dialog(&ctx, &inner, None);
                    });
                }
                let track_head = gtk4::Label::new(Some("Tracking"));
                track_head.add_css_class("muse-section");
                track_head.set_halign(gtk4::Align::Start);
                inner.append(&track_head);
                let tracking = gtk4::ListBox::new();
                tracking.set_widget_name("goals-tracking-list");
                tracking.set_selection_mode(gtk4::SelectionMode::Single);
                inner.append(&tracking);
                let goals_head = gtk4::Label::new(Some("Goals"));
                goals_head.add_css_class("muse-section");
                goals_head.set_halign(gtk4::Align::Start);
                inner.append(&goals_head);
            } else {
                // The fourth page of the web UI port, beside the native
                // one: the same roster, so the two can be compared on the
                // same data. Insensitive until a listing lands, so a window
                // cannot open claiming the page is empty.
                let head_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                heading.set_hexpand(true);
                head_row.append(&heading);
                let web_view = gtk4::Button::with_label("Web view");
                web_view.set_widget_name("ideas-web");
                web_view.set_valign(gtk4::Align::Center);
                web_view.set_sensitive(false);
                web_view.set_tooltip_text(Some(
                    "Open these Ideas in the web UI slice of the port (read-only)",
                ));
                if !crate::webui::available() {
                    web_view.set_tooltip_text(Some("WebKit helpers are unavailable in this build"));
                }
                head_row.append(&web_view);
                {
                    // Resolved per press: the account link is settled long
                    // before a press, and the snapshot is the rows the
                    // native fill last held.
                    let chat = ctx.chat.clone();
                    web_view.connect_clicked(move |_| {
                        let held = ideas_snapshot();
                        let client = chat.upgrade().and_then(|chat| chat.client_handle());
                        if !crate::webui::open_ideas_window(held, client) {
                            eprintln!("muse: web view unavailable in this build");
                        }
                    });
                }
                inner.append(&head_row);
            }
            // The web's roster is one scrolling list: icon, title,
            // clamped description, overflow menu. Ideas rows open the
            // detail dialog; the page itself does not drill in.
            let list = gtk4::ListBox::new();
            list.set_widget_name(&format!("{name}-list"));
            list.set_selection_mode(gtk4::SelectionMode::Single);
            inner.append(&list);
            // Rows open their detail dialog; the page itself does not
            // drill in. Ideas read the idea, goals read the goal —
            // and the Tracking section reads from its own rows.
            if name == "ideas" || name == "goals" {
                let ctx_owned = ctx.clone();
                let page_name = name.to_owned();
                list.connect_row_selected(move |list, row| {
                    let Some(row) = row else { return };
                    if page_name == "ideas" {
                        open_idea_dialog(&ctx_owned, list, row);
                    } else {
                        open_goal_dialog(&ctx_owned, list, row);
                    }
                });
            }
            if name == "goals"
                && let Some(tracking) = named_list(&inner, "goals-tracking-list")
            {
                let ctx_owned = ctx.clone();
                tracking.connect_row_selected(move |list, row| {
                    let Some(row) = row else { return };
                    open_goal_dialog(&ctx_owned, list, row);
                });
            }
        }
        "library" => {
            let heading = gtk4::Label::new(Some("Library"));
            heading.add_css_class("muse-page-title");
            heading.set_halign(gtk4::Align::Start);
            // The first page of the web UI port, beside the native one:
            // same rows, rendered by the web slice, so the two can be
            // compared on the same data.
            let head_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
            head_row.set_valign(gtk4::Align::Center);
            head_row.append(&heading);
            let web_view = gtk4::Button::with_label("Web view");
            web_view.set_widget_name("library-web");
            web_view.set_halign(gtk4::Align::End);
            web_view.set_hexpand(true);
            // Insensitive until a listing exists: a window that opened on
            // nothing would claim the library is empty when the truth is
            // that this page has not read it yet.
            web_view.set_sensitive(false);
            web_view.set_tooltip_text(Some(
                "Open this Library in the web UI slice of the port (read-only)",
            ));
            if !crate::webui::available() {
                web_view.set_tooltip_text(Some("WebKit helpers are unavailable in this build"));
            }
            head_row.append(&web_view);
            inner.append(&head_row);
            // The weak chat is resolved per press, not per build: the window
            // opens late enough that the account link is settled, and a press
            // after that still finds the same client.
            let chat = ctx.chat.clone();
            web_view.connect_clicked(move |_| {
                let (artifacts, total) = library_snapshot();
                let client = chat.upgrade().and_then(|chat| chat.client_handle());
                if !crate::webui::open_library_window(artifacts, total, client) {
                    eprintln!("muse: web view unavailable in this build");
                }
            });
            // Quick Open filters titles client-side; the tab bar
            // filters by kind. Both repaint from held rows, never the
            // wire, and both are wired here so fills never double-wire.
            let search = gtk4::Entry::new();
            search.set_widget_name("library-search");
            search.set_placeholder_text(Some("Quick Open"));
            search.set_hexpand(true);
            inner.append(&search);
            let tabs = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            tabs.set_widget_name("library-tabs");
            inner.append(&tabs);
            let list = gtk4::ListBox::new();
            list.set_widget_name("library-list");
            inner.append(&list);
            let ctx_owned = ctx.clone();
            let page_weak = inner.downgrade();
            search.connect_changed(move |entry| {
                let Some(page) = page_weak.upgrade() else {
                    return;
                };
                let query = entry.text().to_string();
                repaint_library(&ctx_owned, &page, &query);
            });
        }
        "settings" => {
            // The eighth page of the web UI port. Settings is the first page
            // whose native side is held state rather than a listing, so the
            // web view carries the link, the updater's last answer and the
            // policy the chat panel read; the three presses that change
            // anything (check, download, sign out) stay on this page.
            let head_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let heading = gtk4::Label::new(Some("Settings"));
            heading.add_css_class("muse-page-title");
            heading.set_halign(gtk4::Align::Start);
            heading.set_hexpand(true);
            head_row.append(&heading);
            let web_view = gtk4::Button::with_label("Web view");
            web_view.set_widget_name("settings-web");
            web_view.set_valign(gtk4::Align::Center);
            web_view.set_sensitive(crate::webui::available());
            web_view.set_tooltip_text(Some(
                "Open Settings in the web UI slice of the port (read-only)",
            ));
            if !crate::webui::available() {
                web_view.set_tooltip_text(Some("WebKit helpers are unavailable in this build"));
            }
            head_row.append(&web_view);
            {
                // Resolved per press: the window opens on what the app holds
                // now, including the policy the chat panel last read.
                web_view.connect_clicked(move |_| {
                    if !crate::webui::open_settings_window(settings_snapshot()) {
                        eprintln!("muse: web view unavailable in this build");
                    }
                });
            }
            inner.append(&head_row);
            let info = gtk4::Label::new(None);
            info.set_widget_name("settings-info");
            info.set_wrap(true);
            info.set_xalign(0.0);
            inner.append(&info);
            let updates_title = gtk4::Label::new(Some("Updates"));
            updates_title.set_widget_name("updates-title");
            updates_title.set_xalign(0.0);
            inner.append(&updates_title);
            let updates_status = gtk4::Label::new(None);
            updates_status.set_widget_name("updates-status");
            updates_status.set_wrap(true);
            updates_status.set_xalign(0.0);
            inner.append(&updates_status);
            let check = gtk4::Button::with_label("Check for updates");
            check.set_widget_name("updates-check");
            inner.append(&check);
            let action = gtk4::Button::with_label("Download update");
            action.set_widget_name("updates-action");
            inner.append(&action);
            let ctx_check = ctx.clone();
            let page_weak = inner.downgrade();
            check.connect_clicked(move |_| {
                let Some(inner) = page_weak.upgrade() else {
                    return;
                };
                do_check(&ctx_check, &inner);
            });
            let ctx_action = ctx.clone();
            let page_weak = inner.downgrade();
            action.connect_clicked(move |_| {
                let Some(inner) = page_weak.upgrade() else {
                    return;
                };
                do_update_action(&ctx_action, &inner);
            });
            let sign_out = gtk4::Button::with_label("Sign out");
            inner.append(&sign_out);
            let show_signin = ctx.show_signin.clone();
            sign_out.connect_clicked(move |_| {
                if let Some(dir) = account_dir() {
                    let _ = std::fs::remove_file(dir.join("account-connection.json"));
                }
                show_signin();
            });
        }
        "localhost" => {
            let heading = gtk4::Label::new(Some("Local host"));
            heading.add_css_class("muse-page-title");
            heading.set_halign(gtk4::Align::Start);
            inner.append(&heading);
            // The consent copy is the boundary: filenames and
            // content reach Muse only in granted folders, writes
            // only with a local Allow. Nothing here runs until the
            // person checks the box, grants a folder, and starts.
            let intro = gtk4::Label::new(Some(
                "Separate from account connection and the read-only Library. When enabled, Muse may search filenames and read text inside the folders you grant. Filenames and content reach Muse. In a read-only folder every write is refused outright; in a read-write folder, every write requires your local Allow once approval of the exact path and, where text changes, the complete resulting content. Nothing is overwritten, permanently deleted, or run as a shell command.",
            ));
            intro.set_wrap(true);
            intro.set_xalign(0.0);
            inner.append(&intro);
            let identity = gtk4::Label::new(Some(super::local_host::identity_copy()));
            identity.set_widget_name("localhost-identity");
            identity.set_wrap(true);
            identity.set_xalign(0.0);
            identity.add_css_class("dim-label");
            inner.append(&identity);
            let consent = gtk4::CheckButton::with_label(
                "I explicitly enable this local host and allow granted filenames and text to reach Muse.",
            );
            consent.set_widget_name("localhost-consent");
            wrap_check(&consent);
            inner.append(&consent);
            let read_only = gtk4::CheckButton::with_label(
                "Grant read-only: folders granted while this is on can be read and searched, but Muse can never propose a write in them.",
            );
            read_only.set_widget_name("localhost-readonly");
            wrap_check(&read_only);
            inner.append(&read_only);
            let toolbar = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let choose = gtk4::Button::with_label("Grant a folder to Muse…");
            choose.set_widget_name("localhost-choose");
            toolbar.append(&choose);
            let start = gtk4::Button::with_label("Start local host");
            start.set_widget_name("localhost-start");
            start.add_css_class("muse-cta");
            toolbar.append(&start);
            let stop = gtk4::Button::with_label("Stop & revoke all");
            stop.set_widget_name("localhost-stop");
            stop.add_css_class("muse-danger");
            toolbar.append(&stop);
            inner.append(&toolbar);
            let notice = gtk4::Label::new(None);
            notice.set_widget_name("localhost-notice");
            notice.set_wrap(true);
            notice.set_xalign(0.0);
            inner.append(&notice);
            let device = gtk4::Label::new(None);
            device.set_widget_name("localhost-device");
            device.set_xalign(0.0);
            device.add_css_class("dim-label");
            inner.append(&device);
            let grants_head = gtk4::Label::new(Some("Granted folders"));
            grants_head.set_halign(gtk4::Align::Start);
            grants_head.add_css_class("muse-section");
            inner.append(&grants_head);
            let grants = gtk4::ListBox::new();
            grants.set_widget_name("localhost-grants");
            grants.set_selection_mode(gtk4::SelectionMode::None);
            inner.append(&grants);
            let allow_writes = gtk4::CheckButton::with_label(
                "Allow writes in the folders I grant without a per-write card: a saved permission, re-applied whenever I grant the same folder again. Turning this off, or Stop & revoke all, ends it.",
            );
            allow_writes.set_widget_name("localhost-allow-writes");
            wrap_check(&allow_writes);
            inner.append(&allow_writes);
            let cards_head = gtk4::Label::new(Some(
                "Pending file approvals · review the exact path and content before allowing",
            ));
            cards_head.set_halign(gtk4::Align::Start);
            cards_head.add_css_class("muse-section");
            inner.append(&cards_head);
            let cards = gtk4::ListBox::new();
            cards.set_widget_name("localhost-cards");
            cards.set_selection_mode(gtk4::SelectionMode::None);
            inner.append(&cards);
            let activity_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let activity_head =
                gtk4::Label::new(Some("Session activity · commands and outcomes only"));
            activity_head.set_hexpand(true);
            activity_head.set_halign(gtk4::Align::Start);
            activity_head.add_css_class("muse-section");
            activity_row.append(&activity_head);
            let clear = gtk4::Button::with_label("Clear log");
            clear.set_widget_name("localhost-clear");
            activity_row.append(&clear);
            inner.append(&activity_row);
            let log = gtk4::ListBox::new();
            log.set_widget_name("localhost-log");
            log.set_selection_mode(gtk4::SelectionMode::None);
            inner.append(&log);
            wire_localhost(ctx, &inner);
            // The pump lives with the page: poll, repaint, and stop
            // the host when the account goes away. It ends with the
            // page, so a host never outlives its console silently --
            // closing the page leaves cards to expire, never to hang.
            let ctx_pump = ctx.clone();
            let page_weak = inner.downgrade();
            gtk4::glib::timeout_add_local(Duration::from_millis(250), move || {
                let Some(page) = page_weak.upgrade() else {
                    return gtk4::glib::ControlFlow::Break;
                };
                let Some(chat) = ctx_pump.chat.upgrade() else {
                    return gtk4::glib::ControlFlow::Continue;
                };
                {
                    let mut host = chat.host().borrow_mut();
                    if chat.client_handle().is_none()
                        && host.snapshot().phase != Phase::Disconnected
                    {
                        host.stop();
                    }
                    host.poll();
                }
                paint_localhost(&ctx_pump, &page);
                gtk4::glib::ControlFlow::Continue
            });
        }
        _ => {
            let label = gtk4::Label::new(Some("Coming in this milestone"));
            label.set_halign(gtk4::Align::Center);
            label.set_valign(gtk4::Align::Center);
            label.set_vexpand(true);
            inner.append(&label);
        }
    }
    // Pages scroll as a whole: without this, a long list (threads,
    // schedules, Today) sets the window's minimum height and the
    // window opens taller than the screen and cannot shrink.
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&inner));
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    page.append(&scroll);
    page
}

/// Re-poll the host and repaint the page: the pump tick and
/// every press land here, so cards, replies and the notice never
/// go stale while the page shows.
fn refresh_localhost(ctx: &Ctx, page: &gtk4::Box) -> bool {
    let Some(chat) = ctx.chat.upgrade() else {
        return true;
    };
    chat.host().borrow_mut().poll();
    paint_localhost(ctx, page);
    true
}

/// Set the page's notice line. Controller failures that carry no
/// state change (a refused grant, a failed press) surface here
/// instead of silently doing nothing.
fn localhost_note(page: &gtk4::Box, text: &str) {
    if let Some(notice) = named_label(page, "localhost-notice") {
        notice.set_text(text);
    }
}

/// Wire the Local host page's buttons once; repainting only swaps
/// rows, never handlers. Every press repaints afterwards so the
/// notice, grants and cards always show the controller's answer.
fn wire_localhost(ctx: &Ctx, page: &gtk4::Box) {
    let ctx_consent = ctx.clone();
    let page_consent = page.downgrade();
    if let Some(consent) = named_check(page, "localhost-consent") {
        consent.connect_toggled(move |_| {
            let Some(page) = page_consent.upgrade() else {
                return;
            };
            paint_localhost(&ctx_consent, &page);
        });
    }
    let ctx_choose = ctx.clone();
    let page_choose = page.downgrade();
    if let Some(choose) = named_button(page, "localhost-choose") {
        choose.connect_clicked(move |_| {
            let Some(page) = page_choose.upgrade() else {
                return;
            };
            let consent = named_check(&page, "localhost-consent")
                .map(|check| check.is_active())
                .unwrap_or(false);
            let read_only = named_check(&page, "localhost-readonly")
                .map(|check| check.is_active())
                .unwrap_or(true);
            let parent = page
                .root()
                .and_then(|root| root.downcast::<gtk4::Window>().ok());
            let dialog = gtk4::FileChooserNative::new(
                Some("Grant a folder to Muse"),
                parent.as_ref(),
                gtk4::FileChooserAction::SelectFolder,
                Some("Grant"),
                Some("Cancel"),
            );
            dialog.set_select_multiple(false);
            dialog.set_modal(true);
            let ctx_done = ctx_choose.clone();
            let page_done = page_choose.clone();
            dialog.connect_response(move |dialog, response| {
                if response == gtk4::ResponseType::Accept
                    && let Some(path) = dialog
                        .files()
                        .item(0)
                        .and_then(|file| file.downcast::<gtk4::gio::File>().ok())
                        .and_then(|file| file.path())
                    && let Some(chat) = ctx_done.chat.upgrade()
                {
                    if let Err(e) = chat.host().borrow().grant(path, consent, !read_only)
                        && let Some(page) = page_done.upgrade()
                    {
                        localhost_note(&page, e);
                    }
                    if let Some(page) = page_done.upgrade() {
                        refresh_localhost(&ctx_done, &page);
                    }
                }
                dialog.destroy();
            });
            dialog.show();
        });
    }
    let ctx_start = ctx.clone();
    let page_start = page.downgrade();
    if let Some(start) = named_button(page, "localhost-start") {
        start.connect_clicked(move |_| {
            let Some(page) = page_start.upgrade() else {
                return;
            };
            let Some(chat) = ctx_start.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                localhost_note(&page, "Link your account first.");
                return;
            };
            let consent = named_check(&page, "localhost-consent")
                .map(|check| check.is_active())
                .unwrap_or(false);
            let config = client.bridge_config();
            if let Err(e) = chat.host().borrow_mut().start(Some(config), consent) {
                localhost_note(&page, e);
            }
            // The saved permission belongs to the folders: reflect
            // it in the switch now that a session exists to hold it.
            let policies = chat.host().borrow().snapshot().policies;
            if let Some(allow) = named_check(&page, "localhost-allow-writes") {
                *chat.local_host_sync.borrow_mut() = true;
                allow.set_active(!policies.is_empty());
                *chat.local_host_sync.borrow_mut() = false;
            }
            refresh_localhost(&ctx_start, &page);
        });
    }
    let ctx_stop = ctx.clone();
    let page_stop = page.downgrade();
    if let Some(stop) = named_button(page, "localhost-stop") {
        stop.connect_clicked(move |_| {
            let Some(page) = page_stop.upgrade() else {
                return;
            };
            let Some(chat) = ctx_stop.chat.upgrade() else {
                return;
            };
            chat.host().borrow_mut().stop();
            if let Some(consent) = named_check(&page, "localhost-consent") {
                consent.set_active(false);
            }
            refresh_localhost(&ctx_stop, &page);
        });
    }
    let ctx_writes = ctx.clone();
    let page_writes = page.downgrade();
    if let Some(allow) = named_check(page, "localhost-allow-writes") {
        allow.connect_toggled(move |button| {
            let Some(page) = page_writes.upgrade() else {
                return;
            };
            let Some(chat) = ctx_writes.chat.upgrade() else {
                return;
            };
            if *chat.local_host_sync.borrow() {
                return;
            }
            let active = button.is_active();
            if let Err(e) = chat.host().borrow_mut().set_write_policy(active) {
                localhost_note(&page, e);
                // Revert the switch to the saved state: with no
                // session the press cannot take effect, and a stuck
                // switch would claim a permission nobody holds.
                let saved = !chat.host().borrow().snapshot().policies.is_empty();
                *chat.local_host_sync.borrow_mut() = true;
                button.set_active(saved);
                *chat.local_host_sync.borrow_mut() = false;
            }
            refresh_localhost(&ctx_writes, &page);
        });
    }
    let ctx_clear = ctx.clone();
    let page_clear = page.downgrade();
    if let Some(clear) = named_button(page, "localhost-clear") {
        clear.connect_clicked(move |_| {
            let Some(page) = page_clear.upgrade() else {
                return;
            };
            let Some(chat) = ctx_clear.chat.upgrade() else {
                return;
            };
            chat.host().borrow_mut().clear_log();
            refresh_localhost(&ctx_clear, &page);
        });
    }
}

/// Render the host snapshot: notice, grants, approval cards and
/// the activity log. Rows are rebuilt, so card buttons below are
/// wired fresh on every repaint and never dangle at old cards.
fn paint_localhost(ctx: &Ctx, page: &gtk4::Box) {
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    let snap = chat.host().borrow().snapshot();
    if let Some(notice) = named_label(page, "localhost-notice") {
        notice.set_text(&snap.notice);
    }
    if let Some(device) = named_label(page, "localhost-device") {
        match snap.node_id.as_deref() {
            Some(id) => {
                device.set_text(&format!("Device {id}"));
                device.set_visible(true);
            }
            None => device.set_visible(false),
        }
    }
    if let Some(grants) = named_list(page, "localhost-grants") {
        clear_list(&grants);
        if snap.grants.is_empty() {
            let row = gtk4::ListBoxRow::new();
            row.set_selectable(false);
            let dim = gtk4::Label::new(Some("No folders granted."));
            dim.set_xalign(0.0);
            dim.add_css_class("dim-label");
            row.set_child(Some(&dim));
            grants.append(&row);
        }
        for grant in &snap.grants {
            let row = gtk4::ListBoxRow::new();
            row.set_selectable(false);
            let line = gtk4::Label::new(Some(&grant_row_text(
                &grant.label,
                &grant.root,
                grant.writable,
                snap.policies.iter().any(|root| root == &grant.root),
            )));
            line.set_xalign(0.0);
            line.set_wrap(true);
            row.set_child(Some(&line));
            grants.append(&row);
        }
    }
    if let Some(cards) = named_list(page, "localhost-cards") {
        clear_list(&cards);
        if snap.cards.is_empty() {
            let row = gtk4::ListBoxRow::new();
            row.set_selectable(false);
            let dim = gtk4::Label::new(Some("No pending approvals."));
            dim.set_xalign(0.0);
            dim.add_css_class("dim-label");
            row.set_child(Some(&dim));
            cards.append(&row);
        }
        for card in &snap.cards {
            let row = gtk4::ListBoxRow::new();
            row.set_selectable(false);
            let column = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
            let head = gtk4::Label::new(Some(&format!(
                "{} · {}",
                card.request.operation, card.request.path
            )));
            head.set_xalign(0.0);
            head.set_wrap(true);
            column.append(&head);
            if let Some(target) = card.request.target_path.as_deref() {
                let dest = gtk4::Label::new(Some(&format!("→ {target}")));
                dest.set_xalign(0.0);
                dest.add_css_class("dim-label");
                column.append(&dest);
            }
            if !card.request.content.is_empty() {
                let view = gtk4::TextView::new();
                view.set_editable(false);
                view.set_wrap_mode(gtk4::WrapMode::WordChar);
                view.buffer()
                    .set_text(&preview_content(&card.request.content));
                column.append(&view);
            }
            let expiry = gtk4::Label::new(Some(&approval_countdown(card.deadline)));
            expiry.set_xalign(0.0);
            expiry.add_css_class("dim-label");
            column.append(&expiry);
            let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let allow = gtk4::Button::with_label("Allow once");
            allow.add_css_class("muse-cta");
            let deny = gtk4::Button::with_label("Deny");
            if card.deciding {
                allow.set_sensitive(false);
                deny.set_sensitive(false);
            }
            let approval_id = card.request.approval_id.clone();
            let ctx_allow = ctx.clone();
            let page_allow = page.downgrade();
            allow.connect_clicked(move |_| {
                decide_localhost(&ctx_allow, &page_allow, &approval_id, true);
            });
            let approval_id = card.request.approval_id.clone();
            let ctx_deny = ctx.clone();
            let page_deny = page.downgrade();
            deny.connect_clicked(move |_| {
                decide_localhost(&ctx_deny, &page_deny, &approval_id, false);
            });
            actions.append(&allow);
            actions.append(&deny);
            column.append(&actions);
            row.set_child(Some(&column));
            cards.append(&row);
        }
    }
    if let Some(log) = named_list(page, "localhost-log") {
        clear_list(&log);
        for entry in snap.log.iter().rev().take(20) {
            let line = gtk4::Label::new(Some(entry));
            line.set_xalign(0.0);
            line.set_wrap(true);
            line.set_selectable(true);
            line.add_css_class("dim-label");
            log.append(&line);
        }
    }
}

/// One approval press: exactly one decide, then a re-poll and a
/// repaint so the card's fate shows instead of being assumed.
fn decide_localhost(
    ctx: &Ctx,
    page: &gtk4::glib::WeakRef<gtk4::Box>,
    approval_id: &str,
    allow: bool,
) {
    let Some(page) = page.upgrade() else {
        return;
    };
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    if let Err(e) = chat.host().borrow_mut().decide(approval_id, allow) {
        localhost_note(&page, e);
    }
    refresh_localhost(ctx, &page);
}

fn fill(ctx: &Ctx, name: &str) -> bool {
    let Some(page) = ctx.stack.child_by_name(name) else {
        return true;
    };
    let page: gtk4::Box = page.downcast().expect("page box");
    match name {
        "chat" => {
            if let Some(chat) = ctx.chat.upgrade() {
                chat.refresh_panel();
            }
            true
        }
        "threads" => fill_threads(ctx, &page),
        "tasks" => {
            let dispatched = fill_tasks(ctx, &page);
            refresh_local_tasks(&page);
            dispatched
        }
        "library" => fill_library(ctx, &page),
        "search" => fill_search_hint(&page),
        "ideas" => fill_ideas(ctx, &page),
        "goals" => fill_goals(ctx, &page),
        "feed" => fill_feed(ctx, &page),
        "settings" => fill_settings(ctx, &page),
        "localhost" => refresh_localhost(ctx, &page),
        _ => true,
    }
}

/// One-line schedule label: title plus state, never the raw id.
pub fn schedule_label(schedule: &Schedule) -> String {
    if schedule.enabled {
        format!("{} (on)", schedule.title)
    } else {
        format!("{} (off)", schedule.title)
    }
}

/// A schedule's next run in the reader's words: due now, in Nh,
/// tomorrow, in Nd, or in Nw. Pure over an explicit now so the
/// phrasing is unit-tested without touching the clock.
pub fn next_run_label(next_utc: Option<i64>, now_utc: i64) -> String {
    let Some(next) = next_utc else {
        return "no run scheduled".to_owned();
    };
    let delta = next - now_utc;
    if delta <= 0 {
        return "due now".to_owned();
    }
    let hours = delta / 3600;
    if hours < 20 {
        return format!("in {hours}h");
    }
    let days = hours / 24;
    if days == 1 {
        return "tomorrow".to_owned();
    }
    if days < 7 {
        return format!("in {days}d");
    }
    format!("in {}w", days / 7)
}

/// One-line run label: status plus the human summary when one exists.
pub fn run_label(run: &TaskRun) -> String {
    match run.result_summary.as_deref() {
        Some(summary) if !summary.is_empty() => {
            format!("{}: {}", run.status, summary)
        }
        _ => run.status.clone(),
    }
}

/// Find a named widget anywhere under a page: pages wrap their
/// content in a scrolled window, so targets can sit two or three
/// levels deep. The manual first-level scans that predate the wrap
/// would silently miss.
fn find_widget(root: &gtk4::Widget, name: &str) -> Option<gtk4::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(next) = child {
        if let Some(found) = find_widget(&next, name) {
            return Some(found);
        }
        child = next.next_sibling();
    }
    None
}

fn named_list(page: &gtk4::Box, name: &str) -> Option<gtk4::ListBox> {
    find_widget(page.upcast_ref(), name)?
        .downcast::<gtk4::ListBox>()
        .ok()
}

fn clear_list(list: &gtk4::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn named_label(page: &gtk4::Box, name: &str) -> Option<gtk4::Label> {
    find_widget(page.upcast_ref(), name)?
        .downcast::<gtk4::Label>()
        .ok()
}

fn named_button(page: &gtk4::Box, name: &str) -> Option<gtk4::Button> {
    find_widget(page.upcast_ref(), name)?
        .downcast::<gtk4::Button>()
        .ok()
}

/// A check button's label never wraps on its own, and these
/// are sentences: left alone, a single line sets the page's
/// minimum width and pushes the panel off the screen. Walk the
/// subtree and wrap the label like the old client did.
fn wrap_check(check: &gtk4::CheckButton) {
    let mut pending = vec![check.clone().upcast::<gtk4::Widget>()];
    while let Some(widget) = pending.pop() {
        if let Some(text) = widget.downcast_ref::<gtk4::Label>() {
            text.set_wrap(true);
            text.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
            text.set_max_width_chars(80);
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            pending.push(current.clone());
            child = current.next_sibling();
        }
    }
}

fn named_check(page: &gtk4::Box, name: &str) -> Option<gtk4::CheckButton> {
    find_widget(page.upcast_ref(), name)?
        .downcast::<gtk4::CheckButton>()
        .ok()
}

/// One grant row's line: the broker's label, the folder, and the
/// promise the person set at grant time, plus whether writes ride
/// without a card. Capability tokens never render.
fn grant_row_text(label: &str, root: &std::path::Path, writable: bool, policy: bool) -> String {
    let mut line = format!(
        "{} · {} · {}",
        label,
        root.display(),
        if writable { "read-write" } else { "read-only" }
    );
    if policy {
        line.push_str(" · writes without a card");
    }
    line
}

/// An approval card's countdown, recomputed at every repaint so the
/// reader sees a live deadline, never a stale one.
fn approval_countdown(deadline: Instant) -> String {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return "Expired".to_owned();
    }
    let secs = left.as_secs();
    if secs < 90 {
        format!("Expires in {secs}s")
    } else {
        format!("Expires in {}m", secs / 60)
    }
}

/// Approval content for the card: the exact proposal text the
/// person reviews, capped for the card with the cap named.
/// Wrapping the whole result in one scrollable view would let a
/// megabyte proposal set the row height; the cap keeps the card
/// a card.
fn preview_content(content: &str) -> String {
    const LIMIT: usize = 2000;
    if content.chars().count() <= LIMIT {
        return content.to_owned();
    }
    let shown: String = content.chars().take(LIMIT).collect();
    format!("{shown}\n…(showing the first {LIMIT} characters)")
}

/// One idea row's group: the collection it belongs to, read off the
/// service's category first, then kind, then kicker. `None` means the
/// service named none and the row stands ungrouped.
pub fn idea_group(item: &ListingItem) -> Option<&str> {
    [&item.category, &item.kind, &item.kicker]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .find(|text| !text.is_empty())
}

/// One idea row's second block: the body's first lines joined to a
/// glance, capped at three lines and 240 chars. Pure, so the
/// truncation is unit-tested.
pub fn idea_snippet(body: Option<&str>) -> String {
    const LIMIT: usize = 240;
    let text: String = body
        .unwrap_or("")
        .lines()
        .take(3)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() <= LIMIT {
        return text;
    }
    format!("{}…", text.chars().take(LIMIT).collect::<String>())
}

/// Held units per idea-laden page: rows carry only their index, so
/// the detail dialog reads the listing (plus cached heroes) from here
/// on selection. Refilled on each visit beside the rows.
struct IdeaStore {
    items: Vec<ListingItem>,
    heroes: Vec<Option<std::path::PathBuf>>,
}

thread_local! {
    static IDEA_STORE: std::cell::RefCell<std::collections::HashMap<String, IdeaStore>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

// Fill generation for the feed: each visit takes the next number so
// the behind-the-paint hero pass touches only its own rows.
thread_local! {
    static FEED_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

// Fill generation for search: each visit takes the next number so
// the behind-the-paint hero pass touches only its own rows.
thread_local! {
    static SEARCH_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Paint one idea row: icon, bold title, clamped description. The
/// row's name carries its index; a press opens the detail dialog.
fn idea_row(item: &ListingItem, index: usize) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.set_widget_name(&format!("idea:{index}"));
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    let icon = gtk4::Label::new(Some(item.icon.as_deref().unwrap_or("💡")));
    icon.add_css_class("muse-feed-icon");
    line.append(&icon);
    let words = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let title = gtk4::Label::new(None);
    title.set_markup(&format!(
        "<b>{}</b>",
        super::chat::render_markup(&item.title)
            .replace("<br/>", " ")
            .replace("<br>", " ")
    ));
    title.set_wrap(true);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    words.append(&title);
    let snippet = idea_snippet(item.body.as_deref());
    if !snippet.is_empty() {
        let sub = gtk4::Label::new(Some(&snippet));
        sub.set_wrap(true);
        sub.set_xalign(0.0);
        sub.set_lines(3);
        sub.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        sub.add_css_class("dim-label");
        words.append(&sub);
    }
    line.append(&words);
    row.set_child(Some(&line));
    row
}

/// What the reader hears after an idea write: acceptance names what
/// happens next, anything else owns that nothing changed.
pub fn execute_notice(accepted: bool) -> &'static str {
    if accepted {
        "Started — find it under Tasks."
    } else {
        "The service did not start it. Nothing changed."
    }
}

/// What the reader hears after a reaction write: the web answers a
/// reaction inline, never with proof, so the words stay modest.
pub fn react_notice(reaction: &str, accepted: bool) -> String {
    if !accepted {
        return "The service did not record it. Nothing changed.".to_owned();
    }
    if reaction == "👎" {
        "Noted — you will not see this again.".to_owned()
    } else {
        "Noted — more like this.".to_owned()
    }
}

/// Open the idea detail dialog: title, close, full description, the
/// service's own action row — an overflow button and the blue
/// "Let's do it". Writes dispatch on press and report acceptance;
/// rows without a service id explain themselves instead of
/// dispatching nothing.
fn open_idea_dialog(ctx: &Ctx, list: &gtk4::ListBox, row: &gtk4::ListBoxRow) {
    let page = list.widget_name().to_string();
    let page = page.strip_suffix("-list").unwrap_or(&page).to_owned();
    let index: usize = match row
        .widget_name()
        .as_str()
        .strip_prefix("idea:")
        .and_then(|id| id.parse().ok())
    {
        Some(index) => index,
        None => return,
    };
    let held = IDEA_STORE.with(|store| {
        store.borrow().get(&page).map(|data| {
            (
                data.items.get(index).cloned(),
                data.heroes.get(index).and_then(|hero| hero.clone()),
            )
        })
    });
    let Some((Some(item), hero)) = held else {
        return;
    };
    if ctx.chat.upgrade().is_none() {
        return;
    }
    let window = gtk4::Window::new();
    window.set_modal(true);
    window.set_default_size(560, -1);
    window.set_title(Some(&item.title));
    if let Some(root) = list
        .root()
        .and_then(|root| root.downcast::<gtk4::Window>().ok())
    {
        window.set_transient_for(Some(&root));
    }
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    let head = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    let title = gtk4::Label::new(None);
    title.set_markup(&format!(
        "<b>{}</b>",
        super::chat::render_markup(&item.title)
            .replace("<br/>", " ")
            .replace("<br>", " ")
    ));
    title.set_wrap(true);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    head.append(&title);
    let close = gtk4::Button::with_label("✕");
    close.add_css_class("flat");
    close.set_valign(gtk4::Align::Start);
    head.append(&close);
    body.append(&head);
    let words = gtk4::Label::new(None);
    words.set_markup(&feed_body_markup(item.body.as_deref().unwrap_or("")));
    words.set_wrap(true);
    words.set_xalign(0.0);
    body.append(&words);
    if let Some(hero) = hero.as_deref() {
        let picture = gtk4::Picture::for_filename(hero);
        picture.set_can_shrink(true);
        picture.set_halign(gtk4::Align::Start);
        body.append(&picture);
    }
    let notice = gtk4::Label::new(None);
    notice.set_wrap(true);
    notice.set_xalign(0.0);
    notice.set_visible(false);
    body.append(&notice);
    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    let more = gtk4::Button::with_label("⋯");
    more.add_css_class("circular");
    actions.append(&more);
    let go = gtk4::Button::with_label("✨ Let's do it");
    go.add_css_class("muse-cta");
    go.set_hexpand(true);
    actions.append(&go);
    body.append(&actions);
    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    scrolled.set_child(Some(&body));
    window.set_child(Some(&scrolled));
    {
        let window = window.clone();
        let list = list.clone();
        close.connect_clicked(move |_| {
            window.close();
            list.select_row(None::<&gtk4::ListBoxRow>);
        });
    }
    // The overflow menu: the web's three verbs, each a real write.
    let popover = gtk4::Popover::new();
    popover.set_parent(&more);
    let menu = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    menu.set_margin_start(8);
    menu.set_margin_end(8);
    menu.set_margin_top(8);
    menu.set_margin_bottom(8);
    let like = gtk4::Button::with_label("👍 More like this");
    like.add_css_class("flat");
    let dislike = gtk4::Button::with_label("👎 Not interested");
    dislike.add_css_class("flat");
    dislike.add_css_class("muse-danger");
    menu.append(&like);
    menu.append(&dislike);
    popover.set_child(Some(&menu));
    {
        let popover = popover.clone();
        more.connect_clicked(move |_| {
            popover.popup();
        });
    }
    // One press, one write: the id is the service's own, carried on
    // the row's unit. No id, no dispatch — a press with nothing to
    // send would be the lie this codebase avoids.
    let item_id = item.id.clone();
    let unit_id = item.id.clone();
    {
        let go_inner = go.clone();
        let notice_inner = notice.clone();
        let window_inner = window.clone();
        let ctx_inner = ctx.clone();
        let page_inner = page.clone();
        go.connect_clicked(move |_| {
            let go = go_inner.clone();
            let notice = notice_inner.clone();
            let window = window_inner.clone();
            let ctx = ctx_inner.clone();
            let page = page_inner.clone();
            let Some(id) = item_id.clone() else {
                notice.set_text("This unit carries no service id, so there is nothing to start.");
                notice.set_visible(true);
                return;
            };
            go.set_sensitive(false);
            go.set_label("Starting…");
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                notice.set_text("Link your account first.");
                notice.set_visible(true);
                go.set_sensitive(true);
                go.set_label("✨ Let's do it");
                return;
            };
            super::app::dispatch(
                &chat,
                move || client.idea_execute(&id),
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                    notice.set_text(execute_notice(accepted));
                    notice.set_visible(true);
                    if accepted {
                        window.close();
                        show(&ctx, &page);
                    } else {
                        go.set_sensitive(true);
                        go.set_label("✨ Let's do it");
                    }
                    let _ = chat;
                },
            );
        });
    }
    // The overflow menu: the web's own verbs. "More like this"
    // records an engagement, "Not interested" records feedback
    // (which dismisses the card) — each a single live-probed
    // write. Proof is the next visit, never the answer alone.
    for (button, glyph) in [(like, "👍"), (dislike, "👎")] {
        let notice_held = notice.clone();
        let popover_held = popover.clone();
        let ctx_held = ctx.clone();
        let page_held = page.clone();
        let window_held = window.clone();
        let unit_held = unit_id.clone();
        button.connect_clicked(move |_| {
            let notice = notice_held.clone();
            let popover = popover_held.clone();
            let ctx = ctx_held.clone();
            let page = page_held.clone();
            let window = window_held.clone();
            let unit = unit_held.clone();
            popover.popdown();
            let Some(id) = unit.clone() else {
                notice.set_text("This unit carries no service id, so there is nothing to record.");
                notice.set_visible(true);
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                notice.set_text("Link your account first.");
                notice.set_visible(true);
                return;
            };
            let negative = glyph == "👎";
            super::app::dispatch(
                &chat,
                {
                    let client = client.clone();
                    let id = id.clone();
                    move || {
                        if negative {
                            client.idea_feedback(&id).map(|answer| answer.accepted)
                        } else {
                            client.idea_engagement(&id).map(|answer| answer.accepted)
                        }
                    }
                },
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| *ok).unwrap_or(false);
                    notice.set_text(&react_notice(glyph, accepted));
                    notice.set_visible(true);
                    if accepted && negative {
                        window.close();
                        show(&ctx, &page);
                    }
                    let _ = chat;
                },
            );
        });
    }
    // The dialog's lower sections read live: "What's included" and
    // "How it works" ride the detail route, not the listing, so
    // they fill in after the dialog presents. A failed read owns
    // that plainly instead of leaving a hole.
    if let (Some(chat), Some(id)) = (ctx.chat.upgrade(), item.id.clone())
        && let Some(client) = chat.client_handle()
    {
        let body_for_done = body.clone();
        super::app::dispatch(
            &chat,
            {
                let client = client.clone();
                move || client.idea_detail(&id)
            },
            move |chat, answer| {
                match answer {
                    Ok(detail) => {
                        if !detail.items.is_empty() {
                            let header = gtk4::Label::new(Some("What's included"));
                            header.set_xalign(0.0);
                            header.add_css_class("muse-section");
                            body_for_done.append(&header);
                            for included in &detail.items {
                                let name = included.title.clone().unwrap_or_default();
                                if !name.trim().is_empty() {
                                    let line = gtk4::Label::new(None);
                                    line.set_markup(&format!(
                                        "<b>{}</b>",
                                        super::chat::render_markup(&name)
                                            .replace("<br/>", " ")
                                            .replace("<br>", " ")
                                    ));
                                    line.set_wrap(true);
                                    line.set_xalign(0.0);
                                    body_for_done.append(&line);
                                }
                                if let Some(summary) = included.summary.clone()
                                    && !summary.trim().is_empty()
                                {
                                    let sub = gtk4::Label::new(Some(&summary));
                                    sub.set_wrap(true);
                                    sub.set_xalign(0.0);
                                    sub.add_css_class("dim-label");
                                    body_for_done.append(&sub);
                                }
                            }
                        }
                        if let Some(how) = detail.content.build_summary.clone()
                            && !how.trim().is_empty()
                        {
                            let header = gtk4::Label::new(Some("How it works"));
                            header.set_xalign(0.0);
                            header.add_css_class("muse-section");
                            body_for_done.append(&header);
                            let words = gtk4::Label::new(None);
                            words.set_markup(&super::chat::render_markup(&how));
                            words.set_wrap(true);
                            words.set_xalign(0.0);
                            body_for_done.append(&words);
                        }
                    }
                    Err(_) => {
                        body_for_done.append(&card_note("Details did not load."));
                    }
                }
                let _ = chat;
            },
        );
    }
    window.present();
}

/// Open one goal's detail dialog: the row's own id travels from the
/// listing, so a press with nothing to read never dispatches.
fn open_goal_dialog(ctx: &Ctx, list: &gtk4::ListBox, row: &gtk4::ListBoxRow) {
    let index: usize = match row
        .widget_name()
        .as_str()
        .strip_prefix("goal:")
        .and_then(|id| id.parse().ok())
    {
        Some(index) => index,
        None => return,
    };
    // The store key follows the list: the Tracking section keeps
    // its own rows beside the main listing's.
    let page = list.widget_name().to_string();
    let page = page.strip_suffix("-list").unwrap_or(&page).to_owned();
    let held = IDEA_STORE.with(|store| {
        store
            .borrow()
            .get(&page)
            .and_then(|data| data.items.get(index).cloned())
    });
    let Some(item) = held else {
        return;
    };
    let Some(goal_id) = item.id.clone() else {
        return;
    };
    open_goal_detail(ctx, list, goal_id);
}

/// Read one goal and present its detail: status, description,
/// updates and links. The parent button re-enters here with the
/// parent's own id: one user press, one read.
fn open_goal_detail(ctx: &Ctx, list: &gtk4::ListBox, goal_id: String) {
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    let Some(client) = chat.client_handle() else {
        return;
    };
    let ctx_owned = ctx.clone();
    let list_owned = list.clone();
    super::app::dispatch(
        &chat,
        {
            let client = client.clone();
            move || client.goal_get(&goal_id)
        },
        move |chat, answer| {
            let Ok(detail) = answer else {
                return;
            };
            let goal = &detail.goal;
            let title = goal.title.clone().unwrap_or_default();
            let window = gtk4::Window::new();
            window.set_modal(true);
            window.set_default_size(560, -1);
            window.set_title(Some(if title.is_empty() {
                "Goal"
            } else {
                title.as_str()
            }));
            if let Some(root) = list_owned
                .root()
                .and_then(|root| root.downcast::<gtk4::Window>().ok())
            {
                window.set_transient_for(Some(&root));
            }
            let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
            body.set_margin_start(24);
            body.set_margin_end(24);
            body.set_margin_top(20);
            body.set_margin_bottom(20);
            let head = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
            let heading = gtk4::Label::new(None);
            heading.set_markup(&format!(
                "<b>{}</b>",
                super::chat::render_markup(&title)
                    .replace("<br/>", " ")
                    .replace("<br>", " ")
            ));
            heading.set_wrap(true);
            heading.set_xalign(0.0);
            heading.set_hexpand(true);
            head.append(&heading);
            let close = gtk4::Button::with_label("✕");
            close.add_css_class("flat");
            close.set_valign(gtk4::Align::Start);
            head.append(&close);
            body.append(&head);
            let status = goal.status.clone().unwrap_or_else(|| "unknown".to_owned());
            let momentum = goal.momentum_status.clone().unwrap_or_default();
            let state = if momentum.trim().is_empty() {
                format!("Status: {status}")
            } else {
                format!("Status: {status} · {momentum}")
            };
            let state_label = gtk4::Label::new(Some(&state));
            state_label.set_xalign(0.0);
            state_label.add_css_class("dim-label");
            body.append(&state_label);
            let words = goal
                .description
                .clone()
                .or_else(|| goal.summary.clone())
                .unwrap_or_default();
            if !words.trim().is_empty() {
                let description = gtk4::Label::new(None);
                description.set_markup(&super::chat::render_markup(&words));
                description.set_wrap(true);
                description.set_xalign(0.0);
                body.append(&description);
            }
            if let Some(parent) = goal.parent_goal_id.clone() {
                let parent_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                let note = gtk4::Label::new(Some("Part of a larger goal."));
                note.set_xalign(0.0);
                note.add_css_class("dim-label");
                note.set_hexpand(true);
                parent_row.append(&note);
                let open = gtk4::Button::with_label("Open parent");
                open.add_css_class("flat");
                parent_row.append(&open);
                body.append(&parent_row);
                let ctx_for_parent = ctx_owned.clone();
                let list_for_parent = list_owned.clone();
                {
                    let window = window.clone();
                    open.connect_clicked(move |_| {
                        window.close();
                        open_goal_detail(&ctx_for_parent, &list_for_parent, parent.clone());
                    });
                }
            }
            if !detail.updates.is_empty() {
                let header = gtk4::Label::new(Some(&format!("Updates ({})", detail.updates.len())));
                header.set_xalign(0.0);
                header.add_css_class("muse-section");
                body.append(&header);
                for update in detail.updates.iter().take(10) {
                    let entry = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
                    let date = update
                        .effective_at
                        .as_deref()
                        .map(|stamp| stamp.chars().take(10).collect::<String>())
                        .unwrap_or_default();
                    let headline = match (update.title.clone().unwrap_or_default(), date) {
                        (text, day) if !text.is_empty() && !day.is_empty() => {
                            format!("{text} — {day}")
                        }
                        (text, _) if !text.is_empty() => text,
                        (_, day) => day,
                    };
                    if !headline.is_empty() {
                        let name = gtk4::Label::new(None);
                        name.set_markup(&format!(
                            "<b>{}</b>",
                            super::chat::render_markup(&headline)
                                .replace("<br/>", " ")
                                .replace("<br>", " ")
                        ));
                        name.set_wrap(true);
                        name.set_xalign(0.0);
                        entry.append(&name);
                    }
                    let snippet =
                        idea_snippet(update.summary.as_deref().or(update.description.as_deref()));
                    if !snippet.is_empty() {
                        let sub = gtk4::Label::new(Some(&snippet));
                        sub.set_wrap(true);
                        sub.set_xalign(0.0);
                        sub.add_css_class("dim-label");
                        entry.append(&sub);
                    }
                    body.append(&entry);
                }
                if detail.updates.len() > 10 {
                    let more = gtk4::Label::new(Some(&format!(
                        "…and {} more.",
                        detail.updates.len() - 10
                    )));
                    more.set_xalign(0.0);
                    more.add_css_class("dim-label");
                    body.append(&more);
                }
            }
            if !detail.associations.is_empty() {
                let header = gtk4::Label::new(Some("Linked"));
                header.set_xalign(0.0);
                header.add_css_class("muse-section");
                body.append(&header);
                for link in &detail.associations {
                    let line = match (
                        link.association_type.clone().unwrap_or_default(),
                        link.title.clone().unwrap_or_default(),
                    ) {
                        (kind, text) if !kind.is_empty() && !text.is_empty() => {
                            format!("{kind} task: {text}")
                        }
                        (kind, _) if !kind.is_empty() => kind,
                        (_, text) => text,
                    };
                    if line.is_empty() {
                        continue;
                    }
                    let row = gtk4::Label::new(Some(&line));
                    row.set_wrap(true);
                    row.set_xalign(0.0);
                    body.append(&row);
                }
            }
            let scrolled = gtk4::ScrolledWindow::new();
            scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
            scrolled.set_vexpand(true);
            scrolled.set_child(Some(&body));
            window.set_child(Some(&scrolled));
            {
                let window = window.clone();
                let list = list_owned.clone();
                close.connect_clicked(move |_| {
                    window.close();
                    list.select_row(None::<&gtk4::ListBoxRow>);
                });
            }
            window.present();
            let _ = chat;
        },
    );
}

/// Fill the tasks page: schedules with a Run button each, plus the run
/// history. "Run now" is single-press like send: the run holds the
/// shared page slot, so a double press waits instead of starting two
/// runs of the same job.
fn fill_tasks(ctx: &Ctx, page: &gtk4::Box) -> bool {
    let Some(schedules) = named_list(page, "tasks-schedules") else {
        return true;
    };
    let Some(runs) = named_list(page, "tasks-runs") else {
        return true;
    };
    let Some(chat) = ctx.chat.upgrade() else {
        return true;
    };
    let ticket = match chat.page_begin(Op::Tasks) {
        Some(ticket) => ticket,
        None => return false,
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return false;
    };
    let chat_strong = chat.clone();
    // Held by value for the answer closure: the web view's entry point waits
    // on a read, and the cache is the rows the paint just showed.
    let page_owned = page.clone();
    dispatch(
        &chat,
        move || client.tasks(),
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let snapshot = match answer {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    // A failed read is kept beside the rows already shown,
                    // so the web view can report it instead of showing an
                    // empty Tasks page.
                    TASK_STORE.with(|store| {
                        store.borrow_mut().error = Some(error.to_string());
                    });
                    return;
                }
            };
            clear_list(&schedules);
            for schedule in &snapshot.schedules {
                let row = gtk4::ListBoxRow::new();
                let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                let label = gtk4::Label::new(Some(&schedule_label(schedule)));
                label.set_hexpand(true);
                label.set_xalign(0.0);
                // Bounded like the run rows: a single-line label's minimum
                // width is its whole text, which can widen the window.
                label.set_wrap(true);
                label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
                label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                label.set_lines(2);
                line.append(&label);
                let run = gtk4::Button::with_label("Run now");
                let job_id = schedule.id.clone();
                let weak = Rc::downgrade(&chat_strong);
                run.connect_clicked(move |_| {
                    let Some(chat) = weak.upgrade() else { return };
                    run_schedule(&chat, &job_id);
                });
                line.append(&run);
                row.set_child(Some(&line));
                schedules.append(&row);
            }
            clear_list(&runs);
            for run in &snapshot.runs {
                // A run's summary can be a paragraph. An unwrapped label takes
                // its whole text as its minimum width, and the window grows
                // with it -- past the screen on this account -- so the row is
                // clamped to two wrapped lines.
                let label = gtk4::Label::new(Some(&run_label(run)));
                label.set_hexpand(true);
                label.set_xalign(0.0);
                label.set_wrap(true);
                label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
                label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                label.set_lines(2);
                runs.append(&label);
            }
            // The web view opens on exactly these rows; the local reminders
            // are re-read from their store when a page or a reply asks.
            tasks_cache(held_tasks_from(&snapshot));
            if let Some(button) = find_widget(page_owned.upcast_ref(), "tasks-web")
                .and_then(|widget| widget.downcast::<gtk4::Button>().ok())
            {
                button.set_sensitive(crate::webui::available());
            }
        },
    );
    true
}

/// One local reminder row: title, the due words the schedule
/// rows use, status, and whether the runner already notified.
/// Pure over an explicit now so the wording stays unit-tested.
fn reminder_row_text(task: &muse_tasks::Task, now_unix_secs: u64) -> String {
    let due = i64::try_from(task.due_unix_secs).unwrap_or(i64::MAX);
    let now = i64::try_from(now_unix_secs).unwrap_or(i64::MAX);
    let status = match task.status {
        muse_tasks::TaskStatus::Pending => "pending",
        muse_tasks::TaskStatus::Paused => "paused",
        muse_tasks::TaskStatus::Done => "done",
        muse_tasks::TaskStatus::Cancelled => "cancelled",
    };
    let mut line = format!(
        "{} · {} · {}",
        task.title,
        next_run_label(Some(due), now),
        status
    );
    if task.notified {
        line.push_str(" · notified");
    }
    line
}

/// Re-read the local reminder store and repaint its list. Local
/// only: the store lock serializes against the runner, so no page
/// ticket is needed and nothing touches the service.
fn refresh_local_tasks(page: &gtk4::Box) -> bool {
    let Some(list) = named_list(page, "tasks-local") else {
        return true;
    };
    clear_list(&list);
    let Some(dir) = muse_tasks::default_store_dir() else {
        list.append(&dim_row("Local reminders need a home directory."));
        return true;
    };
    let store = match muse_tasks::TaskStore::open(&dir) {
        Ok(store) => store,
        Err(_) => {
            list.append(&dim_row("Local reminders unavailable."));
            return true;
        }
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut tasks = store.list();
    tasks.sort_by_key(|task| task.due_unix_secs);
    if tasks.is_empty() {
        list.append(&dim_row("No local reminders."));
    }
    for task in &tasks {
        let row = gtk4::ListBoxRow::new();
        row.set_selectable(false);
        let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let words = gtk4::Label::new(Some(&reminder_row_text(task, now)));
        words.set_hexpand(true);
        words.set_xalign(0.0);
        words.set_wrap(true);
        line.append(&words);
        // Cancellation is permanent and has no undo, so the first
        // press only arms; the second press cancels for good.
        let cancel = gtk4::Button::with_label("Cancel");
        cancel.add_css_class("muse-danger");
        let armed = cancel.clone();
        let id = task.id;
        let page_weak = page.downgrade();
        cancel.connect_clicked(move |_| {
            if armed.label().as_deref() != Some("Confirm cancel") {
                armed.set_label("Confirm cancel");
                return;
            }
            armed.set_label("Cancel");
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if let Some(dir) = muse_tasks::default_store_dir()
                && let Ok(mut store) = muse_tasks::TaskStore::open(&dir)
            {
                let _ = store.cancel(id);
            }
            refresh_local_tasks(&page);
        });
        line.append(&cancel);
        row.set_child(Some(&line));
        list.append(&row);
    }
    true
}

fn dim_row(text: &str) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.set_selectable(false);
    let dim = gtk4::Label::new(Some(text));
    dim.set_xalign(0.0);
    dim.add_css_class("dim-label");
    row.set_child(Some(&dim));
    row
}

/// The new-reminder dialog: title, optional details, and minutes
/// from now. A spin button instead of a date picker: no date
/// parsing to get wrong, and the bounds are the widget's own.
fn show_reminder_dialog(page: &gtk4::Box) {
    let window = gtk4::Window::new();
    window.set_modal(true);
    window.set_default_size(480, -1);
    window.set_title(Some("New reminder"));
    if let Some(root) = page
        .root()
        .and_then(|root| root.downcast::<gtk4::Window>().ok())
    {
        window.set_transient_for(Some(&root));
    }
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    let title = gtk4::Entry::new();
    title.set_placeholder_text(Some("Title (1–200 characters)"));
    body.append(&title);
    let details = gtk4::Entry::new();
    details.set_placeholder_text(Some("Details, optional"));
    body.append(&details);
    let when_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let when_label = gtk4::Label::new(Some("In minutes:"));
    when_row.append(&when_label);
    let minutes = gtk4::SpinButton::with_range(1.0, 525600.0, 1.0);
    minutes.set_value(60.0);
    when_row.append(&minutes);
    body.append(&when_row);
    let note = gtk4::Label::new(None);
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");
    body.append(&note);
    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let create = gtk4::Button::with_label("Create");
    create.add_css_class("muse-cta");
    let cancel = gtk4::Button::with_label("Cancel");
    actions.append(&create);
    actions.append(&cancel);
    body.append(&actions);
    window.set_child(Some(&body));
    {
        let window = window.clone();
        cancel.connect_clicked(move |_| {
            window.close();
        });
    }
    {
        let window = window.clone();
        let page_weak = page.downgrade();
        create.connect_clicked(move |_| {
            let title_text = title.text().to_string();
            let body_text = details.text().to_string();
            let due = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .saturating_add((minutes.value() as u64).saturating_mul(60));
            let created = match muse_tasks::default_store_dir() {
                Some(dir) => match muse_tasks::TaskStore::open(&dir) {
                    Ok(mut store) => {
                        let body = (!body_text.is_empty()).then_some(body_text.as_str());
                        store.create(&title_text, body.unwrap_or(""), due)
                    }
                    Err(_) => Err(muse_tasks::TaskError::Io("store unavailable")),
                },
                None => Err(muse_tasks::TaskError::Io("no home directory")),
            };
            match created {
                Ok(_) => {
                    window.close();
                    if let Some(page) = page_weak.upgrade() {
                        refresh_local_tasks(&page);
                    }
                }
                Err(e) => {
                    note.set_text(&format!("{e}"));
                }
            }
        });
    }
    window.present();
}

/// Start exactly one run of a schedule: the ticket makes a double
/// press wait, and the refill proves what actually started.
fn run_schedule(chat: &Rc<Chat>, job_id: &str) {
    let ticket = match chat.page_begin(Op::Panel) {
        Some(ticket) => ticket,
        None => return,
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return;
    };
    let job_id = job_id.to_owned();
    dispatch(
        chat,
        move || client.tasks_run(&job_id),
        move |chat, answer| {
            // Release the slot whatever the outcome; the runs list
            // refills on the next visit and proves what started.
            chat.page_accept(&ticket);
            let _ = answer;
        },
    );
}

/// Fill the library page: the artifact listing, pinned first.
/// Tabs the library offers: All, Spaces when any space row rides
/// along, then each file kind in visit order. Buttons exist only for
/// data in hand, never for imagined kinds.
pub fn library_tabs(artifacts: &[ArtifactSummary]) -> Vec<String> {
    let mut tabs = vec!["All".to_owned()];
    if artifacts
        .iter()
        .any(|artifact| artifact.target_kind.as_deref() == Some("space"))
    {
        tabs.push("Spaces".to_owned());
    }
    for artifact in artifacts {
        if artifact.target_kind.as_deref() == Some("space") {
            continue;
        }
        if let Some(kind) = artifact.kind.as_deref().filter(|kind| !kind.is_empty())
            && !tabs.contains(&kind.to_owned())
        {
            tabs.push(kind.to_owned());
        }
    }
    tabs
}

/// Whether a library row survives the active tab and Quick Open query.
pub fn library_visible(artifact: &ArtifactSummary, tab: &str, query: &str) -> bool {
    let tab_ok = match tab {
        "All" => true,
        "Spaces" => artifact.target_kind.as_deref() == Some("space"),
        kind => {
            artifact.target_kind.as_deref() != Some("space")
                && artifact.kind.as_deref() == Some(kind)
        }
    };
    tab_ok
        && (query.is_empty()
            || artifact
                .title
                .to_lowercase()
                .contains(&query.to_lowercase()))
}

/// The quiet meta line under a file row: kind and activity, skipping
/// whatever the service did not send.
pub fn artifact_meta(artifact: &ArtifactSummary, now: i64) -> String {
    let mut parts = Vec::new();
    if let Some(kind) = artifact.kind.as_deref().filter(|kind| !kind.is_empty()) {
        parts.push(kind.to_owned());
    }
    let when = relative_time_ms(artifact.updated_at_ms, now);
    if !when.is_empty() {
        parts.push(when);
    }
    parts.join(" · ")
}

/// The active library tab: the button wearing the filter class, All
/// when none does. Read from the buttons, never from side state.
fn active_tab(tabs: &gtk4::Box) -> String {
    let mut child = tabs.first_child();
    while let Some(next) = child {
        if let Ok(button) = next.clone().downcast::<gtk4::Button>()
            && button.has_css_class("muse-filter-active")
        {
            return button
                .label()
                .map(|label| label.to_string())
                .unwrap_or_default();
        }
        child = next.next_sibling();
    }
    "All".to_owned()
}

// Hold the last good artifacts beside the list: tabs and Quick Open
// repaint from here, so filtering never re-reads the wire.
thread_local! {
    static ART_STORE: std::cell::RefCell<Vec<ArtifactSummary>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The service's own total for the listing, not this page's length:
    /// the web view must say "50 of 76", never "50 items".
    static ART_TOTAL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The held Library rows and the service's total, for the web UI slice: it
/// renders exactly what the native page last read, never a second trip to
/// the wire. Empty rows means the native page has not loaded yet -- the
/// web view shows that state, never an empty library.
pub(crate) fn library_snapshot() -> (Vec<ArtifactSummary>, u64) {
    (
        ART_STORE.with(|store| store.borrow().clone()),
        ART_TOTAL.with(|total| total.get()),
    )
}

/// Replace the held rows with a fresher listing the web view read over the
/// wire, so the native page, the boot payload and the channel all agree
/// afterwards. Called on the main loop, from the thread's answer.
pub(crate) fn library_cache(artifacts: Vec<ArtifactSummary>, total: u64) {
    ART_STORE.with(|store| *store.borrow_mut() = artifacts);
    ART_TOTAL.with(|total_cell| total_cell.set(total));
}

/// The Goals page as the web view opens it: the roster and the Tracking
/// section from the last fill, the service's own totals, and the Tracking
/// failure when that section could not be read -- so an unread section is
/// never shown as an empty one.
#[derive(Clone, Debug, Default)]
pub(crate) struct HeldGoals {
    pub goals: Vec<ListingItem>,
    pub goals_total: u64,
    pub tracking: Vec<ListingItem>,
    pub tracking_total: u64,
    pub tracking_error: Option<String>,
}

thread_local! {
    static GOAL_STORE: std::cell::RefCell<HeldGoals> =
        std::cell::RefCell::new(HeldGoals::default());
}

/// The held Goals rows, for the web UI slice: never a second trip to the
/// wire, and empty rows mean the native page has not loaded yet.
pub(crate) fn goals_snapshot() -> HeldGoals {
    GOAL_STORE.with(|store| store.borrow().clone())
}

/// Replace the held Goals rows with a listing the web view read over the
/// wire, so the native page, the boot payload and the channel agree
/// afterwards. Called on the main loop, from the thread's answer.
pub(crate) fn goals_cache(held: HeldGoals) {
    GOAL_STORE.with(|store| *store.borrow_mut() = held);
}

/// The Ideas page as the web view opens it. `loaded` is the difference
/// between an account with no ideas and a page that has not read them, which
/// an empty list alone cannot make.
#[derive(Clone, Debug, Default)]
pub(crate) struct HeldIdeas {
    pub ideas: Vec<ListingItem>,
    pub total: u64,
    pub loaded: bool,
    /// The last read's failure, kept beside whatever rows are held.
    pub error: Option<String>,
}

thread_local! {
    static IDEAS_STORE: std::cell::RefCell<HeldIdeas> =
        std::cell::RefCell::new(HeldIdeas::default());
}

/// The held Ideas rows, for the web UI slice: never a second trip to the
/// wire.
pub(crate) fn ideas_snapshot() -> HeldIdeas {
    IDEAS_STORE.with(|store| store.borrow().clone())
}

/// Replace the held Ideas rows with a listing the web view read over the
/// wire, so the native page, the boot payload and the channel agree
/// afterwards. Called on the main loop, from the thread's answer.
pub(crate) fn ideas_cache(held: HeldIdeas) {
    IDEAS_STORE.with(|store| *store.borrow_mut() = held);
}

/// One Threads row as the web view shows it. The group header, the
/// snippet-and-clock line, the pin order and the side-chat split are all
/// computed here by the helpers the native rows use, so the web page cannot
/// word a row differently from the list beside it.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct ThreadRow {
    pub id: String,
    pub title: String,
    /// The listing's snippet and its clock time, or empty when it named
    /// neither; the day itself lives in the group header.
    pub meta: String,
    pub group: String,
    pub unread: u64,
    pub pinned: bool,
    /// A side chat files under its own header instead of a day group.
    pub side: bool,
}

/// One transcript row as the web page draws it: the same three shapes the
/// native list builds (a day divider, a message, a service notice), carried
/// across with the native values so the two renderers cannot disagree on a
/// word.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct ChatRow {
    /// `day`, `message` or `notice`.
    pub kind: &'static str,
    pub id: String,
    /// `user`, `assistant`, or empty for rows that are not a message.
    pub role: String,
    pub text: String,
    pub day: String,
    pub sent_at: Option<String>,
    pub attachments: Vec<String>,
    pub options: Vec<String>,
    pub selected: Option<String>,
    /// The turn this message answers, named the way the transcript shows
    /// it: the quoted text capped to a line, never a bare id.
    pub reply_to: Option<String>,
}

/// One staged picture as the page's chip reads it: the name, the pixel size
/// and the byte count. The bytes never cross -- a page stages a paste by
/// asking the app, which owns the clipboard, the chips and the dispatch, so
/// there is no second copy of a picture anywhere.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct StagedChip {
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub bytes: usize,
}

/// One outcome word a pending approval offers, as its button reads it: the
/// service's own word plus the native label, so the page cannot invent a
/// sixth outcome or word one of the five differently.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ApprovalChoice {
    pub word: String,
    pub label: String,
}

/// One pending approval as the page's card shows it: the native headline,
/// the service's summary, every detail row verbatim, and the outcomes the
/// app's own card offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ApprovalCard {
    pub id: String,
    pub headline: String,
    pub summary: String,
    pub details: Vec<String>,
    pub choices: Vec<ApprovalChoice>,
}

/// The approvals the native panel last painted, as the page reads them.
/// `loaded` separates "the service reports none waiting" from "nothing has
/// been read yet", and `note` carries the panel's own link-first sentence.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ApprovalsView {
    pub cards: Vec<ApprovalCard>,
    pub loaded: bool,
    pub note: Option<String>,
}

/// The Chat surface as the web view opens it: the open thread, the rows the
/// native transcript last built, and the status line the composer is showing.
/// `loaded` separates a thread nobody has read yet from an empty one.
#[derive(Clone, Debug, Default)]
pub(crate) struct HeldChat {
    pub thread: Option<String>,
    pub title: String,
    pub account: String,
    pub status: Option<String>,
    /// Whether the app has a send in flight: the same flag the native card
    /// swaps Send for Stop on.
    pub sending: bool,
    /// Pictures staged in the app's own composer. A page can add one through
    /// `chat.attach` and drop one through `chat.unstage`; the bytes and the
    /// send stay with the app.
    pub staged: Vec<StagedChip>,
    /// The open thread's unread count from the service listing: the number
    /// the native sidebar badges the row with.
    pub unread: u64,
    /// The pending approvals the native panel last painted.
    pub approvals: ApprovalsView,
    pub rows: Vec<ChatRow>,
    pub loaded: bool,
    pub error: Option<String>,
}

thread_local! {
    static CHAT_STORE: std::cell::RefCell<HeldChat> =
        std::cell::RefCell::new(HeldChat::default());
}

thread_local! {
    static APPROVAL_STORE: std::cell::RefCell<ApprovalsView> =
        std::cell::RefCell::new(ApprovalsView::default());
}

/// The pending approvals the native panel last painted. A cache read like the
/// transcript: the page follows the app's own read, so the two surfaces can
/// never offer different requests.
pub(crate) fn approvals_view() -> ApprovalsView {
    APPROVAL_STORE.with(|store| store.borrow().clone())
}

/// Replace the pending list the page shows. Called from the native paint --
/// and from its unlinked state -- so the page and the panel read one list. A
/// failed read calls nothing, which keeps the rows already held.
pub(crate) fn approvals_cache(view: ApprovalsView) {
    APPROVAL_STORE.with(|store| *store.borrow_mut() = view);
}

/// Project one pending list into the page's cards, reusing the native
/// headline and the native labels for the five outcome words.
pub(crate) fn held_approvals_from(list: &muse_account::PendingApprovals) -> Vec<ApprovalCard> {
    list.pending
        .iter()
        .map(|approval| ApprovalCard {
            id: approval.approval_id.clone(),
            headline: super::app::approval_headline(approval),
            summary: approval.summary.clone(),
            details: approval
                .details
                .iter()
                .map(|detail| format!("{}: {}", detail.label, detail.value))
                .collect(),
            choices: muse_account::Decision::all()
                .into_iter()
                .map(|decision| ApprovalChoice {
                    word: decision.as_str().to_owned(),
                    label: super::app::decision_label(decision).to_owned(),
                })
                .collect(),
        })
        .collect()
}

/// The transcript the native list last painted. A cache read by construction:
/// the page follows the app's own reads, so the two can never show different
/// conversations. The approvals are merged in from their own store, which the
/// panel paints, so a render never has to know about them.
pub(crate) fn chat_snapshot() -> HeldChat {
    let held = CHAT_STORE.with(|store| store.borrow().clone());
    HeldChat {
        approvals: approvals_view(),
        ..held
    }
}

/// Replace the held transcript. Called on every native render, including the
/// in-place rewrite of a growing reply, so a page following `chat.snapshot`
/// sees the words arrive instead of the first chunk.
pub(crate) fn chat_cache(held: HeldChat) {
    CHAT_STORE.with(|store| *store.borrow_mut() = held);
}

/// Project the rows `chat::build_rows` produced into the page's rows. Pure, so
/// the day dividers, notices and option pills can be checked without a window.
pub(crate) fn held_chat_from(rows: &[super::chat::TranscriptRow]) -> Vec<ChatRow> {
    use super::chat::{Role, TranscriptRow};
    rows.iter()
        .map(|row| match row {
            TranscriptRow::DayDivider { label } => ChatRow {
                kind: "day",
                day: label.clone(),
                ..ChatRow::default()
            },
            TranscriptRow::Notice { text } => ChatRow {
                kind: "notice",
                text: text.clone(),
                ..ChatRow::default()
            },
            TranscriptRow::Message {
                id,
                role,
                text,
                attachments,
                options,
                selected_option,
                sent_at,
                reply_to,
            } => ChatRow {
                kind: "message",
                id: id.clone(),
                role: match role {
                    Role::User => "user",
                    _ => "assistant",
                }
                .to_owned(),
                text: text.clone(),
                sent_at: sent_at.clone(),
                attachments: attachments
                    .iter()
                    .map(|file| match file.kind.as_deref() {
                        Some(kind) if !kind.is_empty() => {
                            format!("{} · {kind}", file.filename)
                        }
                        _ => file.filename.clone(),
                    })
                    .collect(),
                options: options.clone(),
                selected: selected_option.clone(),
                reply_to: reply_to.clone(),
                day: String::new(),
            },
        })
        .collect()
}

/// The Threads page as the web view opens it. `loaded` separates an account
/// with no conversations from a page that has not read the listing yet.
#[derive(Clone, Debug, Default)]
pub(crate) struct HeldThreads {
    pub threads: Vec<ThreadRow>,
    pub total: u64,
    pub loaded: bool,
    /// The last read's failure, kept beside whatever rows are held.
    pub error: Option<String>,
}

thread_local! {
    static THREAD_STORE: std::cell::RefCell<HeldThreads> =
        std::cell::RefCell::new(HeldThreads::default());
}

/// The held Threads rows, for the web UI slice: never a second trip to the
/// wire.
pub(crate) fn threads_snapshot() -> HeldThreads {
    THREAD_STORE.with(|store| store.borrow().clone())
}

/// Replace the held Threads rows with a listing the native paint or a wire
/// read produced, so the page, the boot payload and the channel agree.
pub(crate) fn threads_cache(held: HeldThreads) {
    THREAD_STORE.with(|store| *store.borrow_mut() = held);
}

/// Project a thread listing into the page's rows: pinned first (the listing's
/// own order inside each half), day groups for the main list, side chats
/// apart. `today` is the caller's, so the labels are testable.
pub(crate) fn held_threads_from(threads: &[muse_account::Thread], today: &str) -> HeldThreads {
    let mut rows: Vec<ThreadRow> = threads
        .iter()
        .map(|thread| ThreadRow {
            id: thread.session_id.clone(),
            title: if thread.title.is_empty() {
                "(untitled)".to_owned()
            } else {
                thread.title.clone()
            },
            meta: thread_meta(thread.snippet.as_deref(), thread.updated_at.as_deref()),
            group: thread_group_label(thread.updated_at.as_deref(), today),
            unread: thread.unread_count,
            pinned: thread.pinned,
            side: !thread.is_primary,
        })
        .collect();
    // Stable: a pinned row keeps the listing's order among its peers.
    rows.sort_by_key(|row| !row.pinned);
    HeldThreads {
        total: rows.len() as u64,
        threads: rows,
        loaded: true,
        error: None,
    }
}

/// One Search result as the web view shows it: the conversation's id (the
/// native row opens it in Chat), and the native label's parts - icon, title,
/// then the kicker/kind/stamp line `feed_item_label` joins. Rows without an
/// id are dropped, exactly as the native paint drops them.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct SearchRow {
    pub id: String,
    pub icon: String,
    pub title: String,
    pub meta: String,
    pub image_url: String,
}

/// The Search page as the web view opens it: the last query the native page
/// ran, and the rows it painted.
#[derive(Clone, Debug, Default)]
pub(crate) struct HeldSearch {
    pub query: String,
    pub rows: Vec<SearchRow>,
    pub total: u64,
    pub loaded: bool,
    pub error: Option<String>,
}

thread_local! {
    static SEARCH_STORE: std::cell::RefCell<HeldSearch> =
        std::cell::RefCell::new(HeldSearch::default());
}

/// The held search results, for the web UI slice: the query travels with
/// them, so the page shows what was searched, not just what came back.
pub(crate) fn search_snapshot() -> HeldSearch {
    SEARCH_STORE.with(|store| store.borrow().clone())
}

/// Replace the held search with what a paint just produced.
pub(crate) fn search_cache(held: HeldSearch) {
    SEARCH_STORE.with(|store| *store.borrow_mut() = held);
}

/// One policy row on the settings page: the service's own label and value,
/// plus the flip the chat panel offers for it. The page is read-only, so the
/// flip is named and never offered -- the words travel as data.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct PolicyRowView {
    pub label: String,
    pub value: String,
    pub flip: Option<String>,
}

/// The policy projection the settings page carries: what the chat panel last
/// read, with the connector names whose grants can be reset there.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct PolicyView {
    pub loaded: bool,
    pub rows: Vec<PolicyRowView>,
    pub connectors: Vec<String>,
    pub withheld: u64,
    pub note: Option<String>,
}

/// The saved account link as the settings page shows it. Paths only: the
/// session jar's contents never reach a page.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct LinkView {
    pub linked: bool,
    pub interpreter: String,
    pub session_folder: String,
}

/// The updater's last state as the settings page shows it. The controls that
/// write (check, download, restart) stay on the native page, so the page says
/// what the last check found instead of offering them again.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct UpdatesView {
    pub appimage: bool,
    pub status: String,
    pub busy: bool,
    pub checked: bool,
    pub staged: bool,
}

/// The Settings page as the web view opens it: three things the page cannot
/// read on its own, each already held by the native side.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct HeldSettings {
    pub link: LinkView,
    pub updates: UpdatesView,
    pub policy: PolicyView,
}

thread_local! {
    static SETTINGS_UPDATES: std::cell::RefCell<UpdatesView> =
        std::cell::RefCell::new(UpdatesView::default());
    static SETTINGS_POLICY: std::cell::RefCell<PolicyView> =
        std::cell::RefCell::new(PolicyView::default());
}

/// The settings surface as the web view opens it: the link this clone saved,
/// the updater's last answer, and the policy the chat panel last read. The
/// link is read now (it is one small file); the other two come from their own
/// paints, so the page never claims a read the app has not made.
pub(crate) fn settings_snapshot() -> HeldSettings {
    let link = match super::session::load_saved_link() {
        Some(link) => LinkView {
            linked: true,
            interpreter: link.python.display().to_string(),
            session_folder: link.config_dir.display().to_string(),
        },
        None => LinkView::default(),
    };
    HeldSettings {
        link,
        updates: SETTINGS_UPDATES.with(|store| store.borrow().clone()),
        policy: SETTINGS_POLICY.with(|store| store.borrow().clone()),
    }
}

/// Hold the updater's state beside the panel that paints it, so the page and
/// the native section read the same answer.
pub(crate) fn settings_updates_cache(view: UpdatesView) {
    SETTINGS_UPDATES.with(|store| *store.borrow_mut() = view);
}

/// Hold the policy the chat panel just painted. `flip` is the panel's own
/// table of accepted flips, so the page names exactly what a press there
/// would offer.
pub(crate) fn held_policy_from(
    policy: &muse_account::PolicySettings,
    flip: impl Fn(&str, &str) -> Option<String>,
) -> PolicyView {
    PolicyView {
        loaded: true,
        rows: policy
            .entries
            .iter()
            .map(|entry| PolicyRowView {
                label: entry.label.clone(),
                value: entry.value.clone(),
                flip: flip(&entry.label, &entry.value),
            })
            .collect(),
        connectors: policy.connectors.clone(),
        withheld: policy.withheld,
        note: None,
    }
}

/// Hold a policy projection, including the link-first and failed states the
/// panel paints instead of a read.
pub(crate) fn settings_policy_cache(view: PolicyView) {
    SETTINGS_POLICY.with(|store| *store.borrow_mut() = view);
}

/// Project one search listing into the page's rows, with the native wording.
pub(crate) fn held_search_from(query: &str, listing: &muse_account::ServiceListing) -> HeldSearch {
    let rows = listing
        .items
        .iter()
        .filter_map(|item| {
            let id = item.id.clone().filter(|id| !id.is_empty())?;
            let meta: Vec<&str> = [&item.kicker, &item.kind, &item.stamp]
                .into_iter()
                .flatten()
                .map(String::as_str)
                .filter(|text| !text.is_empty())
                .collect();
            Some(SearchRow {
                id,
                icon: item
                    .icon
                    .as_deref()
                    .filter(|icon| !icon.is_empty())
                    .unwrap_or("🔎")
                    .to_owned(),
                title: item.title.clone(),
                meta: meta.join(" \u{00b7} "),
                image_url: item.image_url.clone().unwrap_or_default(),
            })
        })
        .collect();
    HeldSearch {
        query: query.to_owned(),
        rows,
        total: listing.total,
        loaded: true,
        error: None,
    }
}

/// One Feed card as the web view shows it. The meta line, the relative
/// time, the action label and the first-screenful rule for live graphics
/// are the native page's own, so the two renderers cannot drift.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct FeedCard {
    pub id: String,
    pub icon: String,
    pub meta: String,
    pub title: String,
    pub when: String,
    pub body: String,
    /// The service's own action label on actionable units.
    pub action: String,
    /// Why the service showed this unit, for the card's own line.
    pub why: String,
    /// The hero's address; the page asks `feed.hero` for its bytes rather
    /// than carrying megabytes in the boot payload.
    pub image_url: String,
    /// Service markup for the card's graphic, and the extracted text the
    /// native page falls back to when the markup cannot render.
    pub widget_html: String,
    pub widget_text: String,
    /// True for the first two graphical cards, which the native paint
    /// renders live; the rest read as their extracted text.
    pub live: bool,
}

/// The Feed page as the web view opens it.
#[derive(Clone, Debug, Default)]
pub(crate) struct HeldFeed {
    pub cards: Vec<FeedCard>,
    pub prompt: String,
    pub status: String,
    pub total: u64,
    pub loaded: bool,
    /// The last read's failure, kept beside whatever cards are held.
    pub error: Option<String>,
}

thread_local! {
    static FEED_STORE: std::cell::RefCell<HeldFeed> =
        std::cell::RefCell::new(HeldFeed::default());
}

/// The held Feed cards, for the web UI slice: never a second trip to the
/// wire, and never a second download of a hero.
pub(crate) fn feed_snapshot() -> HeldFeed {
    FEED_STORE.with(|store| store.borrow().clone())
}

/// Replace the held Feed cards with a listing the native paint produced.
/// The prompt and the generation line travel with the cards, as they do on
/// the native page.
pub(crate) fn feed_cache(held: HeldFeed) {
    FEED_STORE.with(|store| *store.borrow_mut() = held);
}

/// Project a feed listing into the page's cards, with the native wording.
/// `now` is the caller's clock, so the relative times are testable.
pub(crate) fn held_feed_from(
    listing: &muse_account::ServiceListing,
    now: i64,
    graphics: bool,
) -> HeldFeed {
    let mut live = 0;
    let cards = listing
        .items
        .iter()
        .map(|item| {
            let frame = item
                .widget_html
                .as_deref()
                .is_some_and(|html| !html.is_empty());
            if graphics && frame {
                live += 1;
            }
            FeedCard {
                id: item.id.clone().unwrap_or_default(),
                icon: item
                    .icon
                    .as_deref()
                    .filter(|icon| !icon.is_empty())
                    .unwrap_or("📰")
                    .to_owned(),
                meta: feed_meta_line(item).unwrap_or_default(),
                title: item.title.clone(),
                when: relative_time(item.stamp.as_deref(), now),
                // The native body markup: escaped text with its links
                // (and bold) already resolved, so both renderers show
                // the same words and the page needs no Markdown parser.
                body: feed_body_markup(item.body.as_deref().unwrap_or_default()),
                action: feed_row_cta(item).unwrap_or_default(),
                why: item.why.clone().unwrap_or_default(),
                image_url: item.image_url.clone().unwrap_or_default(),
                widget_html: item.widget_html.clone().unwrap_or_default(),
                widget_text: item.widget_text.clone().unwrap_or_default(),
                live: graphics && frame && live <= 2,
            }
        })
        .collect();
    HeldFeed {
        cards,
        prompt: listing
            .feed_prompt
            .as_ref()
            .and_then(|prompt| prompt.text.clone())
            .unwrap_or_default(),
        status: feed_status_line(listing.feed_status.as_ref()).unwrap_or_default(),
        total: listing.total,
        loaded: true,
        error: None,
    }
}

/// Fill the web cache from a listing the native page just painted.
fn cache_feed_for_web(listing: &muse_account::ServiceListing) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs() as i64)
        .unwrap_or(0);
    feed_cache(held_feed_from(
        listing,
        now,
        super::widget::widget_available(),
    ));
}

/// One Tasks row as the web view shows it: the native page's own words, so
/// the two renderers cannot drift on state, status or due wording.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct TaskRow {
    pub id: String,
    pub label: String,
}

/// The Tasks page as the web view opens it. `loaded` is the difference
/// between an empty account and a page that has not read it yet, which an
/// empty list alone cannot make.
#[derive(Clone, Debug, Default)]
pub(crate) struct HeldTasks {
    pub schedules: Vec<TaskRow>,
    pub runs: Vec<TaskRow>,
    pub local: Vec<TaskRow>,
    pub loaded: bool,
    /// The last read's failure, kept beside whatever rows are held.
    pub error: Option<String>,
}

thread_local! {
    static TASK_STORE: std::cell::RefCell<HeldTasks> =
        std::cell::RefCell::new(HeldTasks::default());
}

/// The service rows in the native paint's own labels: schedule title plus
/// its on/off word, run status plus its summary.
pub(crate) fn held_tasks_from(snapshot: &muse_account::TasksSnapshot) -> HeldTasks {
    HeldTasks {
        schedules: snapshot
            .schedules
            .iter()
            .map(|schedule| TaskRow {
                id: schedule.id.clone(),
                label: schedule_label(schedule),
            })
            .collect(),
        runs: snapshot
            .runs
            .iter()
            .map(|run| TaskRow {
                id: run.run_id.clone(),
                label: run_label(run),
            })
            .collect(),
        local: Vec::new(),
        loaded: true,
        error: None,
    }
}

/// The held Tasks rows. Empty rows and `loaded: false` mean the native page
/// has not read the service yet, never that the account has no schedules.
pub(crate) fn tasks_snapshot() -> HeldTasks {
    TASK_STORE.with(|store| store.borrow().clone())
}

/// Replace the held Tasks rows with a read of the service, keeping the local
/// reminders the answer did not carry: those are local state, not the wire's.
pub(crate) fn tasks_cache(held: HeldTasks) {
    TASK_STORE.with(|store| {
        let mut current = store.borrow_mut();
        let local = std::mem::take(&mut current.local);
        *current = held;
        if current.local.is_empty() {
            current.local = local;
        }
    });
}

/// The local reminders in the native page's own words, earliest due first.
fn local_task_rows() -> Vec<TaskRow> {
    let Some(dir) = muse_tasks::default_store_dir() else {
        return Vec::new();
    };
    let Ok(store) = muse_tasks::TaskStore::open(&dir) else {
        return Vec::new();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut tasks = store.list();
    tasks.sort_by_key(|task| task.due_unix_secs);
    tasks
        .iter()
        .map(|task| TaskRow {
            id: task.id.to_string(),
            label: reminder_row_text(task, now),
        })
        .collect()
}

/// Re-read the local reminders into the held Tasks rows. Local state, so no
/// wire trip: a page open shows the store as it is now.
pub(crate) fn tasks_local_refresh() {
    let local = local_task_rows();
    TASK_STORE.with(|store| store.borrow_mut().local = local);
}

/// Paint the library from held rows: tab bar, Spaces and Recent
/// sections on All, flat rows under a filter. The tab and query ride
/// in so every repaint is one call.
fn paint_library(
    ctx: &Ctx,
    list: &gtk4::ListBox,
    tabs: &gtk4::Box,
    entry: &gtk4::Entry,
    artifacts: &[ArtifactSummary],
    tab: &str,
    query: &str,
) {
    clear_list(list);
    while let Some(child) = tabs.first_child() {
        tabs.remove(&child);
    }
    for name in library_tabs(artifacts) {
        let button = gtk4::Button::with_label(&name);
        button.add_css_class("flat");
        if name == tab {
            button.add_css_class("muse-filter-active");
        }
        let ctx_inner = ctx.clone();
        let list_inner = list.clone();
        let tabs_inner = tabs.clone();
        let entry_inner = entry.clone();
        let artifacts_inner = artifacts.to_vec();
        let name_inner = name.clone();
        button.connect_clicked(move |_| {
            let query = entry_inner.text().to_string();
            paint_library(
                &ctx_inner,
                &list_inner,
                &tabs_inner,
                &entry_inner,
                &artifacts_inner,
                &name_inner,
                &query,
            );
        });
        tabs.append(&button);
    }
    let spaces: Vec<&ArtifactSummary> = artifacts
        .iter()
        .filter(|artifact| library_visible(artifact, tab, query))
        .filter(|artifact| artifact.target_kind.as_deref() == Some("space"))
        .collect();
    let files: Vec<&ArtifactSummary> = artifacts
        .iter()
        .filter(|artifact| library_visible(artifact, tab, query))
        .filter(|artifact| artifact.target_kind.as_deref() != Some("space"))
        .collect();
    if tab == "All" {
        if !spaces.is_empty() {
            let head = gtk4::Label::new(Some("Spaces"));
            head.set_halign(gtk4::Align::Start);
            head.add_css_class("muse-section");
            list.append(&head);
        }
        for artifact in &spaces {
            list.append(&space_row(ctx, artifact));
        }
        if !files.is_empty() {
            let head = gtk4::Label::new(Some("Recent"));
            head.set_halign(gtk4::Align::Start);
            head.add_css_class("muse-section");
            list.append(&head);
        }
        for artifact in &files {
            list.append(&file_row(ctx, artifact));
        }
    } else {
        for artifact in spaces.iter().chain(files.iter()) {
            if artifact.target_kind.as_deref() == Some("space") {
                list.append(&space_row(ctx, artifact));
            } else {
                list.append(&file_row(ctx, artifact));
            }
        }
    }
    if spaces.is_empty() && files.is_empty() {
        empty_note(list, "Nothing here yet.");
    }
}

/// One space row: title plus recency, with the two writes the protocol
/// offers. Export reports what came back; unshare reports acceptance.
/// Open is omitted: its mechanism was never observed.
fn space_row(ctx: &Ctx, artifact: &ArtifactSummary) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    let words = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let title = gtk4::Label::new(None);
    title.set_markup(&format!(
        "<b>{}</b>",
        super::chat::render_markup(&artifact.title)
            .replace("<br/>", " ")
            .replace("<br>", " ")
    ));
    title.set_wrap(true);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    words.append(&title);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs() as i64)
        .unwrap_or(0);
    let mut meta = vec!["Space".to_owned()];
    let when = relative_time_ms(artifact.updated_at_ms, now);
    if !when.is_empty() {
        meta.push(when);
    }
    let sub = gtk4::Label::new(Some(&meta.join(" · ")));
    sub.set_xalign(0.0);
    sub.add_css_class("dim-label");
    words.append(&sub);
    line.append(&words);
    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    line.append(&spacer);
    let export = gtk4::Button::with_label("Export");
    export.add_css_class("flat");
    export.set_valign(gtk4::Align::Start);
    line.append(&export);
    // The listing names which spaces may be shared; the service
    // decides anyway, so a row without the bit offers no button.
    // "cloudflare" is the share_type the service named itself on
    // a wrong value, sent on both presses of the round-trip.
    let share: Option<gtk4::Button> = artifact.can_share.then(|| {
        let share = gtk4::Button::with_label("Share");
        share.add_css_class("flat");
        share.set_valign(gtk4::Align::Start);
        share
    });
    if let Some(share) = share.as_ref() {
        line.append(share);
    }
    let unshare = gtk4::Button::with_label("Unshare");
    unshare.add_css_class("flat");
    unshare.add_css_class("muse-danger");
    unshare.set_valign(gtk4::Align::Start);
    line.append(&unshare);
    row.set_child(Some(&line));
    let slug = artifact.slug.clone();
    let name = artifact.title.clone();
    {
        let ctx = ctx.clone();
        let line_inner = line.clone();
        let slug_inner = slug.clone();
        let name_inner = name.clone();
        export.connect_clicked(move |_| {
            let Some(slug) = slug_inner.clone() else {
                line_inner.append(&card_note("This space carries no handle."));
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let line_for_done = line_inner.clone();
            let name_for_done = name_inner.clone();
            super::app::dispatch(
                &chat,
                move || client.space_export(&slug),
                move |chat, answer| {
                    match answer {
                        Ok(exported) => {
                            show_export_dialog(&line_for_done, &name_for_done, &exported);
                        }
                        Err(error) => {
                            line_for_done.append(&card_note(&format!("Export failed: {error}")));
                        }
                    }
                    let _ = chat;
                },
            );
        });
    }
    if let Some(share) = share.clone() {
        let ctx = ctx.clone();
        let line_inner = line.clone();
        let slug_inner = slug.clone();
        share.connect_clicked(move |_| {
            let Some(slug) = slug_inner.clone() else {
                line_inner.append(&card_note("This space carries no handle."));
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let line_for_done = line_inner.clone();
            let ctx_for_done = ctx.clone();
            super::app::dispatch(
                &chat,
                move || client.space_share(&slug, "cloudflare", None),
                move |chat, answer| {
                    match answer {
                        Ok(shared) if shared.accepted && shared.url.is_some() => {
                            line_for_done.append(&card_note(&format!(
                                "Published: {}",
                                shared.url.as_deref().unwrap_or("")
                            )));
                        }
                        Ok(shared) if shared.accepted && shared.needs_disclosure => {
                            show_share_confirm_dialog(&line_for_done, &ctx_for_done, &shared);
                        }
                        _ => {
                            line_for_done.append(&card_note(
                                "The service did not share it. Nothing changed.",
                            ));
                        }
                    }
                    let _ = chat;
                },
            );
        });
    }
    {
        let ctx = ctx.clone();
        let line_inner = line.clone();
        unshare.connect_clicked(move |_| {
            let Some(slug) = slug.clone() else {
                line_inner.append(&card_note("This space carries no handle."));
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let line_for_done = line_inner.clone();
            let ctx_for_done = ctx.clone();
            super::app::dispatch(
                &chat,
                move || client.space_unshare(&slug),
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                    if accepted {
                        show(&ctx_for_done, "library");
                    } else {
                        line_for_done.append(&card_note(
                            "The service did not unshare it. Nothing changed.",
                        ));
                    }
                    let _ = chat;
                },
            );
        });
    }
    row
}

/// The export result, read-only: what came back and how big, with the
/// page text capped and the cap stated.
fn show_export_dialog(line: &gtk4::Box, name: &str, exported: &muse_account::SpaceExportAnswer) {
    let window = gtk4::Window::new();
    window.set_modal(true);
    window.set_default_size(560, -1);
    window.set_title(Some(name));
    if let Some(root) = line
        .root()
        .and_then(|root| root.downcast::<gtk4::Window>().ok())
    {
        window.set_transient_for(Some(&root));
    }
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    let head = gtk4::Label::new(Some(&format!(
        "{} · {} bytes",
        exported.format, exported.size
    )));
    head.set_xalign(0.0);
    head.add_css_class("title-4");
    body.append(&head);
    if let Some(html) = exported.html.as_deref() {
        const LIMIT: usize = 4000;
        let shown = html.chars().take(LIMIT).collect::<String>();
        let words = gtk4::Label::new(Some(&shown));
        words.set_wrap(true);
        words.set_xalign(0.0);
        words.set_selectable(true);
        body.append(&words);
        if html.chars().count() > LIMIT {
            let capped =
                gtk4::Label::new(Some(&format!("…(showing the first {LIMIT} characters)")));
            capped.set_xalign(0.0);
            capped.add_css_class("dim-label");
            body.append(&capped);
        }
    }
    let close = gtk4::Button::with_label("Close");
    close.add_css_class("muse-cta");
    body.append(&close);
    window.set_child(Some(&body));
    {
        let window = window.clone();
        close.connect_clicked(move |_| {
            window.close();
        });
    }
    window.present();
}

/// The Publish round-trip behind a space row's Share button: the
/// service answered the first press with the disclosure the reader
/// must accept instead of a link. The summary and flag names are
/// the service's own words about what publishing exposes; Publish
/// hands the confirmation object back verbatim and Cancel changes
/// nothing. The token inside is never display text.
fn show_share_confirm_dialog(line: &gtk4::Box, ctx: &Ctx, shared: &muse_account::SpaceShareAnswer) {
    let window = gtk4::Window::new();
    window.set_modal(true);
    window.set_default_size(480, -1);
    window.set_title(Some(&format!("Share {}", shared.slug)));
    if let Some(root) = line
        .root()
        .and_then(|root| root.downcast::<gtk4::Window>().ok())
    {
        window.set_transient_for(Some(&root));
    }
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    let head = gtk4::Label::new(Some("Before publishing, review what becomes public:"));
    head.set_xalign(0.0);
    head.add_css_class("title-4");
    body.append(&head);
    if let Some(summary) = shared.disclosure_summary.as_deref() {
        let words = gtk4::Label::new(Some(summary));
        words.set_wrap(true);
        words.set_xalign(0.0);
        body.append(&words);
    }
    for flag in shared.flag_names() {
        let item = gtk4::Label::new(Some(&format!("\u{2022} {flag}")));
        item.set_xalign(0.0);
        item.add_css_class("dim-label");
        body.append(&item);
    }
    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let publish = gtk4::Button::with_label("Publish");
    publish.add_css_class("muse-cta");
    let cancel = gtk4::Button::with_label("Cancel");
    actions.append(&publish);
    actions.append(&cancel);
    body.append(&actions);
    window.set_child(Some(&body));
    {
        let window = window.clone();
        cancel.connect_clicked(move |_| {
            window.close();
        });
    }
    {
        let window = window.clone();
        let line_for_done = line.clone();
        let ctx = ctx.clone();
        let slug = shared.slug.clone();
        let share_type = shared
            .share_type
            .clone()
            .unwrap_or_else(|| "cloudflare".to_owned());
        let confirmation = shared.disclosure_confirmation.clone();
        publish.connect_clicked(move |_| {
            let slug = slug.clone();
            let share_type = share_type.clone();
            let confirmation = confirmation.clone();
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_for_done.append(&card_note("Link your account first."));
                return;
            };
            let window_for_done = window.clone();
            let line_for_done = line_for_done.clone();
            super::app::dispatch(
                &chat,
                move || client.space_share(&slug, &share_type, confirmation.clone()),
                move |chat, answer| {
                    window_for_done.close();
                    match answer {
                        Ok(shared) if shared.accepted && shared.url.is_some() => {
                            line_for_done.append(&card_note(&format!(
                                "Published: {}",
                                shared.url.as_deref().unwrap_or("")
                            )));
                        }
                        _ => {
                            line_for_done.append(&card_note(
                                "The service did not share it. Nothing changed.",
                            ));
                        }
                    }
                    let _ = chat;
                },
            );
        });
    }
    window.present();
}

/// One file row: title plus kind and recency, with the web's own ⋯
/// verbs, observed live in the user's session: pin or unpin, and
/// delete. Pin toggles through the live-probed artifacts.pin body;
/// delete asks first, then dispatches the live-probed fs.delete
/// body. Both re-read the Library on acceptance, so the row's new
/// state is proven, not assumed.
fn file_row(ctx: &Ctx, artifact: &ArtifactSummary) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    let words = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let title = gtk4::Label::new(None);
    title.set_markup(&format!(
        "<b>{}</b>",
        super::chat::render_markup(&artifact.title)
            .replace("<br/>", " ")
            .replace("<br>", " ")
    ));
    title.set_wrap(true);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    words.append(&title);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs() as i64)
        .unwrap_or(0);
    let meta = artifact_meta(artifact, now);
    if !meta.is_empty() {
        let sub = gtk4::Label::new(Some(&meta));
        sub.set_xalign(0.0);
        sub.add_css_class("dim-label");
        words.append(&sub);
    }
    line.append(&words);
    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    line.append(&spacer);
    let more = gtk4::Button::with_label("⋯");
    more.add_css_class("flat");
    more.add_css_class("circular");
    more.set_valign(gtk4::Align::Start);
    describe(&more, "File options");
    line.append(&more);
    row.set_child(Some(&line));
    let popover = gtk4::Popover::new();
    popover.set_parent(&more);
    let menu = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    menu.set_margin_start(8);
    menu.set_margin_end(8);
    menu.set_margin_top(8);
    menu.set_margin_bottom(8);
    let pin_label = if artifact.pinned { "Unpin" } else { "Pin" };
    let pin_button = gtk4::Button::with_label(pin_label);
    pin_button.add_css_class("flat");
    menu.append(&pin_button);
    let delete_button = gtk4::Button::with_label("Delete");
    delete_button.add_css_class("flat");
    delete_button.add_css_class("muse-danger");
    menu.append(&delete_button);
    popover.set_child(Some(&menu));
    {
        let ctx = ctx.clone();
        let line_inner = line.clone();
        let popover_held = popover.clone();
        let artifact_ref = artifact.artifact_ref.clone();
        let pinned = artifact.pinned;
        pin_button.connect_clicked(move |_| {
            popover_held.popdown();
            if artifact_ref.is_empty() {
                line_inner.append(&card_note("This row carries no service reference."));
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let line_for_done = line_inner.clone();
            let ctx_for_done = ctx.clone();
            super::app::dispatch(
                &chat,
                {
                    let client = client.clone();
                    let artifact_ref = artifact_ref.clone();
                    move || client.artifact_pin(&artifact_ref, !pinned)
                },
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                    if accepted {
                        show(&ctx_for_done, "library");
                    } else {
                        line_for_done.append(&card_note(
                            "The service did not change it. Nothing changed.",
                        ));
                    }
                    let _ = chat;
                },
            );
        });
    }
    {
        let ctx = ctx.clone();
        let line_inner = line.clone();
        let popover_held = popover.clone();
        let path = artifact.path.clone();
        let name = artifact.title.clone();
        delete_button.connect_clicked(move |_| {
            popover_held.popdown();
            let Some(path) = path.clone() else {
                line_inner.append(&card_note("This row carries no service path."));
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let window = gtk4::Window::new();
            window.set_modal(true);
            window.set_default_size(420, -1);
            window.set_title(Some("Delete file"));
            if let Some(root) = line_inner
                .root()
                .and_then(|root| root.downcast::<gtk4::Window>().ok())
            {
                window.set_transient_for(Some(&root));
            }
            let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
            body.set_margin_start(24);
            body.set_margin_end(24);
            body.set_margin_top(20);
            body.set_margin_bottom(20);
            let words = gtk4::Label::new(Some(&format!(
                "Delete “{}” from your library? This cannot be undone.",
                name
            )));
            words.set_wrap(true);
            words.set_xalign(0.0);
            body.append(&words);
            let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let cancel = gtk4::Button::with_label("Cancel");
            cancel.add_css_class("flat");
            buttons.append(&cancel);
            let confirm = gtk4::Button::with_label("Delete");
            confirm.add_css_class("muse-danger");
            buttons.append(&confirm);
            body.append(&buttons);
            window.set_child(Some(&body));
            {
                let window = window.clone();
                cancel.connect_clicked(move |_| {
                    window.close();
                });
            }
            {
                let window = window.clone();
                let line_for_done = line_inner.clone();
                let ctx_for_done = ctx.clone();
                confirm.connect_clicked(move |_| {
                    window.close();
                    let line_for_done = line_for_done.clone();
                    let ctx_for_done = ctx_for_done.clone();
                    super::app::dispatch(
                        &chat,
                        {
                            let client = client.clone();
                            let path = path.clone();
                            move || client.file_delete(&path)
                        },
                        move |chat, answer| {
                            let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                            if accepted {
                                show(&ctx_for_done, "library");
                            } else {
                                line_for_done.append(&card_note(
                                    "The service did not delete it. Nothing changed.",
                                ));
                            }
                            let _ = chat;
                        },
                    );
                });
            }
            window.present();
        });
    }
    more.connect_clicked(move |_| {
        popover.popup();
    });
    row
}

/// Repaint the library from held rows with the live tab and query:
/// the single path tab presses and Quick Open share.
fn repaint_library(ctx: &Ctx, page: &gtk4::Box, query: &str) {
    let (Some(list), Some(tabs), Some(entry)) = (
        named_list(page, "library-list"),
        find_widget(page.upcast_ref(), "library-tabs")
            .and_then(|widget| widget.downcast::<gtk4::Box>().ok()),
        find_widget(page.upcast_ref(), "library-search")
            .and_then(|widget| widget.downcast::<gtk4::Entry>().ok()),
    ) else {
        return;
    };
    let tab = active_tab(&tabs);
    let artifacts = ART_STORE.with(|store| store.borrow().clone());
    paint_library(ctx, &list, &tabs, &entry, &artifacts, &tab, query);
}

fn fill_library(ctx: &Ctx, page: &gtk4::Box) -> bool {
    let (Some(list), Some(tabs), Some(entry)) = (
        named_list(page, "library-list"),
        find_widget(page.upcast_ref(), "library-tabs")
            .and_then(|widget| widget.downcast::<gtk4::Box>().ok()),
        find_widget(page.upcast_ref(), "library-search")
            .and_then(|widget| widget.downcast::<gtk4::Entry>().ok()),
    ) else {
        return true;
    };
    // A fresh fill resets the view: stale queries against new rows
    // read as broken, never as empty.
    entry.set_text("");
    let Some(chat) = ctx.chat.upgrade() else {
        return true;
    };
    let ctx_owned = ctx.clone();
    fill_log("library", "visit", "");
    let ticket = match chat.page_begin(Op::Library) {
        Some(ticket) => ticket,
        None => {
            fill_log("library", "skip-no-ticket", "");
            return false;
        }
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        clear_list(&list);
        empty_note(&list, "Link your account first.");
        return true;
    };
    // Held by value so the answer closure can enable the web view's entry
    // point once a listing exists, without borrowing the page.
    let web_button = find_widget(page.upcast_ref(), "library-web")
        .and_then(|widget| widget.downcast::<gtk4::Button>().ok());
    clear_list(&list);
    let loading = gtk4::Label::new(Some("Loading the library…"));
    loading.add_css_class("dim-label");
    loading.set_xalign(0.0);
    list.append(&loading);
    dispatch(
        &chat,
        move || {
            let start = Instant::now();
            let listing = client.artifacts();
            let request_ms = start.elapsed().as_millis() as u64;
            listing.map(|listing| (listing, request_ms))
        },
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                fill_log("library", "stale-answer-dropped", "");
                return;
            }
            let Ok((listing, request_ms)) = answer else {
                // Errors read, never vanish: the page names the
                // failure so the reader isn't left guessing.
                if let Err(error) = answer {
                    fill_log("library", "failed", &format!("error=\"{error}\""));
                    listing_failure(&list, "Library", &error);
                }
                return;
            };
            ART_STORE.with(|store| {
                *store.borrow_mut() = listing.artifacts.clone();
            });
            ART_TOTAL.with(|total| total.set(listing.total_count));
            if let Some(button) = &web_button {
                button.set_sensitive(!listing.artifacts.is_empty() && crate::webui::available());
            }
            paint_library(
                &ctx_owned,
                &list,
                &tabs,
                &entry,
                &listing.artifacts,
                "All",
                "",
            );
            fill_log(
                "library",
                "painted",
                &format!("rows={} request_ms={request_ms}", listing.artifacts.len(),),
            );
        },
    );
    true
}

/// A thread row's day (`YYYY-MM-DD`) from its update stamp, when the
/// stamp carries one. Pure, so malformed stamps group as undated.
fn thread_day(updated_at: Option<&str>) -> Option<String> {
    let stamp = updated_at?;
    if stamp.len() >= 10 && stamp.as_bytes()[4] == b'-' && stamp.as_bytes()[7] == b'-' {
        Some(stamp.chars().take(10).collect())
    } else {
        None
    }
}

/// A thread row's clock time (`HH:MM`) from an ISO update stamp, when
/// the stamp carries one. The day itself lives in the group header.
fn thread_time(updated_at: Option<&str>) -> Option<String> {
    let stamp = updated_at?;
    let bytes = stamp.as_bytes();
    if stamp.len() >= 16
        && bytes[10] == b'T'
        && bytes[11..13].iter().all(u8::is_ascii_digit)
        && bytes[13] == b':'
        && bytes[14..16].iter().all(u8::is_ascii_digit)
    {
        Some(stamp.chars().skip(11).take(5).collect())
    } else {
        None
    }
}

/// A thread row's group header against the local today: Today,
/// Yesterday, the date itself for older days, Earlier when undated.
/// Reuses the panel's calendar buckets, so the two lists agree.
fn thread_group_label(updated_at: Option<&str>, today: &str) -> String {
    match thread_day(updated_at) {
        None => "Earlier".to_owned(),
        Some(day) => match super::app::activity_bucket(Some(&day), today) {
            "Today" => "Today".to_owned(),
            "Yesterday" => "Yesterday".to_owned(),
            _ => day,
        },
    }
}

/// A thread row's second line: the service's snippet plus the clock
/// time when the stamp carries one. The day lives in the header, so
/// it never repeats per row.
fn thread_meta(snippet: Option<&str>, updated_at: Option<&str>) -> String {
    let snippet = snippet.map(str::trim).filter(|text| !text.is_empty());
    match (snippet, thread_time(updated_at)) {
        (Some(text), Some(time)) => format!("{text} · {time}"),
        (Some(text), None) => text.to_owned(),
        (None, Some(time)) => time,
        (None, None) => String::new(),
    }
}

/// Split the listing into side chats and the main list. Only an
/// explicit non-primary flag files a side chat; order within each
/// half follows the listing (pinned first from the caller).
fn split_side_chats(
    threads: Vec<muse_account::Thread>,
) -> (Vec<muse_account::Thread>, Vec<muse_account::Thread>) {
    let (side, main): (Vec<_>, Vec<_>) = threads.into_iter().partition(|thread| !thread.is_primary);
    (side, main)
}

/// A note inside a list: "No threads yet." and its friends. The label
/// is not a row, but the list wraps it in one, and that wrapper would
/// take the keyboard and draw the focus ring around the empty box --
/// which reads as a broken panel rather than an answer. Keeping it out
/// of the Tab order is right: nothing opens from it.
fn empty_note(list: &gtk4::ListBox, text: &str) {
    let note = gtk4::Label::new(Some(text));
    list.append(&note);
    if let Some(row) = note.parent().and_downcast::<gtk4::ListBoxRow>() {
        row.set_focusable(false);
    }
}

/// One thread row: title, unread badge, snippet and time, plus the
/// row menu (rename, pin, archive — and Open full chat on side rows).
/// Row selection navigates to Chat; the ⋯ button guards its press so
/// the menu never navigates away.
fn append_thread_row(
    ctx: &Ctx,
    list: &gtk4::ListBox,
    thread: &muse_account::Thread,
    open_full: bool,
) {
    let row = gtk4::ListBoxRow::new();
    row.set_widget_name(&format!("thread:{}", thread.session_id));
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    let words = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let title_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let name = if thread.title.is_empty() {
        "(untitled)".to_owned()
    } else {
        thread.title.clone()
    };
    let name = if thread.pinned {
        format!("📌 {name}")
    } else {
        name
    };
    let label = gtk4::Label::new(Some(name.as_str()));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    title_row.append(&label);
    if thread.unread_count > 0 {
        let badge = gtk4::Label::new(Some(&format!("{}", thread.unread_count)));
        describe(&badge, &format!("{} unread", thread.unread_count));
        badge.add_css_class("muse-unread");
        title_row.append(&badge);
    }
    words.append(&title_row);
    let meta = thread_meta(thread.snippet.as_deref(), thread.updated_at.as_deref());
    if !meta.is_empty() {
        let sub = gtk4::Label::new(Some(&meta));
        sub.set_xalign(0.0);
        sub.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        sub.add_css_class("dim-label");
        words.append(&sub);
    }
    line.append(&words);
    let more = gtk4::Button::with_label("⋯");
    more.add_css_class("flat");
    more.add_css_class("circular");
    more.set_valign(gtk4::Align::Start);
    describe(&more, "Thread options");
    line.append(&more);
    row.set_child(Some(&line));
    let popover = gtk4::Popover::new();
    popover.set_parent(&more);
    let menu = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    menu.set_margin_start(8);
    menu.set_margin_end(8);
    menu.set_margin_top(8);
    menu.set_margin_bottom(8);
    if open_full {
        let open_ctx = ctx.clone();
        let open_popover = popover.clone();
        let open_id = thread.session_id.clone();
        let open = gtk4::Button::with_label("Open full chat");
        open.add_css_class("flat");
        menu.append(&open);
        open.connect_clicked(move |_| {
            open_popover.popdown();
            open_thread(&open_ctx, Some(open_id.clone()));
        });
    }
    let rename = gtk4::Button::with_label("Rename");
    rename.add_css_class("flat");
    menu.append(&rename);
    let pin_label = if thread.pinned { "Unpin" } else { "Pin" };
    let pin = gtk4::Button::with_label(pin_label);
    pin.add_css_class("flat");
    menu.append(&pin);
    let archive_label = if thread.archived {
        "Unarchive"
    } else {
        "Archive"
    };
    let archive = gtk4::Button::with_label(archive_label);
    archive.add_css_class("flat");
    menu.append(&archive);
    popover.set_child(Some(&menu));
    // A press inside the row activates it, and activating a thread
    // row navigates to Chat -- so the menu button unselects its row
    // on press (before the list sees it) and restores it when the
    // menu closes. The button stays a Tab stop, and Return or Space
    // opens the same menu, so the row's options are not pointer-only.
    {
        let popover_held = popover.clone();
        let row_weak = row.downgrade();
        let keys = gtk4::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gtk4::gdk::Key::Return
                && key != gtk4::gdk::Key::KP_Enter
                && key != gtk4::gdk::Key::space
            {
                return gtk4::glib::Propagation::Proceed;
            }
            if let Some(row) = row_weak.upgrade() {
                row.set_selectable(false);
            }
            popover_held.popup();
            gtk4::glib::Propagation::Stop
        });
        more.add_controller(keys);
    }
    let press = gtk4::GestureClick::new();
    {
        let row_weak = row.downgrade();
        let popover_held = popover.clone();
        press.connect_pressed(move |_, _, _, _| {
            if let Some(row) = row_weak.upgrade() {
                row.set_selectable(false);
            }
            popover_held.popup();
        });
    }
    more.add_controller(press);
    {
        let row_weak = row.downgrade();
        popover.connect_closed(move |_| {
            if let Some(row) = row_weak.upgrade() {
                row.set_selectable(true);
            }
        });
    }
    {
        let ctx = ctx.clone();
        let line_inner = line.clone();
        let popover_held = popover.clone();
        let session_id = thread.session_id.clone();
        let title = thread.title.clone();
        rename.connect_clicked(move |_| {
            popover_held.popdown();
            // Per-click locals: the dialog below (and its Save)
            // is built fresh on every press, so everything Save
            // moves must die with the press, never with the row.
            let session_id = session_id.clone();
            let title = title.clone();
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let window = gtk4::Window::new();
            window.set_modal(true);
            window.set_default_size(420, -1);
            window.set_title(Some("Rename thread"));
            if let Some(root) = line_inner
                .root()
                .and_then(|root| root.downcast::<gtk4::Window>().ok())
            {
                window.set_transient_for(Some(&root));
            }
            let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
            body.set_margin_start(24);
            body.set_margin_end(24);
            body.set_margin_top(20);
            body.set_margin_bottom(20);
            let entry = gtk4::Entry::new();
            entry.set_text(&title);
            entry.set_hexpand(true);
            body.append(&entry);
            let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let cancel = gtk4::Button::with_label("Cancel");
            cancel.add_css_class("flat");
            buttons.append(&cancel);
            let save = gtk4::Button::with_label("Save");
            save.add_css_class("muse-cta");
            buttons.append(&save);
            body.append(&buttons);
            window.set_child(Some(&body));
            {
                let window = window.clone();
                cancel.connect_clicked(move |_| {
                    window.close();
                });
            }
            {
                let window = window.clone();
                let line_for_done = line_inner.clone();
                let ctx_for_done = ctx.clone();
                save.connect_clicked(move |_| {
                    let name = entry.text().to_string();
                    if name.trim().is_empty() {
                        line_for_done.append(&card_note("A thread needs a non-blank title."));
                        return;
                    }
                    window.close();
                    let line_for_done = line_for_done.clone();
                    let ctx_for_done = ctx_for_done.clone();
                    super::app::dispatch(
                        &chat,
                        {
                            let client = client.clone();
                            let session_id = session_id.clone();
                            move || client.session_op(&session_id, "rename", Some(&name))
                        },
                        move |chat, answer| {
                            let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                            if accepted {
                                show(&ctx_for_done, "threads");
                            } else {
                                line_for_done.append(&card_note(
                                    "The service did not rename it. Nothing changed.",
                                ));
                            }
                            let _ = chat;
                        },
                    );
                });
            }
            window.present();
        });
    }
    for (button, kind) in [(pin, "pin"), (archive, "archive")] {
        let ctx = ctx.clone();
        let line_inner = line.clone();
        let popover_held = popover.clone();
        let session_id = thread.session_id.clone();
        let pinned = thread.pinned;
        let archived = thread.archived;
        button.connect_clicked(move |_| {
            popover_held.popdown();
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let flip = if kind == "pin" { !pinned } else { !archived };
            let verb = if kind == "pin" {
                if flip { "pin" } else { "unpin" }
            } else if flip {
                "archive"
            } else {
                "unarchive"
            };
            let line_for_done = line_inner.clone();
            let ctx_for_done = ctx.clone();
            super::app::dispatch(
                &chat,
                {
                    let client = client.clone();
                    let session_id = session_id.clone();
                    let verb = verb.to_owned();
                    move || client.session_op(&session_id, &verb, None)
                },
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                    if accepted {
                        show(&ctx_for_done, "threads");
                    } else {
                        line_for_done.append(&card_note(
                            "The service did not change it. Nothing changed.",
                        ));
                    }
                    let _ = chat;
                },
            );
        });
    }
    list.append(&row);
}

fn fill_threads(ctx: &Ctx, page: &gtk4::Box) -> bool {
    let Some(list) = named_list(page, "threads-list") else {
        return true;
    };
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let Some(chat) = ctx.chat.upgrade() else {
        empty_note(&list, "Link your account first.");
        return true;
    };
    let mut threads = chat.threads();
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.pinned));
    // The web slice opens on exactly these rows: one cache fill per paint,
    // built by the same helpers that word the native rows, so the two
    // renderers cannot drift on groups, clock times or pin order.
    threads_cache(held_threads_from(&threads, &super::app::local_today()));
    if let Some(button) = named_button(page, "threads-web") {
        button.set_sensitive(!threads.is_empty() && crate::webui::available());
    }
    if threads.is_empty() {
        empty_note(&list, "No threads yet.");
        return true;
    }
    let (side, main) = split_side_chats(threads);
    // Pinned threads keep their pride of place above the day groups;
    // the rest file under Today / Yesterday / date / Earlier headers.
    let today = super::app::local_today();
    let mut group: Option<String> = None;
    for thread in main {
        if !thread.pinned {
            let label = thread_group_label(thread.updated_at.as_deref(), &today);
            if group.as_deref() != Some(label.as_str()) {
                let head = gtk4::Label::new(Some(&label));
                head.set_halign(gtk4::Align::Start);
                head.add_css_class("muse-section");
                list.append(&head);
                group = Some(label);
            }
        }
        append_thread_row(ctx, &list, &thread, false);
    }
    if !side.is_empty() {
        let head = gtk4::Label::new(Some("Side chats"));
        head.set_halign(gtk4::Align::Start);
        head.add_css_class("muse-section");
        list.append(&head);
        for thread in &side {
            append_thread_row(ctx, &list, thread, true);
        }
    }
    true
}

/// Open a thread from the sidebar history (or a fresh one for a `None`
/// id): switch, then land on the chat page. A repeat press on the open
/// thread only lands; it never reloads what is already showing.
pub fn open_thread(ctx: &Ctx, id: Option<String>) {
    if let Some(chat) = ctx.chat.upgrade()
        && chat.current_thread() != id
    {
        chat.switch_thread(id);
    }
    show(ctx, "chat");
}

/// The sidebar's History section: the thread listing, pinned first,
/// with the open thread marked. Refilled on every page visit, so a
/// new or renamed thread appears without a restart. The profile row
/// names the linked identity beside it.
fn fill_history(ctx: &Ctx) {
    while let Some(child) = ctx.history.first_child() {
        ctx.history.remove(&child);
    }
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    let mut threads = chat.threads();
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.pinned));
    let current = chat.current_thread();
    for thread in &threads {
        let row = gtk4::ListBoxRow::new();
        // The drawer's History is a list of Tab stops: Tab reaches it
        // and the arrow keys walk the threads.
        row.set_focusable(true);
        row.set_widget_name(&format!("thread:{}", thread.session_id));
        let name = if thread.title.is_empty() {
            "(untitled)".to_owned()
        } else {
            thread.title.clone()
        };
        let name = if thread.pinned {
            format!("📌 {name}")
        } else {
            name
        };
        let text = if thread.unread_count > 0 {
            format!("{} ({} unread)", name, thread.unread_count)
        } else {
            name
        };
        let label = gtk4::Label::new(Some(&text));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        row.set_child(Some(&label));
        ctx.history.append(&row);
        // Marking the open thread is only a highlight: History opens a
        // thread on activation, so selecting a row here never navigates.
        if current.as_deref() == Some(thread.session_id.as_str()) {
            ctx.history.select_row(Some(&row));
        }
    }
    let name = chat.session_name().unwrap_or_else(|| "Sign in".to_owned());
    ctx.profile_name.set_text(&name);
}

fn fill_settings(ctx: &Ctx, page: &gtk4::Box) -> bool {
    let Some(info) = find_widget(page.upcast_ref(), "settings-info")
        .and_then(|widget| widget.downcast::<gtk4::Label>().ok())
    else {
        return true;
    };
    match super::session::load_saved_link() {
        Some(link) => info.set_text(&format!(
            "Linked.\nInterpreter: {}\nSession folder: {}",
            link.python.display(),
            link.config_dir.display()
        )),
        None => info.set_text("No saved link."),
    }
    paint_updates(ctx, page);
    // Check on visit when due: the feed read is cheap and the result
    // waits on the settings page instead of interrupting anything.
    if super::update::running_as_appimage()
        && super::update::check_due(super::update::now_epoch())
        && !ctx.updates.borrow().busy
        && ctx.updates.borrow().info.is_none()
    {
        do_check(ctx, page);
    }
    true
}

/// Paint the updates section from the shared panel state.
fn paint_updates(ctx: &Ctx, page: &gtk4::Box) {
    let find = |name: &str| find_widget(page.upcast_ref(), name);
    let state = ctx.updates.borrow();
    // The eligibility and the last answer travel to the web view as data, so
    // its page and this section cannot tell two different stories.
    settings_updates_cache(UpdatesView {
        appimage: super::update::running_as_appimage(),
        status: state.status.clone(),
        busy: state.busy,
        checked: state.info.is_some(),
        staged: state.staged.is_some(),
    });
    if let Some(widget) = find("updates-status")
        && let Ok(label) = widget.downcast::<gtk4::Label>()
    {
        let text = if state.status.is_empty() {
            if super::update::running_as_appimage() {
                "Updates come from the rolling AppImage release."
            } else {
                "This copy updates through its own channel (store or checkout), not here."
            }
        } else {
            &state.status
        };
        label.set_text(text);
    }
    if let Some(widget) = find("updates-check")
        && let Ok(button) = widget.downcast::<gtk4::Button>()
    {
        button.set_sensitive(!state.busy && super::update::running_as_appimage());
    }
    if let Some(widget) = find("updates-action")
        && let Ok(button) = widget.downcast::<gtk4::Button>()
    {
        if state.staged.is_some() {
            button.set_label("Restart to update");
            button.set_visible(true);
            button.set_sensitive(!state.busy);
        } else if state.info.is_some() {
            button.set_label("Download update");
            button.set_visible(true);
            button.set_sensitive(!state.busy);
        } else {
            button.set_visible(false);
        }
    }
}

/// Ask the feed whether a newer build exists. One worker, one answer;
/// the panel repaints whatever it says, including failures.
fn do_check(ctx: &Ctx, page: &gtk4::Box) {
    {
        let mut state = ctx.updates.borrow_mut();
        if state.busy {
            return;
        }
        state.busy = true;
        state.status = "Checking for updates…".to_owned();
    }
    paint_updates(ctx, page);
    let (done_tx, done_rx) =
        std::sync::mpsc::channel::<Result<super::update::ReleaseInfo, String>>();
    std::thread::spawn(move || {
        let _ = done_tx.send(super::update::fetch_latest());
    });
    let ctx = ctx.clone();
    let page_strong: gtk4::Box = page.clone();
    gtk4::glib::idle_add_local(move || {
        let answer = match done_rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                return gtk4::glib::ControlFlow::Continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let mut state = ctx.updates.borrow_mut();
                state.busy = false;
                state.status = "Update check stopped without an answer.".to_owned();
                paint_updates(&ctx, &page_strong);
                return gtk4::glib::ControlFlow::Break;
            }
        };
        {
            let mut state = ctx.updates.borrow_mut();
            state.busy = false;
            match answer {
                Ok(info) => {
                    super::update::note_check_done(super::update::now_epoch());
                    if super::update::update_available(super::update::local_sha(), &info) {
                        state.status = "A newer build is available.".to_owned();
                        state.info = Some(info);
                    } else {
                        state.status = "This build is up to date.".to_owned();
                        state.info = None;
                    }
                }
                Err(message) => {
                    state.status = message;
                    state.info = None;
                }
            }
        }
        paint_updates(&ctx, &page_strong);
        gtk4::glib::ControlFlow::Break
    });
}

/// The action button: download the staged build, or restart into it.
/// The restart is verified on a worker (the new copy must stay up
/// before this one exits); a failed restart reports in the panel with
/// the staged path instead of leaving the reader with nothing.
fn do_update_action(ctx: &Ctx, page: &gtk4::Box) {
    let (info, staged) = {
        let state = ctx.updates.borrow();
        (state.info.clone(), state.staged.clone())
    };
    if let Some(staged) = staged {
        {
            let mut state = ctx.updates.borrow_mut();
            if state.busy {
                return;
            }
            state.busy = true;
            state.status = "Restarting into the update…".to_owned();
        }
        paint_updates(ctx, page);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || {
            let _ = done_tx.send(
                super::update::relaunch_verified(&staged)
                    .map_err(|message| format!("{message} Staged file: {}", staged.display()))
                    .and_then(|()| {
                        // The new copy is verified alive: refresh the
                        // launched file so the launcher opens it next
                        // time. A refresh failure keeps this copy up
                        // and says so instead of exiting into a stale
                        // launcher.
                        super::update::refresh_launcher_copy(&staged).map(|_| ()).map_err(
                            |message| {
                                format!(
                                    "The update is running, but the launcher copy kept its old build: {message}"
                                )
                            },
                        )
                    }),
            );
        });
        let ctx = ctx.clone();
        let page_strong: gtk4::Box = page.clone();
        gtk4::glib::idle_add_local(move || {
            let answer = match done_rx.try_recv() {
                Ok(answer) => answer,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    return gtk4::glib::ControlFlow::Continue;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let mut state = ctx.updates.borrow_mut();
                    state.busy = false;
                    state.status = "Restart check stopped without an answer.".to_owned();
                    paint_updates(&ctx, &page_strong);
                    return gtk4::glib::ControlFlow::Break;
                }
            };
            match answer {
                // The new copy proved it stays up: this one goes.
                Ok(()) => std::process::exit(0),
                Err(message) => {
                    let mut state = ctx.updates.borrow_mut();
                    state.busy = false;
                    state.status = message;
                    paint_updates(&ctx, &page_strong);
                }
            }
            gtk4::glib::ControlFlow::Break
        });
        return;
    }
    let Some(info) = info else {
        return;
    };
    {
        let mut state = ctx.updates.borrow_mut();
        if state.busy {
            return;
        }
        state.busy = true;
        state.status = "Downloading the update…".to_owned();
    }
    paint_updates(ctx, page);
    let (done_tx, done_rx) = std::sync::mpsc::channel::<Result<std::path::PathBuf, String>>();
    std::thread::spawn(move || {
        let _ = done_tx.send(super::update::download_staged(&info));
    });
    let ctx = ctx.clone();
    let page_strong: gtk4::Box = page.clone();
    gtk4::glib::idle_add_local(move || {
        let answer = match done_rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                return gtk4::glib::ControlFlow::Continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let mut state = ctx.updates.borrow_mut();
                state.busy = false;
                state.status = "Update download stopped without an answer.".to_owned();
                paint_updates(&ctx, &page_strong);
                return gtk4::glib::ControlFlow::Break;
            }
        };
        {
            let mut state = ctx.updates.borrow_mut();
            state.busy = false;
            match answer {
                Ok(staged) => {
                    state.status = format!(
                        "Update downloaded to {}. Restart to switch to it.",
                        staged.display()
                    );
                    state.staged = Some(staged);
                }
                Err(message) => {
                    state.status = message;
                }
            }
        }
        paint_updates(&ctx, &page_strong);
        gtk4::glib::ControlFlow::Break
    });
}

/// One feed card: the title plus whatever framing the service gave
/// (kicker, kind, stamp), each optional.
pub fn feed_item_label(item: &ListingItem) -> String {
    let meta: Vec<&str> = [&item.kicker, &item.kind, &item.stamp]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .filter(|text| !text.is_empty())
        .collect();
    let title = match item.icon.as_deref() {
        Some(icon) if !icon.is_empty() => format!("{icon} {}", item.title),
        _ => item.title.clone(),
    };
    if meta.is_empty() {
        title
    } else {
        format!("{title}\n{}", meta.join(" \u{00b7} "))
    }
}

/// A feed timestamp in the reader's words: minutes and hours ago
/// while fresh, Yesterday across the day line, a short date after
/// that. Pure over an explicit now so the phrasing is unit-tested.
pub fn relative_time(stamp: Option<&str>, now: i64) -> String {
    let epoch = stamp.and_then(|stamp| {
        let (date, time) = stamp.split_once('T')?;
        let (year, rest) = date.split_once('-')?;
        let (month, day) = rest.split_once('-')?;
        let (hour, rest) = time.split_once(':')?;
        let minute = rest.get(..2)?;
        let days = super::app::days_from_civil(
            year.parse::<i32>().ok()?,
            month.parse::<u32>().ok()?,
            day.parse::<u32>().ok()?,
        ) as i64;
        Some(days * 86_400 + hour.parse::<i64>().ok()? * 3600 + minute.parse::<i64>().ok()? * 60)
    });
    relative_time_epoch(epoch, now)
}

/// The shared recency buckets over epoch seconds: minutes, hours,
/// Yesterday, then the web's day naming.
fn relative_time_epoch(epoch: Option<i64>, now: i64) -> String {
    let Some(epoch) = epoch else {
        return String::new();
    };
    let delta = (now - epoch).max(0);
    if delta < 3600 {
        let minutes = (delta / 60).max(1);
        return format!("{minutes}m ago");
    }
    if delta < 86_400 {
        return format!("{}h ago", delta / 3600);
    }
    if delta < 2 * 86_400 {
        return "Yesterday".to_owned();
    }
    // Past yesterday the web names the day; the month table is the
    // only locale-shaped piece, and it is fixed English like the
    // service's own stamps.
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = epoch.div_euclid(86_400);
    let mut year = 1970;
    let mut remaining = days;
    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let year_days = if leap { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let lengths = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while remaining >= lengths[month] as i64 {
        remaining -= lengths[month] as i64;
        month += 1;
    }
    format!("{} {}", MONTHS[month], remaining + 1)
}

/// Recency over millisecond epoch stamps (the library's
/// `updated_at_ms`): same buckets as the string stamps, day granularity
/// below.
pub fn relative_time_ms(ms: Option<i64>, now: i64) -> String {
    relative_time_epoch(ms.map(|ms| ms.div_euclid(1000)), now)
}

/// A feed body to Pango markup: escaped, markdown `[text](url)` links
/// and bare URLs linkified, `**bold**` kept. The renderer's markdown
/// pass is transcript-shaped; the feed needs links first, so this
/// stays its own small shaper.
pub fn feed_body_markup(body: &str) -> String {
    let mut escaped = String::with_capacity(body.len());
    for mark in body.chars() {
        match mark {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(mark),
        }
    }
    // Markdown links first: `[text](url)`. An image marker (`![alt]`)
    // stays literal -- bodies carry no inline artwork.
    let mut linked = String::with_capacity(escaped.len());
    let chars: Vec<char> = escaped.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '['
            && (index == 0 || chars[index - 1] != '!')
            && let Some(close) = chars[index..].iter().position(|&mark| mark == ']')
            && chars.get(index + close + 1) == Some(&'(')
            && let Some(end) = chars[index + close + 1..]
                .iter()
                .position(|&mark| mark == ')')
        {
            let text: String = chars[index + 1..index + close].iter().collect();
            let url: String = chars[index + close + 2..index + close + 1 + end]
                .iter()
                .collect();
            let url = url.trim();
            if (url.starts_with("https://") || url.starts_with("http://")) && url.len() <= 2048 {
                linked.push_str(&format!("<a href=\"{url}\">{text}</a>"));
            } else {
                linked.push_str(
                    &chars[index..index + close + 1 + end + 1]
                        .iter()
                        .collect::<String>(),
                );
            }
            index += close + 1 + end + 1;
            continue;
        }
        linked.push(chars[index]);
        index += 1;
    }
    // Bare URLs: a whitespace-delimited address becomes a link, with
    // trailing punctuation left outside it.
    let mut out = String::with_capacity(linked.len());
    for token in linked.split_inclusive(char::is_whitespace) {
        let word = token.trim_end_matches(char::is_whitespace);
        let trail = &token[word.len()..];
        // Skip text already inside a link (its href or its label).
        if word.starts_with("<a href=") || word.ends_with("</a>") {
            out.push_str(token);
            continue;
        }
        let bare = word
            .trim_matches(|mark: char| ".,;:!?)]}'\"".contains(mark))
            .trim_start_matches('(');
        if (bare.starts_with("https://") || bare.starts_with("http://")) && bare.len() <= 2048 {
            out.push_str(&format!("<a href=\"{bare}\">{bare}</a>"));
            out.push_str(&word[bare.len().min(word.len())..]);
        } else {
            out.push_str(word);
        }
        out.push_str(trail);
    }
    // Bold survives linkifying because `**` never appears inside an
    // address the service sends; keep the pass dumb and total.
    let mut bolded = String::with_capacity(out.len());
    let mut parts = out.split("**");
    bolded.push_str(parts.next().unwrap_or(""));
    let mut strong = false;
    for part in parts {
        strong = !strong;
        if strong {
            bolded.push_str("<b>");
        } else {
            bolded.push_str("</b>");
        }
        bolded.push_str(part);
    }
    bolded
}

/// The row's quiet meta line: the service's kicker, with the category
/// beside it when both ride along. Nothing renders when neither does.
pub fn feed_meta_line(item: &ListingItem) -> Option<String> {
    match (
        item.kicker.as_deref().filter(|text| !text.is_empty()),
        item.category.as_deref().filter(|text| !text.is_empty()),
    ) {
        (Some(kicker), Some(category)) => Some(format!("{kicker} · {category}")),
        (Some(kicker), None) => Some(kicker.to_owned()),
        (None, Some(category)) => Some(category.to_owned()),
        (None, None) => None,
    }
}

/// The generation state under the prompt card: a sentence while the
/// service builds a fresh edition, nothing the rest of the time.
pub fn feed_status_line(status: Option<&muse_account::FeedStatus>) -> Option<String> {
    status
        .filter(|state| state.refresh_in_progress)
        .map(|_| "Generating a fresh edition…".to_owned())
}

/// The hero picture for a landed file, shared by the first paint and
/// the behind-the-paint download pass so both hang the same frame.
fn hero_picture(path: &std::path::Path) -> gtk4::Picture {
    let picture = gtk4::Picture::for_filename(path);
    picture.set_can_shrink(true);
    picture.set_halign(gtk4::Align::Start);
    picture
}

/// One feed card: the web's shape — source icon, bold title, relative
/// time, linkified body, hero picture, and the unit's widget graphic.
/// Live graphics mount only where `widget_available` proved the
/// helpers load; anywhere else the service's extracted text carries
/// the card, and no press can construct a crashing frame. Pictures
/// never block or fail the card: no bytes, no picture.
/// Which second button a feed row carries: the service's action
/// label on actionable units, Discuss on everything else.
pub fn feed_row_cta(item: &ListingItem) -> Option<String> {
    item.action_label
        .as_deref()
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
}

/// A small dim note hung on a card: failures, outcomes, anything the
/// reader must see inline because the row has no notice line.
fn card_note(text: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.add_css_class("dim-label");
    label
}

/// The reason dialog behind a feed row's ⋯ menu: the service's own
/// `why_did_i_see_this`, read-only. Display only, never a write.
fn show_why_dialog(card: &gtk4::Box, title: &str, why: &str) {
    let window = gtk4::Window::new();
    window.set_modal(true);
    window.set_default_size(480, -1);
    window.set_title(Some(title));
    if let Some(root) = card
        .root()
        .and_then(|root| root.downcast::<gtk4::Window>().ok())
    {
        window.set_transient_for(Some(&root));
    }
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    let head = gtk4::Label::new(Some("Why this is in your feed"));
    head.set_xalign(0.0);
    head.add_css_class("title-4");
    body.append(&head);
    let words = gtk4::Label::new(None);
    words.set_markup(&feed_body_markup(why));
    words.set_wrap(true);
    words.set_xalign(0.0);
    body.append(&words);
    let close = gtk4::Button::with_label("Close");
    close.add_css_class("muse-cta");
    body.append(&close);
    window.set_child(Some(&body));
    {
        let window = window.clone();
        close.connect_clicked(move |_| {
            window.close();
        });
    }
    window.present();
}

fn feed_card(
    ctx: &Ctx,
    generation: u64,
    index: usize,
    item: &ListingItem,
    now: i64,
    hero: Option<&std::path::Path>,
    live: bool,
) -> gtk4::Widget {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    card.add_css_class("muse-feed-card");
    // The row's name carries its fill generation and index: the
    // behind-the-paint hero pass hangs downloads only on rows from
    // the fill that requested them, never on a newer visit reusing
    // the same indices.
    card.set_widget_name(&format!("feed-card:{generation}:{index}"));
    if let Some(meta) = feed_meta_line(item) {
        let kicker = gtk4::Label::new(Some(&meta));
        kicker.set_xalign(0.0);
        kicker.add_css_class("dim-label");
        card.append(&kicker);
    }
    let head = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    let icon = gtk4::Label::new(Some(item.icon.as_deref().unwrap_or("📰")));
    icon.add_css_class("muse-feed-icon");
    head.append(&icon);
    let title = gtk4::Label::new(None);
    title.set_markup(&format!(
        "<b>{}</b>",
        super::chat::render_markup(&item.title)
            .replace("<br/>", " ")
            .replace("<br>", " ")
    ));
    title.set_wrap(true);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    head.append(&title);
    let when = gtk4::Label::new(Some(&relative_time(item.stamp.as_deref(), now)));
    when.add_css_class("dim-label");
    head.append(&when);
    card.append(&head);
    if let Some(body) = item.body.as_deref()
        && !body.is_empty()
    {
        let words = gtk4::Label::new(None);
        words.set_markup(&feed_body_markup(body));
        words.set_wrap(true);
        words.set_xalign(0.0);
        card.append(&words);
    }
    if let Some(hero) = hero {
        card.append(&hero_picture(hero));
    }
    if let Some(html) = item.widget_html.as_deref()
        && !html.is_empty()
    {
        if live {
            card.append(&super::widget::widget_frame(
                html,
                item.widget_text.as_deref(),
            ));
        } else if let Some(text) = item.widget_text.as_deref()
            && !text.is_empty()
        {
            let graphic = gtk4::Label::new(Some(text));
            graphic.set_wrap(true);
            graphic.set_xalign(0.0);
            graphic.add_css_class("dim-label");
            card.append(&graphic);
        }
    }
    // The web's action row: Love always, the service's labeled action
    // on actionable units, Discuss on the rest, and the ⋯ menu.
    // Failures read inline on the card; the next visit proves the rest.
    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let love = gtk4::Button::with_label("Love");
    love.add_css_class("flat");
    actions.append(&love);
    let cta = feed_row_cta(item);
    let second = match cta.as_deref() {
        Some(label) => {
            let go = gtk4::Button::with_label(label);
            go.add_css_class("muse-cta");
            go
        }
        None => {
            let discuss = gtk4::Button::with_label("Discuss");
            discuss.add_css_class("flat");
            discuss
        }
    };
    actions.append(&second);
    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    actions.append(&spacer);
    let more = gtk4::Button::with_label("⋯");
    more.add_css_class("flat");
    more.add_css_class("circular");
    describe(&more, "Feed unit options");
    actions.append(&more);
    card.append(&actions);
    // One press, one write: Love records a heart reaction, the labeled
    // action executes the unit, Discuss stages a chat draft.
    let card_weak = card.downgrade();
    let unit_id = item.id.clone();
    {
        let ctx = ctx.clone();
        love.clone().connect_clicked(move |_| {
            let pressed = love.clone();
            let Some(id) = unit_id.clone() else {
                if let Some(card) = card_weak.upgrade() {
                    card.append(&card_note("This unit carries no service id."));
                }
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                if let Some(card) = card_weak.upgrade() {
                    card.append(&card_note("Link your account first."));
                }
                return;
            };
            pressed.set_sensitive(false);
            let pressed_inner = pressed.clone();
            let card_inner = card_weak.clone();
            super::app::dispatch(
                &chat,
                move || client.feed_react(&id, "❤️"),
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                    if accepted {
                        describe(&pressed_inner, "Loved");
                    } else {
                        pressed_inner.set_sensitive(true);
                        if let Some(card) = card_inner.upgrade() {
                            card.append(&card_note(
                                "The service did not record it. Nothing changed.",
                            ));
                        }
                    }
                    let _ = chat;
                },
            );
        });
    }
    {
        let ctx = ctx.clone();
        let card_weak = card.downgrade();
        let unit_id = item.id.clone();
        let title = item.title.clone();
        let label = cta.clone().unwrap_or_else(|| "Discuss".to_owned());
        second.clone().connect_clicked(move |_| {
            let pressed = second.clone();
            if cta.is_some() {
                let Some(id) = unit_id.clone() else {
                    if let Some(card) = card_weak.upgrade() {
                        card.append(&card_note("This unit carries no service id."));
                    }
                    return;
                };
                let Some(chat) = ctx.chat.upgrade() else {
                    return;
                };
                let Some(client) = chat.client_handle() else {
                    if let Some(card) = card_weak.upgrade() {
                        card.append(&card_note("Link your account first."));
                    }
                    return;
                };
                pressed.set_sensitive(false);
                pressed.set_label("Starting…");
                let card_inner = card_weak.clone();
                let label_inner = label.clone();
                let pressed_inner = pressed.clone();
                super::app::dispatch(
                    &chat,
                    move || client.idea_execute(&id),
                    move |chat, answer| {
                        let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                        if let Some(card) = card_inner.upgrade() {
                            card.append(&card_note(execute_notice(accepted)));
                        }
                        pressed_inner.set_label(&label_inner);
                        if !accepted {
                            pressed_inner.set_sensitive(true);
                        }
                        let _ = chat;
                    },
                );
            } else {
                let Some(chat) = ctx.chat.upgrade() else {
                    return;
                };
                show(&ctx, "chat");
                chat.propose_draft(&title);
            }
        });
    }
    // The row's ⋯ menu, exactly the web's two verbs: the reason first,
    // delete last. Why opens the service's own text; delete removes
    // the unit through the recorded path-only route, then re-reads
    // the feed so the row's absence is proven, not assumed.
    let popover = gtk4::Popover::new();
    popover.set_parent(&more);
    let menu = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    menu.set_margin_start(8);
    menu.set_margin_end(8);
    menu.set_margin_top(8);
    menu.set_margin_bottom(8);
    if let Some(why) = item.why.clone() {
        let why_button = gtk4::Button::with_label("Why I created this");
        why_button.add_css_class("flat");
        menu.append(&why_button);
        let popover_held = popover.clone();
        let card_weak = card.downgrade();
        let title = item.title.clone();
        why_button.connect_clicked(move |_| {
            popover_held.popdown();
            if let Some(card) = card_weak.upgrade() {
                show_why_dialog(&card, &title, &why);
            }
        });
    }
    let delete_button = gtk4::Button::with_label("Delete");
    delete_button.add_css_class("flat");
    delete_button.add_css_class("muse-danger");
    menu.append(&delete_button);
    {
        let ctx = ctx.clone();
        let card_weak = card.downgrade();
        let unit_id = item.id.clone();
        let popover_held = popover.clone();
        delete_button.connect_clicked(move |_| {
            popover_held.popdown();
            let Some(id) = unit_id.clone() else {
                if let Some(card) = card_weak.upgrade() {
                    card.append(&card_note("This unit carries no service id."));
                }
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                if let Some(card) = card_weak.upgrade() {
                    card.append(&card_note("Link your account first."));
                }
                return;
            };
            let card_for_done = card_weak.clone();
            let ctx_for_done = ctx.clone();
            super::app::dispatch(
                &chat,
                move || client.feed_unit_delete(&id),
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                    if accepted {
                        show(&ctx_for_done, "feed");
                    } else if let Some(card) = card_for_done.upgrade() {
                        card.append(&card_note(
                            "The service did not delete it. Nothing changed.",
                        ));
                    }
                    let _ = chat;
                },
            );
        });
    }
    popover.set_child(Some(&menu));
    more.connect_clicked(move |_| {
        popover.popup();
    });
    card.upcast()
}

/// Fill the feed page: the generating prompt card plus the units.
/// The last good feed, kept on disk: a slow service still reads
/// instantly on the next visit, then refreshes behind the paint.
#[derive(serde::Serialize, serde::Deserialize)]
struct FeedCache {
    listing: ServiceListing,
}

fn feed_cache_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".cache"))
        })?;
    Some(base.join("muse-feed.json"))
}

fn read_feed_cache() -> Option<ServiceListing> {
    let path = feed_cache_path()?;
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<FeedCache>(&bytes)
        .ok()
        .map(|cache| cache.listing)
}

fn write_feed_cache(listing: &ServiceListing) {
    let Some(path) = feed_cache_path() else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(&FeedCache {
        listing: listing.clone(),
    }) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, bytes);
}

/// Paint one listing's rows: prompt card plus cards with cache-only
/// heroes. Returns the rows still missing heroes so the caller can
/// chase them behind the paint. Shared by the cached first paint and
/// the fresh wire paint so both read identically.
fn paint_feed_now(
    ctx: &Ctx,
    list: &gtk4::ListBox,
    prompt_holder: Option<&gtk4::Label>,
    status_holder: Option<&gtk4::Button>,
    listing: &ServiceListing,
) -> (u64, Vec<(usize, String)>) {
    clear_list(list);
    if let Some(holder) = status_holder {
        match feed_status_line(listing.feed_status.as_ref()) {
            Some(line) => {
                holder.set_label(&line);
                holder.set_visible(true);
            }
            None => holder.set_visible(false),
        }
    }
    if let Some(holder) = prompt_holder {
        let text = listing
            .feed_prompt
            .as_ref()
            .and_then(|prompt| prompt.text.as_deref())
            .unwrap_or_default();
        holder.set_text(text);
        holder.set_visible(!text.is_empty());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs() as i64)
        .unwrap_or(0);
    // Each fill takes a generation so the download pass below hangs
    // bytes only on its own rows.
    let generation = FEED_GEN.with(|slot| {
        let next = slot.get().wrapping_add(1);
        slot.set(next);
        next
    });
    // The paint reads cache only: rows appear the moment the listing
    // lands, and the network never holds them.
    let heroes: Vec<Option<std::path::PathBuf>> = listing
        .items
        .iter()
        .map(|item| {
            item.image_url
                .as_deref()
                .and_then(super::images::fetch_hero_cached)
        })
        .collect();
    // Widget frames need working helpers: probe once per fill
    // (the result caches per process) so a broken bundle
    // paints text instead of aborting on a frame.
    let graphics = super::widget::widget_available();
    let mut live = 0;
    for (index, (item, hero)) in listing.items.iter().zip(heroes.iter()).enumerate() {
        // The first two graphics render live, and only where
        // the helper probe passed; the rest read from the
        // service's extracted text (see feed_card).
        let frame = item
            .widget_html
            .as_deref()
            .is_some_and(|html| !html.is_empty());
        if graphics && frame {
            live += 1;
        }
        list.append(&feed_card(
            ctx,
            generation,
            index,
            item,
            now,
            hero.as_deref(),
            graphics && frame && live <= 2,
        ));
    }
    if listing.items.is_empty() {
        let label = gtk4::Label::new(Some("The feed is empty."));
        label.set_xalign(0.0);
        list.append(&label);
    }
    // The network fills the first screenful behind the paint; the
    // generation names the rows this pass may touch.
    let missing: Vec<(usize, String)> = listing
        .items
        .iter()
        .enumerate()
        .filter(|(index, _)| heroes[*index].is_none())
        .filter_map(|(index, item)| item.image_url.clone().map(|url| (index, url)))
        .take(super::images::HERO_LANES)
        .collect();
    (generation, missing)
}

fn fill_feed(ctx: &Ctx, page: &gtk4::Box) -> bool {
    let Some(list) = named_list(page, "feed-list") else {
        return true;
    };
    let Some(chat) = ctx.chat.upgrade() else {
        return true;
    };
    let ticket = match chat.page_begin(Op::Feed) {
        Some(ticket) => ticket,
        None => {
            fill_log("feed", "skip-no-ticket", "");
            return false;
        }
    };
    // The prompt card lives above the list (built with the page);
    // resolve it before the worker round-trip, which cannot borrow it.
    let prompt_holder: Option<gtk4::Label> = find_widget(page.upcast_ref(), "feed-prompt")
        .and_then(|widget| widget.downcast::<gtk4::Label>().ok());
    let status: Option<gtk4::Button> = find_widget(page.upcast_ref(), "feed-status")
        .and_then(|widget| widget.downcast::<gtk4::Button>().ok());
    // Downgraded up front: the fill below outlives this call, and a
    // borrowed page cannot enter its static completion.
    let page_weak = page.downgrade();
    let ctx_owned = ctx.clone();
    // The last good feed paints instantly while the wire answers:
    // a slow service reads as its previous self, never as blank.
    let had_cache = if let Some(cached) = read_feed_cache() {
        let prompt = prompt_holder.as_ref();
        paint_feed_now(&ctx_owned, &list, prompt, status.as_ref(), &cached);
        cache_feed_for_web(&cached);
        if let Some(button) = named_button(page, "feed-web") {
            button.set_sensitive(!cached.items.is_empty() && crate::webui::available());
        }
        let refreshing = gtk4::Label::new(Some("Refreshing the feed…"));
        refreshing.add_css_class("dim-label");
        refreshing.set_xalign(0.0);
        list.append(&refreshing);
        true
    } else {
        let loading = gtk4::Label::new(Some("Loading the feed…"));
        loading.add_css_class("dim-label");
        loading.set_xalign(0.0);
        list.append(&loading);
        false
    };
    // A missing link reads, never blanks: cached rows stay with the
    // reason there is no fresher list.
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        if had_cache {
            let note = gtk4::Label::new(Some("Couldn't refresh: link your account first."));
            note.add_css_class("dim-label");
            note.set_xalign(0.0);
            list.append(&note);
        } else {
            listing_failure(&list, "Feed", &"Link your account first.");
        }
        return false;
    };
    dispatch(
        &chat,
        move || {
            let answer = client.feed();
            if let Ok(listing) = &answer {
                write_feed_cache(listing);
            }
            answer
        },
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let Ok(listing) = answer else {
                // Errors read, never vanish. With cached rows on the
                // page the failure appends instead of replacing: the
                // reader keeps yesterday's feed plus the reason there
                // is no fresher one.
                if let Err(error) = answer {
                    if had_cache {
                        let label = gtk4::Label::new(Some(&listing_failure_text("Feed", &error)));
                        label.set_wrap(true);
                        label.set_xalign(0.0);
                        list.append(&label);
                    } else {
                        listing_failure(&list, "Feed", &error);
                    }
                }
                return;
            };
            let prompt = prompt_holder.as_ref();
            let (generation, missing) =
                paint_feed_now(&ctx_owned, &list, prompt, status.as_ref(), &listing);
            cache_feed_for_web(&listing);
            if let Some(page) = page_weak.upgrade()
                && let Some(button) = named_button(&page, "feed-web")
            {
                button.set_sensitive(!listing.items.is_empty() && crate::webui::available());
            }
            // The network fills the first screenful behind the paint:
            // no page ticket is held while downloads run, so pressing
            // elsewhere still fills at once.
            if !missing.is_empty() {
                super::app::dispatch(
                    chat,
                    move || super::images::fetch_heroes(missing),
                    move |chat, landed| {
                        let Some(page) = page_weak.upgrade() else {
                            return;
                        };
                        for (index, path) in landed {
                            let Some(path) = path else { continue };
                            let name = format!("feed-card:{generation}:{index}");
                            if let Some(card) = find_widget(page.upcast_ref(), &name)
                                .and_then(|widget| widget.downcast::<gtk4::Box>().ok())
                            {
                                card.append(&hero_picture(&path));
                            }
                        }
                        let _ = chat;
                    },
                );
            }
        },
    );
    true
}
/// A failed listing read says so on the page: a blank list with no
/// sentence is indistinguishable from an empty one, and the reader
/// cannot tell a broken fetch from a quiet service.
pub fn listing_failure_text(context: &str, error: &impl std::fmt::Display) -> String {
    format!("{context} didn't load: {error}")
}

fn listing_failure(list: &gtk4::ListBox, context: &str, error: &impl std::fmt::Display) {
    clear_list(list);
    let label = gtk4::Label::new(Some(&listing_failure_text(context, error)));
    label.set_wrap(true);
    label.set_xalign(0.0);
    list.append(&label);
}

/// One service listing behind one list widget: ideas, goals, and any
/// future read-only list. The fetcher runs on a worker; the painter
/// runs on the main thread and only applies through the shared page
/// slot, so a late answer from a left page owns nothing.
/// One row of a listing: the hero picture when the service gave one
/// (fetched and cached off-thread), always beside the text card.
/// Pictures never block or fail the row: no bytes, text-only.
/// A live widget frame leads whenever the service sent markup -- that
/// is the Mac's own order: frame first, hero and text as fallback.
pub fn listing_row(
    item: &muse_account::ListingItem,
    hero: Option<&std::path::Path>,
) -> gtk4::Widget {
    if let Some(html) = item.widget_html.as_deref()
        && !html.is_empty()
    {
        return super::widget::widget_frame(html, item.widget_text.as_deref());
    }
    let label = gtk4::Label::new(Some(&feed_item_label(item)));
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    let Some(hero) = hero else {
        return label.upcast();
    };
    let picture = gtk4::Picture::for_filename(hero);
    picture.set_can_shrink(true);
    picture.set_size_request(240, -1);
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.append(&picture);
    row.append(&label);
    row.upcast()
}

/// The category filters a goals fill offers: the distinct non-empty
/// categories in visit order. Buttons exist only for data in hand,
/// never for imagined kinds.
pub fn goal_categories(items: &[ListingItem]) -> Vec<String> {
    let mut kinds = Vec::new();
    for item in items {
        if let Some(category) = item.category.as_deref().filter(|text| !text.is_empty())
            && !kinds.contains(&category.to_owned())
        {
            kinds.push(category.to_owned());
        }
    }
    kinds
}

/// Whether a roster row survives the active filter. Only goals filter,
/// and only by category; everything else always reads.
pub fn roster_visible(item: &ListingItem, page: &str, filter: Option<&str>) -> bool {
    match (page, filter) {
        ("goals", Some(name)) => item.category.as_deref() == Some(name),
        _ => true,
    }
}

/// Open the goal creation dialog: title, summary, description and
/// emoji, under the row's goal for a subgoal or top-level for
/// None. One confirmed press is one goal; the re-read listing
/// proves it exists.
fn open_goal_create_dialog(ctx: &Ctx, anchor: &gtk4::Box, parent: Option<(String, String)>) {
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    let Some(client) = chat.client_handle() else {
        anchor.append(&card_note("Link your account first."));
        return;
    };
    let window = gtk4::Window::new();
    window.set_modal(true);
    window.set_default_size(480, -1);
    window.set_title(Some(&match &parent {
        Some((_, title)) => format!("Subgoal of “{title}”"),
        None => "Create a goal".to_owned(),
    }));
    if let Some(root) = anchor
        .root()
        .and_then(|root| root.downcast::<gtk4::Window>().ok())
    {
        window.set_transient_for(Some(&root));
    }
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    let name = gtk4::Entry::new();
    name.set_placeholder_text(Some("Goal title"));
    name.set_hexpand(true);
    body.append(&name);
    let summary = gtk4::Entry::new();
    summary.set_placeholder_text(Some("One-line summary"));
    summary.set_hexpand(true);
    body.append(&summary);
    let description = gtk4::Entry::new();
    description.set_placeholder_text(Some("What done looks like"));
    description.set_hexpand(true);
    body.append(&description);
    let emoji = gtk4::Entry::new();
    emoji.set_text("🎯");
    emoji.set_hexpand(true);
    body.append(&emoji);
    let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let cancel = gtk4::Button::with_label("Cancel");
    cancel.add_css_class("flat");
    buttons.append(&cancel);
    let save = gtk4::Button::with_label(if parent.is_some() {
        "Add subgoal"
    } else {
        "Create goal"
    });
    save.add_css_class("muse-cta");
    buttons.append(&save);
    body.append(&buttons);
    window.set_child(Some(&body));
    {
        let window = window.clone();
        cancel.connect_clicked(move |_| {
            window.close();
        });
    }
    {
        let window = window.clone();
        let anchor_for_done = anchor.clone();
        let ctx_for_done = ctx.clone();
        save.connect_clicked(move |_| {
            let title = name.text().to_string();
            let summary = summary.text().to_string();
            let description = description.text().to_string();
            let emoji = emoji.text().to_string();
            if title.trim().is_empty()
                || summary.trim().is_empty()
                || description.trim().is_empty()
                || emoji.trim().is_empty()
            {
                anchor_for_done.append(&card_note(
                    "Every field needs words before there is anything to add.",
                ));
                return;
            }
            window.close();
            let anchor_for_done = anchor_for_done.clone();
            let ctx_for_done = ctx_for_done.clone();
            super::app::dispatch(
                &chat,
                {
                    let client = client.clone();
                    let parent = parent.clone();
                    move || match &parent {
                        Some((parent_goal_id, _)) => client
                            .goal_add_subgoal(
                                parent_goal_id,
                                &title,
                                &summary,
                                &description,
                                &emoji,
                            )
                            .map(|answer| answer.accepted),
                        None => client
                            .goal_create(&title, &summary, &description, &emoji)
                            .map(|answer| answer.accepted),
                    }
                },
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| *ok).unwrap_or(false);
                    if accepted {
                        show(&ctx_for_done, "goals");
                    } else {
                        anchor_for_done
                            .append(&card_note("The service did not add it. Nothing changed."));
                    }
                    let _ = chat;
                },
            );
        });
    }
    window.present();
}

/// One goal row: a completion checkbox reflecting the service's own
/// status, title plus clamped description, and the ⋯ menu. A row
/// press opens the detail dialog; the id travels from the listing,
/// so a press with nothing to read never dispatches.
fn goal_row(ctx: &Ctx, item: &ListingItem, index: usize) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.set_widget_name(&format!("goal:{index}"));
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    let done = gtk4::CheckButton::new();
    done.set_sensitive(false);
    done.set_valign(gtk4::Align::Start);
    done.set_tooltip_text(Some(&format!("Mark {} complete", item.title)));
    line.append(&done);
    let words = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let title = gtk4::Label::new(None);
    title.set_markup(&format!(
        "<b>{}</b>",
        super::chat::render_markup(&item.title)
            .replace("<br/>", " ")
            .replace("<br>", " ")
    ));
    title.set_wrap(true);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    words.append(&title);
    let snippet = idea_snippet(item.body.as_deref());
    if !snippet.is_empty() {
        let sub = gtk4::Label::new(Some(&snippet));
        sub.set_wrap(true);
        sub.set_xalign(0.0);
        sub.set_lines(3);
        sub.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        sub.add_css_class("dim-label");
        words.append(&sub);
    }
    line.append(&words);
    let more = gtk4::Button::with_label("⋯");
    more.add_css_class("flat");
    more.add_css_class("circular");
    more.set_valign(gtk4::Align::Start);
    describe(&more, "Goal options");
    line.append(&more);
    row.set_child(Some(&line));
    // The web's five verbs: discuss proposes a draft, rename and
    // complete/reopen dispatch, delete dispatches, and adding a
    // subgoal creates a child through the live-probed route.
    let popover = gtk4::Popover::new();
    popover.set_parent(&more);
    let menu = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    menu.set_margin_start(8);
    menu.set_margin_end(8);
    menu.set_margin_top(8);
    menu.set_margin_bottom(8);
    let discuss = gtk4::Button::with_label("Discuss");
    discuss.add_css_class("flat");
    menu.append(&discuss);
    let rename = gtk4::Button::with_label("Rename");
    rename.add_css_class("flat");
    menu.append(&rename);
    let subgoal = gtk4::Button::with_label("Add subgoal");
    subgoal.add_css_class("flat");
    menu.append(&subgoal);
    let complete = gtk4::Button::with_label("Complete");
    complete.add_css_class("flat");
    menu.append(&complete);
    let delete = gtk4::Button::with_label("Delete");
    delete.add_css_class("flat");
    delete.add_css_class("muse-danger");
    menu.append(&delete);
    popover.set_child(Some(&menu));
    {
        let ctx = ctx.clone();
        let title = item.title.clone();
        let popover_held = popover.clone();
        discuss.connect_clicked(move |_| {
            popover_held.popdown();
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            show(&ctx, "chat");
            chat.propose_draft(&title);
        });
    }
    // The web's five verbs. Discuss proposes a draft; rename and
    // complete/reopen dispatch the live-probed PATCHes and re-read;
    // only "Add subgoal" has no backend, so only it reads plainly.
    let completed = item.kind.as_deref() == Some("completed");
    done.set_active(completed);
    done.set_tooltip_text(Some(if completed {
        "Completed — reopen it from the options."
    } else {
        "Not completed."
    }));
    complete.set_label(if completed { "Reopen" } else { "Complete" });
    {
        let ctx = ctx.clone();
        let line_inner = line.clone();
        let popover_held = popover.clone();
        let goal_id = item.id.clone();
        let title = item.title.clone();
        rename.connect_clicked(move |_| {
            popover_held.popdown();
            let Some(goal_id) = goal_id.clone() else {
                line_inner.append(&card_note("This row carries no service id."));
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let window = gtk4::Window::new();
            window.set_modal(true);
            window.set_default_size(420, -1);
            window.set_title(Some("Rename goal"));
            if let Some(root) = line_inner
                .root()
                .and_then(|root| root.downcast::<gtk4::Window>().ok())
            {
                window.set_transient_for(Some(&root));
            }
            let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
            body.set_margin_start(24);
            body.set_margin_end(24);
            body.set_margin_top(20);
            body.set_margin_bottom(20);
            let entry = gtk4::Entry::new();
            entry.set_text(&title);
            entry.set_hexpand(true);
            body.append(&entry);
            let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let cancel = gtk4::Button::with_label("Cancel");
            cancel.add_css_class("flat");
            buttons.append(&cancel);
            let save = gtk4::Button::with_label("Save");
            save.add_css_class("muse-cta");
            buttons.append(&save);
            body.append(&buttons);
            window.set_child(Some(&body));
            {
                let window = window.clone();
                cancel.connect_clicked(move |_| {
                    window.close();
                });
            }
            {
                let window = window.clone();
                let line_for_done = line_inner.clone();
                let ctx_for_done = ctx.clone();
                save.connect_clicked(move |_| {
                    let title = entry.text().to_string();
                    if title.trim().is_empty() {
                        line_for_done.append(&card_note("A goal needs a non-blank title."));
                        return;
                    }
                    window.close();
                    let line_for_done = line_for_done.clone();
                    let ctx_for_done = ctx_for_done.clone();
                    super::app::dispatch(
                        &chat,
                        {
                            let client = client.clone();
                            let goal_id = goal_id.clone();
                            move || client.goal_rename(&goal_id, &title)
                        },
                        move |chat, answer| {
                            let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                            if accepted {
                                show(&ctx_for_done, "goals");
                            } else {
                                line_for_done.append(&card_note(
                                    "The service did not rename it. Nothing changed.",
                                ));
                            }
                            let _ = chat;
                        },
                    );
                });
            }
            window.present();
        });
    }
    {
        let ctx = ctx.clone();
        let line_inner = line.clone();
        let popover_held = popover.clone();
        let goal_id = item.id.clone();
        complete.connect_clicked(move |_| {
            popover_held.popdown();
            let Some(goal_id) = goal_id.clone() else {
                line_inner.append(&card_note("This row carries no service id."));
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let line_for_done = line_inner.clone();
            let ctx_for_done = ctx.clone();
            super::app::dispatch(
                &chat,
                {
                    let client = client.clone();
                    let goal_id = goal_id.clone();
                    move || client.goal_set_complete(&goal_id, !completed)
                },
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                    if accepted {
                        show(&ctx_for_done, "goals");
                    } else {
                        line_for_done.append(&card_note(
                            "The service did not change it. Nothing changed.",
                        ));
                    }
                    let _ = chat;
                },
            );
        });
    }
    // "Add subgoal" opens the web's own creation: title, summary,
    // description and emoji under the row's goal. One confirmed
    // press is one child goal; the re-read listing proves it.
    {
        let ctx = ctx.clone();
        let line_inner = line.clone();
        let popover_held = popover.clone();
        let goal_id = item.id.clone();
        let title = item.title.clone();
        subgoal.connect_clicked(move |_| {
            popover_held.popdown();
            let Some(parent_goal_id) = goal_id.clone() else {
                line_inner.append(&card_note("This row carries no service id."));
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let window = gtk4::Window::new();
            window.set_modal(true);
            window.set_default_size(480, -1);
            window.set_title(Some(&format!("Subgoal of “{title}”")));
            if let Some(root) = line_inner
                .root()
                .and_then(|root| root.downcast::<gtk4::Window>().ok())
            {
                window.set_transient_for(Some(&root));
            }
            let body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
            body.set_margin_start(24);
            body.set_margin_end(24);
            body.set_margin_top(20);
            body.set_margin_bottom(20);
            let name = gtk4::Entry::new();
            name.set_placeholder_text(Some("Subgoal title"));
            name.set_hexpand(true);
            body.append(&name);
            let summary = gtk4::Entry::new();
            summary.set_placeholder_text(Some("One-line summary"));
            summary.set_hexpand(true);
            body.append(&summary);
            let description = gtk4::Entry::new();
            description.set_placeholder_text(Some("What done looks like"));
            description.set_hexpand(true);
            body.append(&description);
            let emoji = gtk4::Entry::new();
            emoji.set_text("🎯");
            emoji.set_hexpand(true);
            body.append(&emoji);
            let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let cancel = gtk4::Button::with_label("Cancel");
            cancel.add_css_class("flat");
            buttons.append(&cancel);
            let save = gtk4::Button::with_label("Add subgoal");
            save.add_css_class("muse-cta");
            buttons.append(&save);
            body.append(&buttons);
            window.set_child(Some(&body));
            {
                let window = window.clone();
                cancel.connect_clicked(move |_| {
                    window.close();
                });
            }
            {
                let window = window.clone();
                let line_for_done = line_inner.clone();
                let ctx_for_done = ctx.clone();
                save.connect_clicked(move |_| {
                    let title = name.text().to_string();
                    let summary = summary.text().to_string();
                    let description = description.text().to_string();
                    let emoji = emoji.text().to_string();
                    if title.trim().is_empty()
                        || summary.trim().is_empty()
                        || description.trim().is_empty()
                        || emoji.trim().is_empty()
                    {
                        line_for_done.append(&card_note(
                            "Every field needs words before there is anything to add.",
                        ));
                        return;
                    }
                    window.close();
                    let line_for_done = line_for_done.clone();
                    let ctx_for_done = ctx_for_done.clone();
                    super::app::dispatch(
                        &chat,
                        {
                            let client = client.clone();
                            let parent_goal_id = parent_goal_id.clone();
                            move || {
                                client.goal_add_subgoal(
                                    &parent_goal_id,
                                    &title,
                                    &summary,
                                    &description,
                                    &emoji,
                                )
                            }
                        },
                        move |chat, answer| {
                            let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                            if accepted {
                                show(&ctx_for_done, "goals");
                            } else {
                                line_for_done.append(&card_note(
                                    "The service did not add it. Nothing changed.",
                                ));
                            }
                            let _ = chat;
                        },
                    );
                });
            }
            window.present();
        });
    }
    {
        let ctx = ctx.clone();
        let unit_id = item.id.clone();
        let popover_held = popover.clone();
        let line_inner = line.clone();
        delete.connect_clicked(move |_| {
            popover_held.popdown();
            let Some(id) = unit_id.clone() else {
                line_inner.append(&card_note("This unit carries no service id."));
                return;
            };
            let Some(chat) = ctx.chat.upgrade() else {
                return;
            };
            let Some(client) = chat.client_handle() else {
                line_inner.append(&card_note("Link your account first."));
                return;
            };
            let line_for_done = line_inner.clone();
            let ctx_inner = ctx.clone();
            super::app::dispatch(
                &chat,
                move || client.goal_delete(&id),
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                    if accepted {
                        show(&ctx_inner, "goals");
                    } else {
                        line_for_done.append(&card_note(
                            "The service did not delete it. Nothing changed.",
                        ));
                    }
                    let _ = chat;
                },
            );
        });
    }
    more.connect_clicked(move |_| {
        popover.popup();
    });
    row
}

/// Paint one roster fill: the goals filter bar (only for the categories
/// in hand), group headers, and rows. Goal rows carry the web's ⋯ menu.
/// The filter is re-entrant: pressing one repaints from the same slices.
fn paint_roster(
    ctx: &Ctx,
    list: &gtk4::ListBox,
    page: &str,
    items: &[ListingItem],
    heroes: &[Option<std::path::PathBuf>],
    filter: Option<&str>,
) {
    clear_list(list);
    if page == "goals" {
        let mut kinds = vec!["All".to_owned()];
        kinds.extend(goal_categories(items));
        if kinds.len() > 2 {
            let bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let row = gtk4::ListBoxRow::new();
            row.set_selectable(false);
            for name in kinds {
                let button = gtk4::Button::with_label(&name);
                button.add_css_class("flat");
                if filter.unwrap_or("All") == name {
                    button.add_css_class("muse-filter-active");
                }
                let ctx = ctx.clone();
                let list = list.clone();
                let items = items.to_vec();
                let heroes = heroes.to_vec();
                let page = page.to_owned();
                let choice = name.clone();
                button.connect_clicked(move |_| {
                    let pick = if choice == "All" {
                        None
                    } else {
                        Some(choice.as_str())
                    };
                    paint_roster(&ctx, &list, &page, &items, &heroes, pick);
                });
                bar.append(&button);
            }
            row.set_child(Some(&bar));
            list.append(&row);
        }
    }
    let mut section: Option<&str> = None;
    for (index, item) in items.iter().enumerate() {
        if !roster_visible(item, page, filter) {
            continue;
        }
        let group = idea_group(item);
        if group != section {
            section = group;
            if let Some(name) = group {
                let head = gtk4::Label::new(Some(name));
                head.set_halign(gtk4::Align::Start);
                head.add_css_class("muse-section");
                list.append(&head);
            }
        }
        if page == "goals" {
            list.append(&goal_row(ctx, item, index));
        } else {
            list.append(&idea_row(item, index));
        }
    }
    if items.iter().all(|item| !roster_visible(item, page, filter)) {
        empty_note(list, "Nothing here yet.");
    } else if let Some(row) = list.row_at_index(0) {
        list.select_row(Some(&row));
    }
}

/// App-side page-fill diagnostics, beside the bridge's own
/// op-timing.log in the account dir: one line per visit, skip,
/// paint and failure, so a blank page is explainable after the
/// fact without guessing. Counts, latencies and error codes only —
/// titles, bodies, queries and URLs never land here. Best-effort:
/// logging must never fail a fill.
fn fill_log(page: &str, event: &str, detail: &str) {
    let Some(dir) = super::session::account_dir() else {
        return;
    };
    fill_log_to(&dir, page, event, detail);
}

/// The log writer with an explicit directory, so tests never touch
/// the process environment.
fn fill_log_to(dir: &std::path::Path, page: &str, event: &str, detail: &str) {
    use std::io::Write;
    const PATH: &str = "page-fill.log";
    const CAP_BYTES: u64 = 200 * 1024;
    const KEEP_LINES: usize = 400;
    let path = dir.join(PATH);
    if std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) > CAP_BYTES
        && let Ok(text) = std::fs::read_to_string(&path)
    {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > KEEP_LINES {
            let kept = lines[lines.len() - KEEP_LINES..].join("\n") + "\n";
            let _ = std::fs::write(&path, kept);
        }
    }
    let epoch_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_millis())
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let pid = std::process::id();
        let _ = writeln!(
            file,
            "{epoch_ms} pid={pid} page={page} event={event} {detail}"
        );
    }
}

/// The cached file for the first screenful of roster heroes: disk
/// only, never the network. The paint below reads this, so rows
/// appear the moment the listing lands; missing heroes warm up
/// behind the paint instead of holding it.
fn roster_cached_heroes(items: &[ListingItem]) -> Vec<Option<std::path::PathBuf>> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            if index < super::images::HERO_LANES {
                item.image_url
                    .as_deref()
                    .and_then(super::images::fetch_hero_cached)
            } else {
                None
            }
        })
        .collect()
}

/// Warm a painted roster's dialog store behind the paint: parallel
/// downloads, no page ticket held, so pressing elsewhere still
/// fills at once. The store update lands the artwork for dialogs
/// opened after it; the rows themselves never wait.
fn warm_dialog_heroes(
    chat: &Rc<Chat>,
    store_page: &str,
    items: &[ListingItem],
    heroes: &[Option<std::path::PathBuf>],
) {
    let missing: Vec<(usize, String)> = items
        .iter()
        .enumerate()
        .filter(|(index, _)| *index < super::images::HERO_LANES && heroes[*index].is_none())
        .filter_map(|(index, item)| item.image_url.clone().map(|url| (index, url)))
        .collect();
    if missing.is_empty() {
        return;
    }
    let store_page = store_page.to_owned();
    let log_page = store_page.clone();
    let expect = missing.len();
    super::app::dispatch(
        chat,
        move || {
            let start = Instant::now();
            let landed = super::images::fetch_heroes(missing);
            let warm_ms = start.elapsed().as_millis() as u64;
            (landed, warm_ms)
        },
        move |_chat, (landed, warm_ms)| {
            let warmed = landed.iter().filter(|(_, path)| path.is_some()).count();
            fill_log(
                &log_page,
                "heroes-warmed",
                &format!("expect={expect} warmed={warmed} warm_ms={warm_ms}"),
            );
            IDEA_STORE.with(|store| {
                if let Some(held) = store.borrow_mut().get_mut(&store_page) {
                    for (index, path) in landed {
                        if let Some(slot) = held.heroes.get_mut(index) {
                            *slot = path;
                        }
                    }
                }
            });
        },
    );
}

fn fill_service_listing<F>(
    ctx: &Ctx,
    page: &gtk4::Box,
    list: &gtk4::ListBox,
    op: Op,
    fetch: F,
) -> bool
where
    F: FnOnce(
            &muse_account::AccountClient,
        ) -> Result<muse_account::ServiceListing, muse_account::AccountError>
        + Send
        + 'static,
{
    let Some(chat) = ctx.chat.upgrade() else {
        return true;
    };
    let ctx_owned = ctx.clone();
    // Held by value for the answer closure: the web view's entry point waits
    // on a read, so the page has to outlive the dispatch.
    let page_owned = page.clone();
    let page_name = list.widget_name().to_string();
    let page_name = page_name
        .strip_suffix("-list")
        .unwrap_or(&page_name)
        .to_owned();
    fill_log(&page_name, "visit", "");
    let ticket = match chat.page_begin(op) {
        Some(ticket) => ticket,
        None => {
            fill_log(&page_name, "skip-no-ticket", "");
            return false;
        }
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        fill_log(&page_name, "no-client", "");
        return false;
    };
    let list = list.clone();
    dispatch(
        &chat,
        move || {
            let start = Instant::now();
            let listing = fetch(&client)?;
            // The paint reads cache only: rows appear the moment the
            // listing lands, and the network never holds them.
            let heroes = roster_cached_heroes(&listing.items);
            let request_ms = start.elapsed().as_millis() as u64;
            Ok::<_, muse_account::AccountError>((listing, heroes, request_ms))
        },
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                fill_log(&page_name, "stale-answer-dropped", "");
                return;
            }
            // A failed listing read says so on the page: a blank list
            // with no sentence is indistinguishable from an empty one.
            let (listing, heroes, request_ms) = match answer {
                Ok(tuple) => tuple,
                Err(error) => {
                    fill_log(&page_name, "failed", &format!("error=\"{error}\""));
                    listing_failure(&list, title(&page_name), &error);
                    // The web view reports the same failure beside the rows
                    // it holds, never as an empty page.
                    if page_name == "ideas" {
                        IDEAS_STORE.with(|store| {
                            store.borrow_mut().error = Some(error.to_string());
                        });
                    }
                    return;
                }
            };
            // The dialog reads its unit from here: hold the listing
            // and heroes beside the rows on every fill.
            IDEA_STORE.with(|store| {
                store.borrow_mut().insert(
                    page_name.clone(),
                    IdeaStore {
                        items: listing.items.clone(),
                        heroes: heroes.clone(),
                    },
                );
            });
            paint_roster(&ctx_owned, &list, &page_name, &listing.items, &heroes, None);
            // Ideas keeps the roster the web view opens on, with `loaded`
            // beside it: an account with no ideas is a real answer that an
            // empty list alone cannot give.
            if page_name == "ideas" {
                IDEAS_STORE.with(|store| {
                    let mut held = store.borrow_mut();
                    held.ideas = listing.items.clone();
                    held.total = listing.total;
                    held.loaded = true;
                    held.error = None;
                });
            }
            // The web view's entry point stays off until a listing exists.
            if let Some(button) = find_widget(page_owned.upcast_ref(), &format!("{page_name}-web"))
                .and_then(|widget| widget.downcast::<gtk4::Button>().ok())
            {
                button.set_sensitive(!listing.items.is_empty() && crate::webui::available());
            }
            let cached = heroes.iter().filter(|hero| hero.is_some()).count();
            fill_log(
                &page_name,
                "painted",
                &format!(
                    "rows={} request_ms={request_ms} heroes_cached={cached}",
                    listing.items.len(),
                ),
            );
            warm_dialog_heroes(chat, &page_name, &listing.items, &heroes);
        },
    );
    true
}

fn fill_ideas(ctx: &Ctx, page: &gtk4::Box) -> bool {
    let Some(list) = named_list(page, "ideas-list") else {
        return true;
    };
    fill_service_listing(ctx, page, &list, Op::Ideas, |client| client.ideas())
}

/// Fill the Goals page: the roster plus the Tracking section under
/// one page ticket. Two separate fills would race the single-flight
/// slot — the second begin lands while the first read is still in
/// flight, so its section never dispatches — so one worker reads
/// both and each section paints (or reports) on its own.
fn fill_goals(ctx: &Ctx, page: &gtk4::Box) -> bool {
    let Some(list) = named_list(page, "goals-list") else {
        return true;
    };
    let Some(tracking) = named_list(page, "goals-tracking-list") else {
        return fill_service_listing(ctx, page, &list, Op::Goals, |client| client.goals());
    };
    clear_list(&tracking);
    if let Some(old) = find_widget(page.upcast_ref(), "goals-tracking-more") {
        page.remove(&old);
    }
    let Some(chat) = ctx.chat.upgrade() else {
        return true;
    };
    let ctx_owned = ctx.clone();
    fill_log("goals", "visit", "sections=roster+tracking");
    let ticket = match chat.page_begin(Op::Goals) {
        Some(ticket) => ticket,
        None => {
            fill_log("goals", "skip-no-ticket", "sections=roster+tracking");
            return false;
        }
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        fill_log("goals", "no-client", "");
        return false;
    };
    let page_owned = page.clone();
    dispatch(
        &chat,
        move || {
            let goals_start = Instant::now();
            let goals = client.goals();
            // The roster paint reads cache only: rows appear the
            // moment the listing lands, and the network never holds
            // them.
            let heroes = goals
                .as_ref()
                .ok()
                .map(|listing| roster_cached_heroes(&listing.items))
                .unwrap_or_default();
            let goals_ms = goals_start.elapsed().as_millis() as u64;
            let tracked_start = Instant::now();
            let tracked = client.goals_tracking();
            let tracked_ms = tracked_start.elapsed().as_millis() as u64;
            (goals, tracked, heroes, goals_ms, tracked_ms)
        },
        move |chat, (goals, tracked, heroes, goals_ms, tracked_ms)| {
            if !chat.page_accept(&ticket) {
                fill_log("goals", "stale-answer-dropped", "sections=roster+tracking");
                return;
            }
            match goals {
                Err(error) => {
                    fill_log(
                        "goals",
                        "failed",
                        &format!("section=roster error=\"{error}\""),
                    );
                    listing_failure(&list, "Goals", &error);
                }
                Ok(listing) => {
                    // The dialog reads its goal from here: hold the
                    // listing and heroes beside the rows on every fill.
                    IDEA_STORE.with(|store| {
                        store.borrow_mut().insert(
                            "goals".to_owned(),
                            IdeaStore {
                                items: listing.items.clone(),
                                heroes: heroes.clone(),
                            },
                        );
                    });
                    paint_roster(&ctx_owned, &list, "goals", &listing.items, &heroes, None);
                    // The web view opens on exactly these rows, and its
                    // entry point stays off until some exist. A failed
                    // roster is not cached: the page keeps what it has.
                    GOAL_STORE.with(|store| {
                        let mut held = store.borrow_mut();
                        held.goals = listing.items.clone();
                        held.goals_total = listing.total;
                    });
                    if let Some(button) = find_widget(page_owned.upcast_ref(), "goals-web")
                        .and_then(|widget| widget.downcast::<gtk4::Button>().ok())
                    {
                        button
                            .set_sensitive(!listing.items.is_empty() && crate::webui::available());
                    }
                    let cached = heroes.iter().filter(|hero| hero.is_some()).count();
                    fill_log(
                        "goals",
                        "painted",
                        &format!(
                            "section=roster rows={} request_ms={goals_ms} heroes_cached={cached}",
                            listing.items.len(),
                        ),
                    );
                    warm_dialog_heroes(chat, "goals", &listing.items, &heroes);
                }
            }
            match tracked {
                Err(error) => {
                    fill_log(
                        "goals",
                        "failed",
                        &format!("section=tracking error=\"{error}\""),
                    );
                    listing_failure(&tracking, "Goals tracking", &error);
                    // The web view reports the same failure and keeps the
                    // Tracking rows the native page still shows: one failed
                    // read must not empty a section that is merely stale.
                    GOAL_STORE.with(|store| {
                        store.borrow_mut().tracking_error = Some(error.to_string());
                    });
                }
                Ok(listing) => {
                    IDEA_STORE.with(|store| {
                        store.borrow_mut().insert(
                            "goals-tracking".to_owned(),
                            IdeaStore {
                                items: listing.items.clone(),
                                heroes: vec![None; listing.items.len()],
                            },
                        );
                    });
                    paint_tracking(&ctx_owned, &tracking, &page_owned, &listing.items, 10);
                    GOAL_STORE.with(|store| {
                        let mut held = store.borrow_mut();
                        held.tracking = listing.items.clone();
                        held.tracking_total = listing.total;
                        held.tracking_error = None;
                    });
                    fill_log(
                        "goals",
                        "painted",
                        &format!(
                            "section=tracking rows={} request_ms={tracked_ms}",
                            listing.items.len(),
                        ),
                    );
                }
            }
        },
    );
    true
}

/// Paint the Tracking section's first `shown` rows plus the "Show
/// more" expander while rows remain. Row taps resolve through the
/// tracking store, so indices stay global across expansions.
fn paint_tracking(
    ctx: &Ctx,
    list: &gtk4::ListBox,
    page: &gtk4::Box,
    items: &[ListingItem],
    shown: usize,
) {
    clear_list(list);
    if let Some(old) = find_widget(page.upcast_ref(), "goals-tracking-more") {
        page.remove(&old);
    }
    for (index, item) in items.iter().enumerate().take(shown) {
        list.append(&goal_row(ctx, item, index));
    }
    if items.len() > shown {
        let more = gtk4::Button::with_label(&format!("Show {} more", items.len() - shown));
        more.set_widget_name("goals-tracking-more");
        more.add_css_class("flat");
        more.set_halign(gtk4::Align::Start);
        page.append(&more);
        let ctx_owned = ctx.clone();
        let list_owned = list.clone();
        let page_owned = page.clone();
        let items = items.to_vec();
        more.connect_clicked(move |_| {
            paint_tracking(&ctx_owned, &list_owned, &page_owned, &items, shown + 10);
        });
    }
}

/// A search visit invites a query; nothing dispatches until Search.
fn fill_search_hint(page: &gtk4::Box) -> bool {
    let Some(list) = named_list(page, "search-list") else {
        return true;
    };
    clear_list(&list);
    empty_note(
        &list,
        "Search every conversation in the account. A result opens its conversation in Chat.",
    );
    true
}

/// Run one account-wide conversation search. A result button opens
/// its conversation: the id becomes the chat's thread scope, and the
/// reader lands in Chat.
fn do_search(ctx: &Ctx, chat: &Rc<Chat>, page: &gtk4::Box) -> bool {
    let Some(list) = named_list(page, "search-list") else {
        return true;
    };
    let query = {
        find_widget(page.upcast_ref(), "search-query")
            .and_then(|widget| widget.downcast::<gtk4::Entry>().ok())
            .map(|entry| entry.text().to_string())
            .unwrap_or_default()
    };
    if query.trim().is_empty() {
        clear_list(&list);
        empty_note(&list, "Type words to search for first.");
        return true;
    }
    fill_log("search", "visit", "");
    let ticket = match chat.page_begin(Op::Search) {
        Some(ticket) => ticket,
        None => {
            fill_log("search", "skip-no-ticket", "");
            return false;
        }
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        fill_log("search", "no-client", "");
        return false;
    };
    let ctx = ctx.clone();
    let page_weak = page.downgrade();
    // The fetch closure takes the query; the paint reports it back, so the
    // web cache names the words that produced these rows.
    let query_for_paint = query.clone();
    dispatch(
        chat,
        move || {
            let start = Instant::now();
            let found = client.search(&query)?;
            // The paint reads cache only: rows appear the moment the
            // results land, and the network never holds them.
            let heroes = roster_cached_heroes(&found.items);
            let request_ms = start.elapsed().as_millis() as u64;
            Ok::<_, muse_account::AccountError>((found, heroes, request_ms))
        },
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                fill_log("search", "stale-answer-dropped", "");
                return;
            }
            let (found, heroes, request_ms) = match answer {
                Ok(tuple) => tuple,
                Err(error) => {
                    fill_log("search", "failed", &format!("error=\"{error}\""));
                    // The failure travels with the query: the web page shows
                    // the words that failed beside the reason.
                    search_cache(HeldSearch {
                        query: query_for_paint,
                        loaded: true,
                        error: Some(error.to_string()),
                        ..HeldSearch::default()
                    });
                    if let Some(page) = page_weak.upgrade()
                        && let Some(button) = named_button(&page, "search-web")
                    {
                        button.set_sensitive(crate::webui::available());
                    }
                    listing_failure(&list, "Search", &error);
                    return;
                }
            };
            // The web cache is filled from the same listing the list
            // paints, so the two cannot word a row differently.
            search_cache(held_search_from(&query_for_paint, &found));
            if let Some(page) = page_weak.upgrade()
                && let Some(button) = named_button(&page, "search-web")
            {
                button.set_sensitive(crate::webui::available());
            }
            clear_list(&list);
            if found.items.is_empty() {
                empty_note(&list, "No matches.");
                fill_log("search", "painted", "rows=0");
                return;
            }
            // Each visit takes a generation so the download pass below
            // touches only its own rows.
            let generation = SEARCH_GEN.with(|slot| {
                let next = slot.get().wrapping_add(1);
                slot.set(next);
                next
            });
            for (index, (item, hero)) in found.items.iter().zip(heroes.iter()).enumerate() {
                let id = item.id.clone().unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                let open = gtk4::Button::new();
                open.set_widget_name(&format!("search-row:{generation}:{index}"));
                open.set_child(Some(&listing_row(item, hero.as_deref())));
                open.set_has_frame(false);
                let ctx = ctx.clone();
                open.connect_clicked(move |_| {
                    if let Some(chat) = ctx.chat.upgrade() {
                        chat.switch_thread(Some(id.clone()));
                    }
                    go_chat(&ctx);
                });
                list.append(&open);
            }
            // The network fills the first screenful behind the paint:
            // no page ticket is held while downloads run, so pressing
            // elsewhere still fills at once.
            let missing: Vec<(usize, String)> = found
                .items
                .iter()
                .enumerate()
                .filter(|(index, _)| *index < super::images::HERO_LANES && heroes[*index].is_none())
                .filter_map(|(index, item)| item.image_url.clone().map(|url| (index, url)))
                .collect();
            fill_log(
                "search",
                "painted",
                &format!(
                    "rows={} request_ms={request_ms} heroes_missing={}",
                    found.items.len(),
                    missing.len(),
                ),
            );
            if !missing.is_empty() {
                let items = found.items.clone();
                let list_weak = list.downgrade();
                let expect = missing.len();
                super::app::dispatch(
                    chat,
                    move || {
                        let start = Instant::now();
                        let landed = super::images::fetch_heroes(missing);
                        let warm_ms = start.elapsed().as_millis() as u64;
                        (landed, warm_ms)
                    },
                    move |_chat, (landed, warm_ms)| {
                        let warmed = landed.iter().filter(|(_, path)| path.is_some()).count();
                        fill_log(
                            "search",
                            "heroes-landed",
                            &format!("expect={expect} warmed={warmed} warm_ms={warm_ms}"),
                        );
                        let Some(list) = list_weak.upgrade() else {
                            return;
                        };
                        for (index, path) in landed {
                            let Some(path) = path else { continue };
                            let Some(item) = items.get(index) else {
                                continue;
                            };
                            // Framed rows render live markup, never a
                            // hero picture: leave their frame alone.
                            if item
                                .widget_html
                                .as_deref()
                                .is_some_and(|html| !html.is_empty())
                            {
                                continue;
                            }
                            let name = format!("search-row:{generation}:{index}");
                            if let Some(row) = find_widget(list.upcast_ref(), &name)
                                .and_then(|widget| widget.downcast::<gtk4::Button>().ok())
                            {
                                row.set_child(Some(&listing_row(item, Some(&path))));
                            }
                        }
                    },
                );
            }
        },
    );
    true
}

/// Land in Chat: show the page, select it in the sidebar.
fn go_chat(ctx: &Ctx) {
    ctx.stack.set_visible_child_name("chat");
    if let Some(first) = ctx.sidebar.row_at_index(0) {
        ctx.sidebar.select_row(Some(&first));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(title: &str, enabled: bool) -> Schedule {
        Schedule {
            id: "job-1".to_owned(),
            title: title.to_owned(),
            enabled,
            timezone: None,
            next_run_at_utc: None,
        }
    }

    #[test]
    fn sidebar_selects_only_on_change() {
        assert!(needs_select(None, 2));
        assert!(needs_select(Some(1), 2));
        assert!(!needs_select(Some(2), 2));
    }

    #[test]
    fn rail_closes_only_the_open_page() {
        assert!(rail_closes(Some("feed"), "feed", true));
        assert!(!rail_closes(None, "feed", true));
        assert!(!rail_closes(Some("goals"), "feed", true));
        assert!(!rail_closes(Some("feed"), "feed", false));
    }

    #[test]
    fn cut_pages_stay_cut() {
        // The owner's call: Memory and Policy have no web counterpart,
        // so they are not pages. Re-adding one must be a decision,
        // never an accident.
        assert!(!PAGES.contains(&"memory"));
        assert!(!PAGES.contains(&"policy"));
    }

    #[test]
    fn roster_hero_resolution_never_blocks_the_paint() {
        // A black hole: accepts connections, never answers. An
        // uncached hero URL pointing here pays the full download
        // timeout -- and the roster paint below must not wait for it.
        let hole = std::net::TcpListener::bind("127.0.0.1:0").expect("hole");
        let port = hole.local_addr().expect("port").port();
        std::thread::spawn(move || {
            for _ in hole.incoming().flatten() {
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        });
        let item = |n: u8| ListingItem {
            title: "t".to_owned(),
            image_url: Some(format!("http://127.0.0.1:{port}/{n}.jpg")),
            ..Default::default()
        };
        let items = [item(1), item(2)];
        let start = std::time::Instant::now();
        let heroes = roster_cached_heroes(&items);
        let elapsed = start.elapsed();
        assert!(heroes.iter().all(|hero| hero.is_none()));
        assert!(
            elapsed < std::time::Duration::from_secs(8),
            "roster paint waited {elapsed:?} on hero downloads",
        );
    }

    #[test]
    fn fill_log_writes_one_greppable_line() {
        let dir = std::env::temp_dir().join(format!("muse-fill-log-line-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        fill_log_to(
            &dir,
            "goals",
            "painted",
            "section=roster rows=3 request_ms=42",
        );
        let text = std::fs::read_to_string(dir.join("page-fill.log")).expect("log");
        let mut parts = text.lines().next().expect("one line").splitn(5, ' ');
        let epoch: u128 = parts.next().expect("epoch").parse().expect("digits");
        assert!(epoch > 0);
        let pid: u32 = parts
            .next()
            .expect("pid")
            .strip_prefix("pid=")
            .expect("pid=")
            .parse()
            .expect("digits");
        assert!(pid > 0);
        assert_eq!(parts.next(), Some("page=goals"));
        assert_eq!(parts.next(), Some("event=painted"));
        assert_eq!(parts.next(), Some("section=roster rows=3 request_ms=42"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fill_log_rotates_instead_of_growing() {
        let dir = std::env::temp_dir().join(format!("muse-fill-log-rotate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        let seed: String = (0..600)
            .map(|n| format!("{n} page=goals event=painted pad={}\n", "x".repeat(400)))
            .collect();
        assert!(seed.len() as u64 > 200 * 1024);
        let first = seed.lines().next().expect("seed").to_owned();
        std::fs::write(dir.join("page-fill.log"), &seed).expect("seed");
        fill_log_to(&dir, "goals", "visit", "");
        let text = std::fs::read_to_string(dir.join("page-fill.log")).expect("log");
        assert!(text.lines().count() <= 401);
        assert!(text.lines().last().expect("last").contains("event=visit"));
        assert_ne!(text.lines().next(), Some(first.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn side_chats_are_the_non_primary_threads_in_order() {
        let thread = |id: &str, primary: bool| muse_account::Thread {
            session_id: id.to_owned(),
            title: id.to_owned(),
            archived: false,
            pinned: false,
            is_thread: true,
            unread_count: 0,
            snippet: None,
            is_primary: primary,
            updated_at: None,
        };
        let (side, main) = split_side_chats(vec![
            thread("a", true),
            thread("b", false),
            thread("c", true),
        ]);
        assert_eq!(
            main.iter()
                .map(|row| row.session_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert_eq!(
            side.iter()
                .map(|row| row.session_id.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
    }

    #[test]
    fn thread_rows_group_by_day_with_times() {
        let today = "2026-11-19";
        assert_eq!(
            thread_day(Some("2026-11-19T12:00:00Z")),
            Some("2026-11-19".to_owned())
        );
        assert_eq!(
            thread_day(Some("2026-11-18T23:59:59Z")),
            Some("2026-11-18".to_owned())
        );
        assert_eq!(thread_day(None), None);
        assert_eq!(thread_day(Some("not a stamp")), None);
        assert_eq!(thread_day(Some("2026-1-1")), None);
        assert_eq!(
            thread_group_label(Some("2026-11-19T12:00:00Z"), today),
            "Today"
        );
        assert_eq!(
            thread_group_label(Some("2026-11-18T23:59:59Z"), today),
            "Yesterday"
        );
        assert_eq!(
            thread_group_label(Some("2026-11-10T08:00:00Z"), today),
            "2026-11-10"
        );
        assert_eq!(
            thread_group_label(Some("2026-11-20T08:00:00Z"), today),
            "Today"
        );
        assert_eq!(thread_group_label(None, today), "Earlier");
        assert_eq!(thread_group_label(Some("garbage"), today), "Earlier");
        assert_eq!(
            thread_time(Some("2026-11-19T12:34:56Z")),
            Some("12:34".to_owned())
        );
        assert_eq!(thread_time(Some("2026-11-19")), None);
        assert_eq!(thread_time(None), None);
        assert_eq!(
            thread_meta(Some("hello"), Some("2026-11-19T12:34:56Z")),
            "hello · 12:34"
        );
        assert_eq!(thread_meta(Some("  hi  "), None), "hi");
        assert_eq!(thread_meta(None, Some("2026-11-19T12:34:56Z")), "12:34");
        assert_eq!(thread_meta(None, None), "");
    }

    #[test]
    fn next_runs_read_in_plain_words() {
        let now = 1_800_000_000;
        assert_eq!(next_run_label(None, now), "no run scheduled");
        assert_eq!(next_run_label(Some(now - 10), now), "due now");
        assert_eq!(next_run_label(Some(now + 5 * 3600), now), "in 5h");
        assert_eq!(next_run_label(Some(now + 30 * 3600), now), "tomorrow");
        assert_eq!(next_run_label(Some(now + 3 * 86400), now), "in 3d");
        assert_eq!(next_run_label(Some(now + 14 * 86400), now), "in 2w");
    }

    #[test]
    fn local_reminder_rows_name_title_due_and_state() {
        let task = |due: u64, status: muse_tasks::TaskStatus, notified: bool| muse_tasks::Task {
            id: 7,
            title: "Water plants".to_owned(),
            body: String::new(),
            due_unix_secs: due,
            status,
            created_unix_secs: 1_000,
            notified,
        };
        let now = 2_000_000_000u64;
        assert_eq!(
            reminder_row_text(
                &task(now + 3600, muse_tasks::TaskStatus::Pending, false),
                now
            ),
            "Water plants · in 1h · pending"
        );
        assert_eq!(
            reminder_row_text(&task(now - 10, muse_tasks::TaskStatus::Pending, true), now),
            "Water plants · due now · pending · notified"
        );
        assert_eq!(
            reminder_row_text(
                &task(now + 7200, muse_tasks::TaskStatus::Paused, false),
                now
            ),
            "Water plants · in 2h · paused"
        );
    }

    #[test]
    fn localhost_phases_compare_by_variant() {
        assert_ne!(
            crate::local_host::Phase::Disconnected,
            crate::local_host::Phase::Registered
        );
    }

    #[test]
    fn localhost_page_names_itself_and_stays_in_the_sidebar() {
        assert_eq!(title("localhost"), "Local host");
        assert!(PAGES.contains(&"localhost"));
        assert_eq!(rail_glyph("localhost"), None);
    }

    #[test]
    fn localhost_grant_rows_name_label_path_and_promise() {
        let root = std::path::Path::new("/home/u/work");
        assert_eq!(
            grant_row_text("Folder 1", root, true, false),
            "Folder 1 · /home/u/work · read-write"
        );
        assert_eq!(
            grant_row_text("Folder 1", root, false, false),
            "Folder 1 · /home/u/work · read-only"
        );
        assert_eq!(
            grant_row_text("Folder 1", root, true, true),
            "Folder 1 · /home/u/work · read-write · writes without a card"
        );
    }

    #[test]
    fn localhost_countdowns_name_a_live_deadline_never_a_stale_one() {
        let soon = Instant::now() + Duration::from_secs(45);
        assert!(approval_countdown(soon).starts_with("Expires in 4"));
        let later = Instant::now() + Duration::from_secs(200);
        assert_eq!(approval_countdown(later), "Expires in 3m");
        let past = Instant::now() - Duration::from_secs(1);
        assert_eq!(approval_countdown(past), "Expired");
    }

    #[test]
    fn localhost_content_previews_cap_but_say_so() {
        assert_eq!(preview_content("abc"), "abc");
        let big = "x".repeat(2500);
        let shown = preview_content(&big);
        assert!(shown.starts_with(&"x".repeat(2000)));
        assert!(shown.contains("2000 characters"));
        assert!(!shown.contains(&"x".repeat(2001)));
    }

    #[test]
    fn rail_glyphs_name_bundled_svgs() {
        assert_eq!(rail_glyph("chat"), Some("chat"));
        assert_eq!(rail_glyph("search"), Some("search"));
        assert_eq!(rail_glyph("feed"), Some("library"));
        assert_eq!(rail_glyph("ideas"), Some("ideas"));
        assert_eq!(rail_glyph("goals"), Some("tasks"));
        assert_eq!(rail_glyph("library"), Some("shapes"));
        assert_eq!(rail_glyph("tasks"), None);
        assert_eq!(rail_glyph("settings"), None);
    }

    #[test]
    fn page_labels_name_state_never_raw_ids() {
        assert_eq!(schedule_label(&schedule("Nightly", true)), "Nightly (on)");
        assert_eq!(schedule_label(&schedule("Nightly", false)), "Nightly (off)");
    }

    /// The page's cards are the panel's cards: the native headline, the
    /// service's detail rows verbatim, and the same five outcome words with
    /// the same labels. A sixth outcome, or a reworded one, cannot reach it.
    #[test]
    fn approval_cards_keep_the_native_headline_and_the_five_words() {
        let list = muse_account::PendingApprovals {
            pending: vec![muse_account::Approval {
                approval_id: "a-1".to_owned(),
                kind: Some("network".to_owned()),
                decision: Some("ask".to_owned()),
                always_scope: Some("destination_domain".to_owned()),
                attention_mode: None,
                title: "Reach example.com".to_owned(),
                summary: "The agent wants to fetch a page.".to_owned(),
                details: vec![muse_account::ApprovalDetail {
                    label: "Host".to_owned(),
                    value: "example.com".to_owned(),
                }],
            }],
        };
        let cards = held_approvals_from(&list);
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!(card.id, "a-1");
        assert_eq!(
            card.headline,
            crate::app::approval_headline(&list.pending[0])
        );
        assert!(card.headline.contains("Reach example.com"));
        assert_eq!(card.summary, "The agent wants to fetch a page.");
        assert_eq!(card.details, vec!["Host: example.com".to_owned()]);
        assert_eq!(
            card.choices
                .iter()
                .map(|choice| choice.word.as_str())
                .collect::<Vec<_>>(),
            [
                "allow_once",
                "allow_task",
                "allow_site",
                "allow_always",
                "deny"
            ]
        );
        assert_eq!(card.choices[0].label, "Allow once");
        assert_eq!(card.choices[4].label, "Deny");
    }

    /// The snapshot the page polls carries the approvals the panel painted,
    /// merged from their own store: a render never has to know about them, and
    /// a page reading the transcript still gets the pending list beside it.
    #[test]
    fn the_chat_snapshot_carries_the_approvals_the_panel_painted() {
        chat_cache(HeldChat::default());
        approvals_cache(ApprovalsView {
            cards: vec![ApprovalCard {
                id: "a-7".to_owned(),
                ..ApprovalCard::default()
            }],
            loaded: true,
            note: None,
        });
        let held = chat_snapshot();
        assert_eq!(held.approvals.cards.len(), 1);
        assert_eq!(held.approvals.cards[0].id, "a-7");
        assert!(held.approvals.loaded);
        assert!(held.approvals.note.is_none());
        approvals_cache(ApprovalsView::default());
    }

    #[test]
    fn run_labels_prefer_the_human_summary() {
        let plain = TaskRun {
            run_id: "r".to_owned(),
            job_id: "j".to_owned(),
            status: "done".to_owned(),
            result_summary: None,
            error_text: None,
            scheduled_for_utc: None,
        };
        assert_eq!(run_label(&plain), "done");
        let summarized = TaskRun {
            result_summary: Some("3 files".to_owned()),
            ..plain
        };
        assert_eq!(run_label(&summarized), "done: 3 files");
    }

    #[test]
    fn feed_cards_frame_the_title_with_service_meta() {
        let bare = ListingItem {
            title: "Morning".to_owned(),
            ..Default::default()
        };
        assert_eq!(feed_item_label(&bare), "Morning");
        let framed = ListingItem {
            title: "Morning".to_owned(),
            kicker: Some("Digest".to_owned()),
            kind: Some("edition".to_owned()),
            ..Default::default()
        };
        assert_eq!(feed_item_label(&framed), "Morning\nDigest \u{00b7} edition");
        let iconed = ListingItem {
            title: "Morning".to_owned(),
            icon: Some("📰".to_owned()),
            ..Default::default()
        };
        assert_eq!(feed_item_label(&iconed), "📰 Morning");
    }

    #[test]
    fn feed_times_read_minutes_hours_days() {
        // 2026-11-17T13:54:08Z is a Monday; the web would call it
        // "22m ago" at 14:16, "2h ago" at 16:00, Yesterday on the
        // 18th, and "Nov 17" past that.
        let noonish = |hour: u32, min: u32| {
            super::super::app::days_from_civil(2026, 11, 17) as i64 * 86_400
                + hour as i64 * 3600
                + min as i64 * 60
        };
        let stamp = Some("2026-11-17T13:54:08.702000+00:00");
        assert_eq!(relative_time(stamp, noonish(14, 16)), "22m ago");
        assert_eq!(relative_time(stamp, noonish(16, 0)), "2h ago");
        assert_eq!(relative_time(stamp, noonish(23, 59)), "10h ago");
        assert_eq!(
            relative_time(
                stamp,
                super::super::app::days_from_civil(2026, 11, 19) as i64 * 86_400
            ),
            "Yesterday"
        );
        assert_eq!(
            relative_time(
                stamp,
                super::super::app::days_from_civil(2026, 11, 20) as i64 * 86_400
            ),
            "Nov 17"
        );
        assert_eq!(relative_time(None, noonish(14, 16)), "");
        assert_eq!(relative_time(Some("garbage"), noonish(14, 16)), "");
    }

    #[test]
    fn feed_bodies_escape_linkify_and_bold() {
        let markup = feed_body_markup("See https://example.com/a. **Bold** <tag>");
        assert!(markup.contains("<a href=\"https://example.com/a\">https://example.com/a</a>"));
        assert!(markup.contains("<b>Bold</b>"));
        assert!(markup.contains("&lt;tag&gt;"));
        // Trailing punctuation stays outside the link.
        let dotted = feed_body_markup("Go https://example.com/a, now.");
        assert!(dotted.ends_with("</a>, now."));
        // Markdown links beat raw text: the label reads, the address hides.
        let linked = feed_body_markup(
            "An unauthenticated attacker can [bypass PowerStudio SCADA authentication](https://radar.offseq.com/threat-1) today.",
        );
        assert!(linked.contains("<a href=\"https://radar.offseq.com/threat-1\">bypass PowerStudio SCADA authentication</a>"));
        assert!(!linked.contains("[bypass"));
        // An image marker keeps its literal label; the address inside
        // still links like any bare URL.
        let image = feed_body_markup("See ![alt](https://example.com/a.png) here.");
        assert!(!image.contains(">alt</a>"));
        assert!(image.contains("https://example.com/a.png"));
    }

    #[test]
    fn idea_rows_group_by_collection_first() {
        let bare = ListingItem {
            title: "X".to_owned(),
            ..Default::default()
        };
        assert_eq!(idea_group(&bare), None);
        let categorized = ListingItem {
            category: Some("Productivity".to_owned()),
            kind: Some("Kind".to_owned()),
            kicker: Some("Kicker".to_owned()),
            ..Default::default()
        };
        assert_eq!(idea_group(&categorized), Some("Productivity"));
        let fallback = ListingItem {
            kicker: Some("Your UFP onboarding".to_owned()),
            ..Default::default()
        };
        assert_eq!(idea_group(&fallback), Some("Your UFP onboarding"));
    }

    #[test]
    fn feed_cache_round_trips_a_listing() {
        let listing = ServiceListing {
            items: vec![ListingItem {
                title: "Morning".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&FeedCache { listing }).expect("cache serializes");
        let back: FeedCache = serde_json::from_slice(&bytes).expect("cache parses");
        assert_eq!(back.listing.items.len(), 1);
        assert_eq!(back.listing.items[0].title, "Morning");
    }

    #[test]
    fn feed_header_states_read_plainly() {
        assert_eq!(
            feed_meta_line(&ListingItem {
                kicker: Some("Exploited in the wild".to_owned()),
                category: Some("Development".to_owned()),
                ..Default::default()
            }),
            Some("Exploited in the wild · Development".to_owned())
        );
        assert_eq!(
            feed_meta_line(&ListingItem {
                kicker: Some("Solo".to_owned()),
                ..Default::default()
            }),
            Some("Solo".to_owned())
        );
        assert_eq!(feed_meta_line(&ListingItem::default()), None);
        assert_eq!(
            feed_status_line(Some(&muse_account::FeedStatus {
                refresh_in_progress: true,
                ..Default::default()
            })),
            Some("Generating a fresh edition…".to_owned())
        );
        assert_eq!(
            feed_status_line(Some(&muse_account::FeedStatus::default())),
            None
        );
        assert_eq!(feed_status_line(None), None);
    }

    #[test]
    fn library_tabs_and_filters_come_only_from_data_in_hand() {
        let space = ArtifactSummary {
            title: "UFP Onboarding".to_owned(),
            target_kind: Some("space".to_owned()),
            ..Default::default()
        };
        let doc = ArtifactSummary {
            title: "Notes".to_owned(),
            kind: Some("Documents".to_owned()),
            ..Default::default()
        };
        let items = vec![space, doc];
        assert_eq!(
            library_tabs(&items),
            vec![
                "All".to_owned(),
                "Spaces".to_owned(),
                "Documents".to_owned()
            ]
        );
        assert!(library_visible(&items[0], "Spaces", ""));
        assert!(!library_visible(&items[1], "Spaces", ""));
        assert!(library_visible(&items[1], "Documents", "notes"));
        assert!(!library_visible(&items[1], "Documents", "zzz"));
        assert!(library_visible(&items[0], "All", ""));
        let bare: Vec<ArtifactSummary> = vec![ArtifactSummary::default()];
        assert_eq!(library_tabs(&bare), vec!["All".to_owned()]);
    }

    #[test]
    fn artifact_meta_and_ms_times_read_plainly() {
        let now = 1_000_000_000i64;
        assert_eq!(relative_time_ms(Some(999_999_000_000), now), "16m ago");
        assert_eq!(relative_time_ms(None, now), "");
        let dated = ArtifactSummary {
            kind: Some("HTML".to_owned()),
            updated_at_ms: Some(999_913_600_000),
            ..Default::default()
        };
        assert_eq!(artifact_meta(&dated, now), "HTML · Yesterday");
        let bare = ArtifactSummary::default();
        assert_eq!(artifact_meta(&bare, now), "");
    }

    #[test]
    fn goal_filters_come_only_from_data_in_hand() {
        let items = vec![
            ListingItem {
                category: Some("Health".to_owned()),
                ..Default::default()
            },
            ListingItem {
                category: Some("Finance".to_owned()),
                ..Default::default()
            },
            ListingItem {
                category: Some("Health".to_owned()),
                ..Default::default()
            },
            ListingItem::default(),
        ];
        assert_eq!(
            goal_categories(&items),
            vec!["Health".to_owned(), "Finance".to_owned()]
        );
        assert!(roster_visible(&items[0], "goals", Some("Health")));
        assert!(!roster_visible(&items[1], "goals", Some("Health")));
        assert!(roster_visible(&items[1], "goals", None));
        assert!(roster_visible(&items[0], "ideas", Some("Health")));
    }

    #[test]
    fn feed_rows_carry_the_service_action_or_discuss() {
        let actionable = ListingItem {
            action_label: Some("Let's do it".to_owned()),
            ..Default::default()
        };
        assert_eq!(feed_row_cta(&actionable), Some("Let's do it".to_owned()));
        let plain = ListingItem::default();
        assert_eq!(feed_row_cta(&plain), None);
        let blank = ListingItem {
            action_label: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(feed_row_cta(&blank), None);
    }

    #[test]
    fn failed_listings_name_the_failure() {
        assert_eq!(
            listing_failure_text("Feed", &"the bridge went away"),
            "Feed didn't load: the bridge went away"
        );
    }

    #[test]
    fn idea_writes_report_acceptance_plainly() {
        assert_eq!(execute_notice(true), "Started — find it under Tasks.");
        assert_eq!(
            execute_notice(false),
            "The service did not start it. Nothing changed."
        );
        assert_eq!(react_notice("👍", true), "Noted — more like this.");
        assert_eq!(
            react_notice("👎", true),
            "Noted — you will not see this again."
        );
        assert_eq!(
            react_notice("👍", false),
            "The service did not record it. Nothing changed."
        );
    }

    #[test]
    fn idea_rows_show_a_clamped_glance() {
        assert_eq!(idea_snippet(None), "");
        assert_eq!(idea_snippet(Some("")), "");
        assert_eq!(
            idea_snippet(Some("Pitch a film podcast.\nSecond line.")),
            "Pitch a film podcast. Second line."
        );
        let long = format!("{}\nSecond line.", "x".repeat(300));
        let snippet = idea_snippet(Some(&long));
        assert!(snippet.ends_with("…"));
        assert!(snippet.chars().count() <= 241);
    }

    #[test]
    fn held_policy_names_the_panels_own_flips_and_nothing_else() {
        let policy = muse_account::PolicySettings {
            entries: vec![
                muse_account::PolicyEntry {
                    label: "connector_default".into(),
                    value: "always_ask".into(),
                },
                muse_account::PolicyEntry {
                    label: "unflippable".into(),
                    value: "fixed".into(),
                },
            ],
            connectors: vec!["calendar".into()],
            withheld: 3,
        };
        let view = held_policy_from(&policy, |label, value| {
            (label == "connector_default" && value == "always_ask").then(|| "auto_allow".to_owned())
        });
        assert!(view.loaded, "a read that answered is a read that carries");
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.rows[0].flip.as_deref(), Some("auto_allow"));
        assert_eq!(view.rows[1].flip, None, "no flip the panel would not offer");
        assert_eq!(view.connectors, vec!["calendar".to_owned()]);
        assert_eq!(view.withheld, 3);
    }
}
