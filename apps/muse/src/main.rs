//! Mac-parity Muse client for Linux: GTK4 + Libadwaita shell, same
//! account protocol as the shipped Mac client. Milestone 1: empty
//! navigation skeleton with a cold-start budget (first paint fast,
//! pages built lazily).

use std::rc::Rc;

use adw::prelude::*;

mod app;
mod chat;
mod chat_view;
mod composer;
mod login;
mod pages;
mod session;
mod signin;
mod update;

const APP_ID: &str = "org.kythos.MuseLinux";
const DEFAULT_WINDOW_WIDTH: i32 = 1100;
const DEFAULT_WINDOW_HEIGHT: i32 = 700;

fn main() {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run();
}

/// Shell only: sidebar navigation plus a content stack. Pages are
/// built on first visit, never up front -- eager construction of
/// every surface is what made the old shell slow to first paint.
fn build_ui(app: &adw::Application) {
    // Permanent cold-start probe: build timestamp vs first present, so a
    // startup-cost regression shows up in logs instead of as a complaint.
    let started = std::time::Instant::now();
    // Plain ApplicationWindow: AdwApplicationWindow refuses a manually
    // set titlebar, and without one the window has no drag handle or
    // window controls.
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title("Muse")
        .default_width(DEFAULT_WINDOW_WIDTH)
        .default_height(DEFAULT_WINDOW_HEIGHT)
        .build();
    // Client-side decorations: without a titlebar the window has no
    // drag handle, no resize grip, and no min/max buttons -- it sits
    // stuck wherever the compositor puts it.
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk4::Label::new(Some("Muse"))));
    window.set_titlebar(Some(&header));
    let split = adw::OverlaySplitView::builder().show_sidebar(true).build();
    let linked = session::load_saved_link().filter(|link| link.consent);
    if linked.is_some() {
        eprintln!("muse: saved link found, opening chat");
        show_main(&split);
    } else {
        eprintln!("muse: no saved link, showing sign-in");
        show_signin(&split);
    }
    window.set_child(Some(&split));
    window.present();
    eprintln!(
        "muse cold-start: first present {:?} after build",
        started.elapsed()
    );
}

/// The main surface: sidebar plus a lazily-filled page stack. Only the
/// chat page builds up front (cheap widgets); every other page builds
/// on first visit and refills on every visit.
fn show_main(split: &adw::OverlaySplitView) {
    split.set_show_sidebar(true);
    let sidebar = gtk4::ListBox::new();
    for name in pages::PAGES {
        let row = gtk4::ListBoxRow::new();
        let label = gtk4::Label::new(Some(pages::title(name)));
        label.set_halign(gtk4::Align::Start);
        row.set_child(Some(&label));
        sidebar.append(&row);
    }
    split.set_sidebar(Some(&sidebar));
    let stack = gtk4::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    let chat_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    stack.add_named(&chat_box, Some("chat"));
    let chat = app::Chat::install(&chat_box);
    // App-lifetime singletons: the signal handlers and idle sources
    // hold only weak references, so the strong references live here,
    // leaked for the life of the process.
    let weak_chat = Rc::downgrade(&chat);
    std::mem::forget(chat);
    split.set_content(Some(&stack));
    let ctx = pages::Ctx {
        chat: weak_chat,
        stack: stack.clone(),
        sidebar: sidebar.clone(),
        updates: std::rc::Rc::new(std::cell::RefCell::new(update::UpdateCheck::default())),
        show_signin: {
            let split = split.clone();
            Rc::new(move || show_signin(&split))
        },
    };
    let ctx = Rc::new(ctx);
    sidebar.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        let index = row.index();
        if let Some(name) = pages::PAGES.get(index as usize) {
            pages::show(&ctx, name);
        }
    });
    stack.set_visible_child_name("chat");
    if let Some(first) = sidebar.row_at_index(0) {
        sidebar.select_row(Some(&first));
    }
}

/// The sign-in screen, shown with no usable saved link and after an
/// explicit sign-out. A linked form swaps back to the main surface.
fn show_signin(split: &adw::OverlaySplitView) {
    split.set_show_sidebar(false);
    let form = signin::SignIn::build({
        let split = split.clone();
        move |_| show_main(&split)
    });
    split.set_content(Some(form.widget()));
}
