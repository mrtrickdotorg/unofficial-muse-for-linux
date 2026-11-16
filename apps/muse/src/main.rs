//! Mac-parity Muse client for Linux: GTK4 + Libadwaita shell, same
//! account protocol as the shipped Mac client. Milestone 1: empty
//! navigation skeleton with a cold-start budget (first paint fast,
//! pages built lazily).

use adw::prelude::*;

mod chat;
mod chat_view;
mod session;

const APP_ID: &str = "org.kythos.MuseLinux";
const DEFAULT_WINDOW_WIDTH: i32 = 1100;
const DEFAULT_WINDOW_HEIGHT: i32 = 700;

fn main() {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run();
}

/// Shell only: sidebar navigation plus an empty content stack. Pages
/// are built on first visit, never up front -- eager construction of
/// every surface is what made the old shell slow to first paint.
fn build_ui(app: &adw::Application) {
    // Permanent cold-start probe: build timestamp vs first present, so a
    // startup-cost regression shows up in logs instead of as a complaint.
    let started = std::time::Instant::now();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Muse")
        .default_width(DEFAULT_WINDOW_WIDTH)
        .default_height(DEFAULT_WINDOW_HEIGHT)
        .build();
    let split = adw::OverlaySplitView::builder().show_sidebar(true).build();
    let sidebar = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    for name in ["Chat", "Tasks", "Library", "Feed", "Memory", "Settings"] {
        let row = gtk4::Label::new(Some(name));
        row.set_halign(gtk4::Align::Start);
        sidebar.append(&row);
    }
    split.set_sidebar(Some(&sidebar));
    // Transcript structure is built eagerly (cheap widgets); history
    // rows fill in lazily when the first read lands.
    let transcript = gtk4::ListBox::new();
    transcript.set_selection_mode(gtk4::SelectionMode::None);
    let scrolled = gtk4::ScrolledWindow::builder()
        .child(&transcript)
        .vexpand(true)
        .hexpand(true)
        .build();
    let content = scrolled;
    content.set_halign(gtk4::Align::Center);
    content.set_valign(gtk4::Align::Center);
    content.set_vexpand(true);
    content.set_hexpand(true);
    split.set_content(Some(&content));
    window.set_content(Some(&split));
    window.present();
    eprintln!(
        "muse cold-start: first present {:?} after build",
        started.elapsed()
    );
}
