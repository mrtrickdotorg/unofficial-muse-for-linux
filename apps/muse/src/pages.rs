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

use super::app::Chat;
use super::session::account_dir;

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
    "chat", "threads", "tasks", "library", "feed", "memory", "policy", "settings",
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
        "settings" => fill_settings(&page),
        _ => {}
    }
}

fn first_list(page: &gtk4::Box, name: &str) -> Option<gtk4::ListBox> {
    let mut child = page.first_child();
    while let Some(widget) = child {
        if widget.widget_name() == name {
            return widget.downcast::<gtk4::ListBox>().ok();
        }
        child = widget.next_sibling();
    }
    None
}

fn fill_threads(ctx: &Ctx, page: &gtk4::Box) {
    let Some(list) = first_list(page, "threads-list") else {
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
