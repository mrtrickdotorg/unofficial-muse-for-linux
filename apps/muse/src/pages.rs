//! Secondary surfaces for the rewrite: threads, settings, and the
//! lazy page builders behind the sidebar. Each page builds its
//! structure once and refills on every visit, so a revisit never
//! shows a stale list. Service reads ride the chat's ticket slot;
//! pages never dispatch directly.
//!
//! The surfaces land in route order across this milestone; this file
//! grows one page at a time.

use std::rc::{Rc, Weak};

use adw::prelude::*;

use super::app::{Chat, dispatch};
use super::session::account_dir;
use muse_account::{
    ActiveGrant, ArtifactSummary, GrantItem, ListingItem, MemoryHit, Schedule, ServiceListing,
    TaskRun,
};

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
    "chat", "threads", "tasks", "library", "feed", "memory", "search", "ideas", "goals", "policy",
    "settings",
];

/// Human titles for the sidebar rows.
pub fn title(name: &str) -> &str {
    match name {
        "chat" => "Chat",
        "threads" => "Threads",
        "tasks" => "Tasks",
        "library" => "Library",
        "feed" => "Feed",
        "memory" => "Memory",
        "policy" => "Policy",
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

/// Show a page: build its structure on first visit, refill on every
/// visit, then navigate the stack and the sidebar together.
pub fn show(ctx: &Ctx, name: &str) {
    let existing = ctx.stack.child_by_name(name);
    if existing.is_none() {
        let page = build(ctx, name);
        ctx.stack.add_named(&page, Some(name));
    }
    fill(ctx, name);
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
}

fn build(ctx: &Ctx, name: &str) -> gtk4::Box {
    let inner = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    inner.set_margin_start(16);
    inner.set_margin_end(16);
    inner.set_margin_top(12);
    match name {
        "threads" => {
            let list = gtk4::ListBox::new();
            list.set_widget_name("threads-list");
            inner.append(&list);
            let weak = ctx.chat.clone();
            let stack = ctx.stack.clone();
            let sidebar = ctx.sidebar.clone();
            list.connect_row_selected(move |list, row| {
                let Some(row) = row else { return };
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
            let schedules = gtk4::ListBox::new();
            schedules.set_widget_name("tasks-schedules");
            inner.append(&schedules);
            let runs = gtk4::ListBox::new();
            runs.set_widget_name("tasks-runs");
            inner.append(&runs);
        }
        "feed" => {
            // The web opens on its prompt card above the units: the
            // prompt fills on every visit beside the list, with Edit
            // and the generation state beside it.
            let heading = gtk4::Label::new(Some("Feed"));
            heading.add_css_class("muse-page-title");
            heading.set_halign(gtk4::Align::Start);
            inner.append(&heading);
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
            // Editing has no backend yet: the button reads, plainly.
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
                let note =
                    gtk4::Label::new(Some("Prompt editing isn't available in this client yet."));
                note.set_wrap(true);
                note.set_xalign(0.0);
                body.append(&note);
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
        "memory" => {
            let search = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            let entry = gtk4::Entry::new();
            entry.set_widget_name("memory-query");
            entry.set_hexpand(true);
            entry.set_placeholder_text(Some("Search memory"));
            search.append(&entry);
            let go = gtk4::Button::with_label("Search");
            go.set_widget_name("memory-go");
            search.append(&go);
            inner.append(&search);
            let list = gtk4::ListBox::new();
            list.set_widget_name("memory-list");
            inner.append(&list);
            let weak = ctx.chat.clone();
            let page_weak = inner.downgrade();
            go.connect_clicked(move |_| {
                let Some(inner) = page_weak.upgrade() else {
                    return;
                };
                let Some(chat) = weak.upgrade() else { return };
                fill_memory(&chat, &inner);
            });
        }
        "search" => {
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
            inner.append(&heading);
            // The web's roster is one scrolling list: icon, title,
            // clamped description, overflow menu. Ideas rows open the
            // detail dialog; the page itself does not drill in.
            let list = gtk4::ListBox::new();
            list.set_widget_name(&format!("{name}-list"));
            list.set_selection_mode(gtk4::SelectionMode::Single);
            inner.append(&list);
            // Ideas rows open the detail dialog; goals stay inert
            // until the web's goals page is read the same way.
            if name == "ideas" {
                let ctx_owned = ctx.clone();
                list.connect_row_selected(move |list, row| {
                    let Some(row) = row else { return };
                    open_idea_dialog(&ctx_owned, list, row);
                });
            }
        }
        "policy" => {
            let notice = gtk4::Label::new(None);
            notice.set_widget_name("policy-notice");
            notice.set_wrap(true);
            notice.set_xalign(0.0);
            notice.set_visible(false);
            inner.append(&notice);
            let active = gtk4::ListBox::new();
            active.set_widget_name("policy-active");
            inner.append(&active);
            let list = gtk4::ListBox::new();
            list.set_widget_name("policy-list");
            inner.append(&list);
        }
        "library" => {
            let heading = gtk4::Label::new(Some("Library"));
            heading.add_css_class("muse-page-title");
            heading.set_halign(gtk4::Align::Start);
            inner.append(&heading);
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

fn fill(ctx: &Ctx, name: &str) {
    let Some(page) = ctx.stack.child_by_name(name) else {
        return;
    };
    let page: gtk4::Box = page.downcast().expect("page box");
    match name {
        "chat" => {
            if let Some(chat) = ctx.chat.upgrade() {
                chat.refresh_panel();
            }
        }
        "threads" => fill_threads(ctx, &page),
        "tasks" => fill_tasks(ctx, &page),
        "library" => fill_library(ctx, &page),
        "search" => fill_search_hint(&page),
        "ideas" => fill_ideas(ctx, &page),
        "goals" => fill_goals(ctx, &page),
        "feed" => fill_feed(ctx, &page),
        "memory" => fill_memory_opt(ctx, &page),
        "policy" => fill_policy(ctx, &page),
        "settings" => fill_settings(ctx, &page),
        _ => {}
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
    // Reactions: the glyph is the vote. Acceptance shows inline;
    // proof is the next visit, never the answer alone.
    for (button, glyph) in [(like, "👍"), (dislike, "👎")] {
        let notice_held = notice.clone();
        let popover_held = popover.clone();
        let ctx_held = ctx.clone();
        let page_held = page.clone();
        let unit_held = unit_id.clone();
        button.connect_clicked(move |_| {
            let notice = notice_held.clone();
            let popover = popover_held.clone();
            let ctx = ctx_held.clone();
            let page = page_held.clone();
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
            let reaction = glyph.to_owned();
            super::app::dispatch(
                &chat,
                move || client.feed_react(&id, &reaction),
                move |chat, answer| {
                    let accepted = answer.as_ref().map(|ok| ok.accepted).unwrap_or(false);
                    notice.set_text(&react_notice(glyph, accepted));
                    notice.set_visible(true);
                    if accepted && glyph == "👎" {
                        show(&ctx, &page);
                    }
                    let _ = chat;
                },
            );
        });
    }
    window.present();
}

/// Fill the tasks page: schedules with a Run button each, plus the run
/// history. "Run now" is single-press like send: the run holds the
/// shared page slot, so a double press waits instead of starting two
/// runs of the same job.
fn fill_tasks(ctx: &Ctx, page: &gtk4::Box) {
    let Some(schedules) = named_list(page, "tasks-schedules") else {
        return;
    };
    let Some(runs) = named_list(page, "tasks-runs") else {
        return;
    };
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    let ticket = match chat.page_begin() {
        Some(ticket) => ticket,
        None => return,
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return;
    };
    let chat_strong = chat.clone();
    dispatch(
        &chat,
        move || client.tasks(),
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let Ok(snapshot) = answer else { return };
            clear_list(&schedules);
            for schedule in &snapshot.schedules {
                let row = gtk4::ListBoxRow::new();
                let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                let label = gtk4::Label::new(Some(&schedule_label(schedule)));
                label.set_hexpand(true);
                label.set_xalign(0.0);
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
                runs.append(&gtk4::Label::new(Some(&run_label(run))));
            }
        },
    );
}

/// Start exactly one run of a schedule: the ticket makes a double
/// press wait, and the refill proves what actually started.
fn run_schedule(chat: &Rc<Chat>, job_id: &str) {
    let ticket = match chat.page_begin() {
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
        list.append(&gtk4::Label::new(Some("Nothing here yet.")));
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
    more.set_tooltip_text(Some("File options"));
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

fn fill_library(ctx: &Ctx, page: &gtk4::Box) {
    let (Some(list), Some(tabs), Some(entry)) = (
        named_list(page, "library-list"),
        find_widget(page.upcast_ref(), "library-tabs")
            .and_then(|widget| widget.downcast::<gtk4::Box>().ok()),
        find_widget(page.upcast_ref(), "library-search")
            .and_then(|widget| widget.downcast::<gtk4::Entry>().ok()),
    ) else {
        return;
    };
    // A fresh fill resets the view: stale queries against new rows
    // read as broken, never as empty.
    entry.set_text("");
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    let ctx_owned = ctx.clone();
    let ticket = match chat.page_begin() {
        Some(ticket) => ticket,
        None => {
            eprintln!("muse: library pressed while the session slot is held");
            return;
        }
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        clear_list(&list);
        list.append(&gtk4::Label::new(Some("Link your account first.")));
        return;
    };
    clear_list(&list);
    let loading = gtk4::Label::new(Some("Loading the library…"));
    loading.add_css_class("dim-label");
    loading.set_xalign(0.0);
    list.append(&loading);
    dispatch(
        &chat,
        move || client.artifacts(),
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let Ok(listing) = answer else {
                // Errors read, never vanish: the page names the
                // failure so the reader isn't left guessing.
                if let Err(error) = answer {
                    listing_failure(&list, "Library", &error);
                }
                return;
            };
            ART_STORE.with(|store| {
                *store.borrow_mut() = listing.artifacts.clone();
            });
            paint_library(
                &ctx_owned,
                &list,
                &tabs,
                &entry,
                &listing.artifacts,
                "All",
                "",
            );
        },
    );
}

fn fill_threads(ctx: &Ctx, page: &gtk4::Box) {
    let Some(list) = named_list(page, "threads-list") else {
        return;
    };
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let Some(chat) = ctx.chat.upgrade() else {
        list.append(&gtk4::Label::new(Some("Link your account first.")));
        return;
    };
    let mut threads = chat.threads();
    if threads.is_empty() {
        list.append(&gtk4::Label::new(Some("No threads yet.")));
        return;
    }
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.pinned));
    for thread in threads {
        let row = gtk4::ListBoxRow::new();
        row.set_widget_name(&format!("thread:{}", thread.session_id));
        let label = gtk4::Label::new(Some(if thread.title.is_empty() {
            "(untitled)"
        } else {
            &thread.title
        }));
        label.set_xalign(0.0);
        if thread.pinned {
            label.set_text(&format!("\u{1f4cc} {}", label.text()));
        }
        row.set_child(Some(&label));
        list.append(&row);
    }
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

// True while the history refill is marking the open thread. Marking
// re-fires row_selected, whose handler calls straight back into
// show(): the mark is a refill echo, never a reader press.
thread_local! {
    pub(crate) static MARKING_HISTORY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
        row.set_widget_name(&format!("thread:{}", thread.session_id));
        let label = gtk4::Label::new(Some(if thread.title.is_empty() {
            "(untitled)"
        } else {
            &thread.title
        }));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        if thread.pinned {
            label.set_text(&format!("\u{1f4cc} {}", label.text()));
        }
        row.set_child(Some(&label));
        ctx.history.append(&row);
        if current.as_deref() == Some(thread.session_id.as_str()) {
            MARKING_HISTORY.set(true);
            ctx.history.select_row(Some(&row));
            MARKING_HISTORY.set(false);
        }
    }
    let name = chat.session_name().unwrap_or_else(|| "Sign in".to_owned());
    ctx.profile_name.set_text(&name);
}

fn fill_settings(ctx: &Ctx, page: &gtk4::Box) {
    let Some(info) = find_widget(page.upcast_ref(), "settings-info")
        .and_then(|widget| widget.downcast::<gtk4::Label>().ok())
    else {
        return;
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
}

/// Paint the updates section from the shared panel state.
fn paint_updates(ctx: &Ctx, page: &gtk4::Box) {
    let find = |name: &str| find_widget(page.upcast_ref(), name);
    let state = ctx.updates.borrow();
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
    more.set_tooltip_text(Some("Feed unit options"));
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
                        pressed_inner.set_tooltip_text(Some("Loved"));
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

/// One memory hit: the passage truncated to a readable length plus its
/// location when the service gave one.
pub fn memory_hit_label(hit: &MemoryHit) -> String {
    const LIMIT: usize = 200;
    let text: String = hit.text.chars().take(LIMIT + 1).collect();
    let text = if text.chars().count() > LIMIT {
        format!("{}…", text.chars().take(LIMIT).collect::<String>())
    } else {
        text
    };
    match hit.path.as_deref() {
        Some(path) if !path.is_empty() => format!("{}\n{}", text, path),
        _ => text,
    }
}

/// One grant row: who holds what, with the capability count.
pub fn grant_label(grant: &GrantItem) -> String {
    format!(
        "{} {} ({} {})",
        grant.subject_kind,
        grant.subject_id,
        grant.capabilities.len(),
        if grant.capabilities.len() == 1 {
            "capability"
        } else {
            "capabilities"
        }
    )
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

fn fill_feed(ctx: &Ctx, page: &gtk4::Box) {
    let Some(list) = named_list(page, "feed-list") else {
        return;
    };
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    let ticket = match chat.page_begin() {
        Some(ticket) => ticket,
        None => return,
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
        return;
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

/// Memory fills on demand: a visit with a query runs it, a visit
/// without one only invites a query. Blank queries never dispatch --
/// the service rejects them.
fn fill_memory_opt(ctx: &Ctx, page: &gtk4::Box) {
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    fill_memory(&chat, page);
}

fn fill_memory(chat: &Rc<Chat>, page: &gtk4::Box) {
    let Some(list) = named_list(page, "memory-list") else {
        return;
    };
    let query = {
        find_widget(page.upcast_ref(), "memory-query")
            .and_then(|widget| widget.downcast::<gtk4::Entry>().ok())
            .map(|entry| entry.text().to_string())
            .unwrap_or_default()
    };
    if query.trim().is_empty() {
        clear_list(&list);
        list.append(&gtk4::Label::new(Some(
            "Type a query above and press Search.",
        )));
        return;
    }
    let ticket = match chat.page_begin() {
        Some(ticket) => ticket,
        None => return,
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return;
    };
    dispatch(
        chat,
        move || client.memory_search(&query),
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let Ok(found) = answer else { return };
            clear_list(&list);
            if found.hits.is_empty() {
                list.append(&gtk4::Label::new(Some("No matches.")));
                return;
            }
            for hit in &found.hits {
                let card = gtk4::Label::new(Some(&memory_hit_label(hit)));
                card.set_wrap(true);
                card.set_xalign(0.0);
                list.append(&card);
            }
        },
    );
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

/// One goal row: an insensitive completion checkbox (the service offers
/// no completion write, so nothing pretends to stick), title plus
/// clamped description, and the ⋯ menu. The row itself stays inert:
/// the web's row-press destination was never observed.
fn goal_row(ctx: &Ctx, item: &ListingItem, _index: usize) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
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
    more.set_tooltip_text(Some("Goal options"));
    line.append(&more);
    row.set_child(Some(&line));
    // The web's five verbs. Only Delete has a backend; the rest read
    // plainly instead of pretending.
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
    for (button, verb) in [
        (rename, "Rename"),
        (subgoal, "Add subgoal"),
        (complete, "Complete"),
    ] {
        let line_inner = line.clone();
        let popover_held = popover.clone();
        button.connect_clicked(move |_| {
            popover_held.popdown();
            line_inner.append(&card_note(&format!(
                "{verb} isn't available for goals yet."
            )));
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
        list.append(&gtk4::Label::new(Some("Nothing here yet.")));
    } else if let Some(row) = list.row_at_index(0) {
        list.select_row(Some(&row));
    }
}

fn fill_service_listing<F>(ctx: &Ctx, list: &gtk4::ListBox, fetch: F)
where
    F: FnOnce(
            &muse_account::AccountClient,
        ) -> Result<muse_account::ServiceListing, muse_account::AccountError>
        + Send
        + 'static,
{
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    let ctx_owned = ctx.clone();
    let ticket = match chat.page_begin() {
        Some(ticket) => ticket,
        None => return,
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return;
    };
    let list = list.clone();
    dispatch(
        &chat,
        move || {
            let listing = fetch(&client)?;
            // Artwork prefetches here, off the main thread: the first
            // few units with a hero URL get their cached files, and the
            // painter below only ever reads the disk.
            let heroes: Vec<Option<std::path::PathBuf>> = listing
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    if index < 8 {
                        item.image_url
                            .as_deref()
                            .and_then(super::images::fetch_hero)
                    } else {
                        None
                    }
                })
                .collect();
            Ok::<_, muse_account::AccountError>((listing, heroes))
        },
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let Ok((listing, heroes)) = answer else {
                return;
            };
            // The dialog reads its unit from here: hold the listing
            // and heroes beside the rows on every fill.
            let page = list.widget_name().to_string();
            let page = page.strip_suffix("-list").unwrap_or(&page).to_owned();
            IDEA_STORE.with(|store| {
                store.borrow_mut().insert(
                    page.clone(),
                    IdeaStore {
                        items: listing.items.clone(),
                        heroes: heroes.clone(),
                    },
                );
            });
            paint_roster(&ctx_owned, &list, &page, &listing.items, &heroes, None);
        },
    );
}

fn fill_ideas(ctx: &Ctx, page: &gtk4::Box) {
    let Some(list) = named_list(page, "ideas-list") else {
        return;
    };
    fill_service_listing(ctx, &list, |client| client.ideas());
}

fn fill_goals(ctx: &Ctx, page: &gtk4::Box) {
    let Some(list) = named_list(page, "goals-list") else {
        return;
    };
    fill_service_listing(ctx, &list, |client| client.goals());
}

/// A search visit invites a query; nothing dispatches until Search.
fn fill_search_hint(page: &gtk4::Box) {
    let Some(list) = named_list(page, "search-list") else {
        return;
    };
    clear_list(&list);
    list.append(&gtk4::Label::new(Some(
        "Search every conversation in the account. A result opens its conversation in Chat.",
    )));
}

/// Run one account-wide conversation search. A result button opens
/// its conversation: the id becomes the chat's thread scope, and the
/// reader lands in Chat.
fn do_search(ctx: &Ctx, chat: &Rc<Chat>, page: &gtk4::Box) {
    let Some(list) = named_list(page, "search-list") else {
        return;
    };
    let query = {
        find_widget(page.upcast_ref(), "search-query")
            .and_then(|widget| widget.downcast::<gtk4::Entry>().ok())
            .map(|entry| entry.text().to_string())
            .unwrap_or_default()
    };
    if query.trim().is_empty() {
        clear_list(&list);
        list.append(&gtk4::Label::new(Some("Type words to search for first.")));
        return;
    }
    let ticket = match chat.page_begin() {
        Some(ticket) => ticket,
        None => return,
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return;
    };
    let ctx = ctx.clone();
    dispatch(
        chat,
        move || {
            let found = client.search(&query)?;
            let heroes: Vec<Option<std::path::PathBuf>> = found
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    if index < 8 {
                        item.image_url
                            .as_deref()
                            .and_then(super::images::fetch_hero)
                    } else {
                        None
                    }
                })
                .collect();
            Ok::<_, muse_account::AccountError>((found, heroes))
        },
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let Ok((found, heroes)) = answer else { return };
            clear_list(&list);
            if found.items.is_empty() {
                list.append(&gtk4::Label::new(Some("No matches.")));
                return;
            }
            for (item, hero) in found.items.iter().zip(heroes.iter()) {
                let id = item.id.clone().unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                let open = gtk4::Button::new();
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
        },
    );
}

/// Land in Chat: show the page, select it in the sidebar.
fn go_chat(ctx: &Ctx) {
    ctx.stack.set_visible_child_name("chat");
    if let Some(first) = ctx.sidebar.row_at_index(0) {
        ctx.sidebar.select_row(Some(&first));
    }
}

/// One active grant: what it covers, in the service's own words.
/// Only revocable rows grow a button; the service's mark decides.
pub fn active_grant_label(grant: &ActiveGrant) -> String {
    let mut meta: Vec<&str> = [
        &grant.kind,
        &grant.mode,
        &grant.policy_subject,
        &grant.lifetime,
    ]
    .into_iter()
    .flatten()
    .map(String::as_str)
    .filter(|text| !text.is_empty())
    .collect();
    if !grant.revocable {
        meta.push("service-marked irrevocable");
    }
    if meta.is_empty() {
        grant.permission_id.clone()
    } else {
        format!("{}\n{}", grant.permission_id, meta.join(" \u{00b7} "))
    }
}

/// The revoke outcome line: acceptance removes the row locally and the
/// refill proves it; anything else names the failure and warns the
/// grant may still be in force.
pub fn revoke_notice(accepted: bool) -> &'static str {
    if accepted {
        "The service accepted the revocation. Reading the list again to confirm the grant is gone."
    } else {
        "The service did not accept the revocation. The grant is still in force."
    }
}

/// Fill the policy page: revocable active grants with one-press
/// Revoke buttons, plus the read-only issued inventory. A revocation
/// holds the shared page slot, so a second press waits; the refill
/// after an accepted revoke proves the grant is gone.
fn fill_policy(ctx: &Ctx, page: &gtk4::Box) {
    let Some(active) = named_list(page, "policy-active") else {
        return;
    };
    let Some(list) = named_list(page, "policy-list") else {
        return;
    };
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    let ticket = match chat.page_begin() {
        Some(ticket) => ticket,
        None => return,
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return;
    };
    let ctx = ctx.clone();
    let page_weak = page.downgrade();
    dispatch(
        &chat,
        move || (client.permissions(), client.grants(50, None, false)),
        move |chat, (permissions, inventory)| {
            if !chat.page_accept(&ticket) {
                return;
            }
            clear_list(&active);
            if let Ok(grants) = permissions {
                for grant in &grants.grants {
                    let row = gtk4::ListBoxRow::new();
                    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                    let label = gtk4::Label::new(Some(&active_grant_label(grant)));
                    label.set_hexpand(true);
                    label.set_xalign(0.0);
                    label.set_wrap(true);
                    line.append(&label);
                    if grant.revocable {
                        let revoke = gtk4::Button::with_label("Revoke");
                        let id = grant.permission_id.clone();
                        let ctx = ctx.clone();
                        let page_weak = page_weak.clone();
                        revoke.connect_clicked(move |_| {
                            let Some(page) = page_weak.upgrade() else {
                                return;
                            };
                            let Some(chat) = ctx.chat.upgrade() else {
                                return;
                            };
                            do_revoke(&ctx, &chat, &page, &id);
                        });
                        line.append(&revoke);
                    }
                    row.set_child(Some(&line));
                    active.append(&row);
                }
            }
            clear_list(&list);
            if let Ok(inventory) = inventory {
                for grant in &inventory.grants {
                    list.append(&gtk4::Label::new(Some(&grant_label(grant))));
                }
            }
        },
    );
}

/// Send exactly the revocation the reader pressed, once. Acceptance
/// removes nothing by itself: the refill re-reads the list, and a
/// grant still in force comes back in plain sight.
fn do_revoke(ctx: &Ctx, chat: &Rc<Chat>, page: &gtk4::Box, permission_id: &str) {
    let ticket = match chat.page_begin() {
        Some(ticket) => ticket,
        None => return,
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return;
    };
    let id = permission_id.to_owned();
    let ctx = ctx.clone();
    let page_strong: gtk4::Box = page.clone();
    dispatch(
        chat,
        move || client.revoke(&id),
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let text = match &answer {
                Ok(answer) => revoke_notice(answer.accepted).to_owned(),
                Err(error) => format!(
                    "Revocation failed: {error}. The grant may still be in force; read the list again instead of pressing twice."
                ),
            };
            if let Some(notice) = find_widget(page_strong.upcast_ref(), "policy-notice")
                .and_then(|widget| widget.downcast::<gtk4::Label>().ok())
            {
                notice.set_text(&text);
                notice.set_visible(true);
            }
            let _ = &ctx;
            fill_policy(&ctx, &page_strong);
        },
    );
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
    fn rail_glyphs_name_bundled_svgs() {
        assert_eq!(rail_glyph("chat"), Some("chat"));
        assert_eq!(rail_glyph("search"), Some("search"));
        assert_eq!(rail_glyph("feed"), Some("library"));
        assert_eq!(rail_glyph("ideas"), Some("ideas"));
        assert_eq!(rail_glyph("goals"), Some("tasks"));
        assert_eq!(rail_glyph("library"), Some("shapes"));
        assert_eq!(rail_glyph("tasks"), None);
        assert_eq!(rail_glyph("policy"), None);
        assert_eq!(rail_glyph("settings"), None);
    }

    #[test]
    fn page_labels_name_state_never_raw_ids() {
        assert_eq!(schedule_label(&schedule("Nightly", true)), "Nightly (on)");
        assert_eq!(schedule_label(&schedule("Nightly", false)), "Nightly (off)");
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
    fn memory_hits_truncate_long_passages_and_name_places() {
        let long = MemoryHit {
            memory_uri: "memory://x".to_owned(),
            text: "y".repeat(300),
            path: Some("/notes".to_owned()),
            ..Default::default()
        };
        let label = memory_hit_label(&long);
        assert!(label.ends_with("\n/notes"));
        assert!(label.chars().count() < 300);
        let short = MemoryHit {
            memory_uri: "memory://x".to_owned(),
            text: "hi".to_owned(),
            ..Default::default()
        };
        assert_eq!(memory_hit_label(&short), "hi");
    }

    #[test]
    fn active_grants_name_coverage_and_revocability() {
        let grant = ActiveGrant {
            permission_id: "p-1".to_owned(),
            kind: Some("files".to_owned()),
            lifetime: None,
            mode: Some("write".to_owned()),
            policy_subject: None,
            expires_at_ms: None,
            revocable: true,
            title: String::new(),
        };
        let label = active_grant_label(&grant);
        assert!(label.contains("p-1") && label.contains("files"));
        assert!(!label.contains("irrevocable"));
        let locked = ActiveGrant {
            revocable: false,
            ..grant
        };
        assert!(active_grant_label(&locked).contains("service-marked irrevocable"));
    }

    #[test]
    fn revoke_outcomes_name_acceptance_or_warn_plainly() {
        assert!(revoke_notice(true).contains("accepted"));
        assert!(revoke_notice(false).contains("still in force"));
    }

    #[test]
    fn grant_rows_count_capabilities_with_right_plural() {
        let one = GrantItem {
            grant_id: "g".to_owned(),
            subject_kind: "connector".to_owned(),
            subject_id: "drive".to_owned(),
            capabilities: vec!["read".to_owned()],
            revision: None,
            not_before_ms: None,
            expires_at_ms: None,
            revoked_at_ms: None,
            provenance: None,
            created_at_ms: None,
        };
        assert_eq!(grant_label(&one), "connector drive (1 capability)");
        let many = GrantItem {
            capabilities: vec!["a".to_owned(), "b".to_owned()],
            ..one
        };
        assert_eq!(grant_label(&many), "connector drive (2 capabilities)");
    }
}
