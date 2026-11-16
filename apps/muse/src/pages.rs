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
    ActiveGrant, ArtifactSummary, GrantItem, ListingItem, MemoryHit, Schedule, TaskRun,
};

/// What a page needs: the chat for reads, the stack to navigate, and
/// a way back to the sign-in screen when the link goes away.
#[derive(Clone)]
pub struct Ctx {
    pub chat: Weak<Chat>,
    pub stack: gtk4::Stack,
    pub sidebar: gtk4::ListBox,
    pub show_signin: Rc<dyn Fn()>,
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
    for (index, page) in PAGES.iter().enumerate() {
        if *page == name
            && let Some(row) = ctx.sidebar.row_at_index(index as i32)
        {
            ctx.sidebar.select_row(Some(&row));
        }
    }
}

fn build(ctx: &Ctx, name: &str) -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    page.set_margin_start(16);
    page.set_margin_end(16);
    page.set_margin_top(12);
    match name {
        "threads" => {
            let list = gtk4::ListBox::new();
            list.set_widget_name("threads-list");
            page.append(&list);
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
            page.append(&schedules);
            let runs = gtk4::ListBox::new();
            runs.set_widget_name("tasks-runs");
            page.append(&runs);
        }
        "feed" => {
            let prompt = gtk4::Label::new(None);
            prompt.set_widget_name("feed-prompt");
            prompt.set_wrap(true);
            prompt.set_xalign(0.0);
            page.append(&prompt);
            let list = gtk4::ListBox::new();
            list.set_widget_name("feed-list");
            page.append(&list);
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
            page.append(&search);
            let list = gtk4::ListBox::new();
            list.set_widget_name("memory-list");
            page.append(&list);
            let weak = ctx.chat.clone();
            let page_weak = page.downgrade();
            go.connect_clicked(move |_| {
                let Some(page) = page_weak.upgrade() else {
                    return;
                };
                let Some(chat) = weak.upgrade() else { return };
                fill_memory(&chat, &page);
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
            page.append(&query);
            let list = gtk4::ListBox::new();
            list.set_widget_name("search-list");
            page.append(&list);
            let weak = ctx.chat.clone();
            let ctx_search = ctx.clone();
            let page_weak = page.downgrade();
            go.connect_clicked(move |_| {
                let Some(page) = page_weak.upgrade() else {
                    return;
                };
                let Some(chat) = weak.upgrade() else { return };
                do_search(&ctx_search, &chat, &page);
            });
        }
        "ideas" | "goals" => {
            let list = gtk4::ListBox::new();
            list.set_widget_name(&format!("{name}-list"));
            page.append(&list);
        }
        "policy" => {
            let notice = gtk4::Label::new(None);
            notice.set_widget_name("policy-notice");
            notice.set_wrap(true);
            notice.set_xalign(0.0);
            notice.set_visible(false);
            page.append(&notice);
            let active = gtk4::ListBox::new();
            active.set_widget_name("policy-active");
            page.append(&active);
            let list = gtk4::ListBox::new();
            list.set_widget_name("policy-list");
            page.append(&list);
        }
        "library" => {
            let list = gtk4::ListBox::new();
            list.set_widget_name("library-list");
            page.append(&list);
        }
        "settings" => {
            let info = gtk4::Label::new(None);
            info.set_widget_name("settings-info");
            info.set_wrap(true);
            info.set_xalign(0.0);
            page.append(&info);
            let sign_out = gtk4::Button::with_label("Sign out");
            page.append(&sign_out);
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
            page.append(&label);
        }
    }
    page
}

fn fill(ctx: &Ctx, name: &str) {
    let Some(page) = ctx.stack.child_by_name(name) else {
        return;
    };
    let page: gtk4::Box = page.downcast().expect("page box");
    match name {
        "threads" => fill_threads(ctx, &page),
        "tasks" => fill_tasks(ctx, &page),
        "library" => fill_library(ctx, &page),
        "search" => fill_search_hint(&page),
        "ideas" => fill_ideas(ctx, &page),
        "goals" => fill_goals(ctx, &page),
        "feed" => fill_feed(ctx, &page),
        "memory" => fill_memory_opt(ctx, &page),
        "policy" => fill_policy(ctx, &page),
        "settings" => fill_settings(&page),
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

/// One-line run label: status plus the human summary when one exists.
pub fn run_label(run: &TaskRun) -> String {
    match run.result_summary.as_deref() {
        Some(summary) if !summary.is_empty() => {
            format!("{}: {}", run.status, summary)
        }
        _ => run.status.clone(),
    }
}

/// One-line artifact label: title plus kind when the service names one.
pub fn artifact_label(artifact: &ArtifactSummary) -> String {
    match artifact.kind.as_deref() {
        Some(kind) if !kind.is_empty() => {
            format!("{} [{}]", artifact.title, kind)
        }
        _ => artifact.title.clone(),
    }
}

fn named_list(page: &gtk4::Box, name: &str) -> Option<gtk4::ListBox> {
    let mut child = page.first_child();
    while let Some(widget) = child {
        if widget.widget_name() == name {
            return widget.downcast::<gtk4::ListBox>().ok();
        }
        child = widget.next_sibling();
    }
    None
}

fn clear_list(list: &gtk4::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
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
fn fill_library(ctx: &Ctx, page: &gtk4::Box) {
    let Some(list) = named_list(page, "library-list") else {
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
    dispatch(
        &chat,
        move || client.artifacts(),
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let Ok(listing) = answer else { return };
            clear_list(&list);
            for artifact in &listing.artifacts {
                list.append(&gtk4::Label::new(Some(&artifact_label(artifact))));
            }
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

fn fill_settings(page: &gtk4::Box) {
    let Some(info) = ({
        let mut child = page.first_child();
        let mut found = None;
        while let Some(widget) = child {
            if widget.widget_name() == "settings-info" {
                found = widget.downcast::<gtk4::Label>().ok();
                break;
            }
            child = widget.next_sibling();
        }
        found
    }) else {
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
    if meta.is_empty() {
        item.title.clone()
    } else {
        format!("{}\n{}", item.title, meta.join(" \u{00b7} "))
    }
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
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return;
    };
    dispatch(
        &chat,
        move || client.feed(),
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let Ok(listing) = answer else { return };
            clear_list(&list);
            for item in &listing.items {
                let card = gtk4::Label::new(Some(&feed_item_label(item)));
                card.set_wrap(true);
                card.set_xalign(0.0);
                list.append(&card);
            }
        },
    );
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
        let mut found = String::new();
        let mut child = page.first_child();
        while let Some(widget) = child {
            if widget.widget_name() == "memory-query" {
                if let Ok(entry) = widget.downcast::<gtk4::Entry>() {
                    found = entry.text().to_string();
                }
                break;
            }
            child = widget.next_sibling();
        }
        found
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
fn fill_service_listing<F>(chat: &Rc<Chat>, list: &gtk4::ListBox, fetch: F)
where
    F: FnOnce(
            &muse_account::AccountClient,
        ) -> Result<muse_account::ServiceListing, muse_account::AccountError>
        + Send
        + 'static,
{
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
        chat,
        move || fetch(&client),
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let Ok(listing) = answer else { return };
            clear_list(&list);
            for item in &listing.items {
                let card = gtk4::Label::new(Some(&feed_item_label(item)));
                card.set_wrap(true);
                card.set_xalign(0.0);
                list.append(&card);
            }
        },
    );
}

fn fill_ideas(ctx: &Ctx, page: &gtk4::Box) {
    let Some(list) = named_list(page, "ideas-list") else {
        return;
    };
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    fill_service_listing(&chat, &list, |client| client.ideas());
}

fn fill_goals(ctx: &Ctx, page: &gtk4::Box) {
    let Some(list) = named_list(page, "goals-list") else {
        return;
    };
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    fill_service_listing(&chat, &list, |client| client.goals());
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
        let mut found = String::new();
        let mut child = page.first_child();
        while let Some(widget) = child {
            if widget.widget_name() == "search-query" {
                if let Ok(entry) = widget.downcast::<gtk4::Entry>() {
                    found = entry.text().to_string();
                }
                break;
            }
            child = widget.next_sibling();
        }
        found
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
        move || client.search(&query),
        move |chat, answer| {
            if !chat.page_accept(&ticket) {
                return;
            }
            let Ok(found) = answer else { return };
            clear_list(&list);
            if found.items.is_empty() {
                list.append(&gtk4::Label::new(Some("No matches.")));
                return;
            }
            for item in &found.items {
                let id = item.id.clone().unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                let open = gtk4::Button::new();
                let label = feed_item_label(item);
                open.set_label(&label);
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
            let mut child = page_strong.first_child();
            while let Some(widget) = child {
                if widget.widget_name() == "policy-notice" {
                    if let Ok(notice) = widget.downcast::<gtk4::Label>() {
                        notice.set_text(&text);
                        notice.set_visible(true);
                    }
                    break;
                }
                child = widget.next_sibling();
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

    #[test]
    fn artifact_labels_show_kind_only_when_named() {
        let bare = ArtifactSummary {
            artifact_ref: "a".to_owned(),
            title: "Notes".to_owned(),
            kind: None,
            format: None,
            status: None,
            placement: None,
            path: None,
            target_kind: None,
            slug: None,
            updated_at_ms: None,
            pinned: false,
            can_open: false,
            can_share: false,
        };
        assert_eq!(artifact_label(&bare), "Notes");
        let kinded = ArtifactSummary {
            kind: Some("doc".to_owned()),
            ..bare
        };
        assert_eq!(artifact_label(&kinded), "Notes [doc]");
    }
}
