//! Mac-parity Muse client for Linux: GTK4 + Libadwaita shell, same
//! account protocol as the shipped Mac client. Milestone 1: empty
//! navigation skeleton with a cold-start budget (first paint fast,
//! pages built lazily).

use adw::prelude::*;

mod app;
mod chat;
mod chat_view;
mod composer;
mod session;
mod signin;

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
    let linked = session::load_saved_link().filter(|link| link.consent);
    if linked.is_some() {
        eprintln!("muse: saved link found, opening chat");
        let page = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let chat = app::Chat::install(&page);
        // App-lifetime singleton: the signal handlers and idle sources
        // hold only weak references, so the strong reference lives here,
        // leaked for the life of the process.
        std::mem::forget(chat);
        split.set_content(Some(&page));
    } else {
        eprintln!("muse: no saved link, showing sign-in");
        let form = signin::SignIn::build({
            let split = split.clone();
            move |_| {
                let page = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
                let chat = app::Chat::install(&page);
                std::mem::forget(chat);
                split.set_content(Some(&page));
            }
        });
        split.set_content(Some(form.widget()));
    }
    window.set_content(Some(&split));
    window.present();
    eprintln!(
        "muse cold-start: first present {:?} after build",
        started.elapsed()
    );
}
