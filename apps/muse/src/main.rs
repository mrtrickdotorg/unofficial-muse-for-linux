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
mod images;
mod login;
mod pages;
mod session;
mod signin;
mod update;
mod voice;
mod widget;

const APP_ID: &str = "org.kythos.MuseLinux";
const DEFAULT_WINDOW_WIDTH: i32 = 1100;
const DEFAULT_WINDOW_HEIGHT: i32 = 700;

fn main() {
    // A staged update takes over as its own instance: the old copy is
    // still holding the app name when the new one starts, and a unique
    // instance would forward its launch to the old one and exit --
    // the old one then exits, leaving nothing. Ordinary launches stay
    // single-instance.
    let mut builder = adw::Application::builder().application_id(APP_ID);
    if update::updating_relaunch() {
        builder = builder.flags(gtk4::gio::ApplicationFlags::NON_UNIQUE);
    }
    let app = builder.build();
    app.connect_activate(build_ui);
    app.run();
}

/// Shell only: sidebar navigation plus a content stack. Pages are
/// built on first visit, never up front -- eager construction of
/// every surface is what made the old shell slow to first paint.
fn build_ui(app: &adw::Application) {
    // The client follows the system theme: the forced-light
    // experiment lost to a dark GNOME box that paints dark whatever
    // the manager claims, so the stylesheet carries the meta.ai dark
    // surfaces as fixed values instead.
    // Permanent cold-start probe: build timestamp vs first present, so
    // a startup-cost regression shows up in logs, not as a complaint.
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
    // Mac-signature surfaces (floating composer card, grey bubbles,
    // blue send circle): fixed brand values, loaded once per display.
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(include_str!("theme.css"));
    gtk4::style_context_add_provider_for_display(
        &gtk4::prelude::RootExt::display(&window),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
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
    // The Mac idiom: icon tools on top, the page list, the History
    // section with the thread listing, and the profile row at the
    // bottom. The page list keeps its index order, so the select/show
    // recursion guard keeps working untouched.
    let side = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    side.add_css_class("muse-side");
    side.set_margin_start(8);
    side.set_margin_end(8);
    side.set_margin_top(8);
    side.set_margin_bottom(8);
    let tools = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let search_button = gtk4::Button::with_label("🔍");
    search_button.add_css_class("flat");
    search_button.set_tooltip_text(Some("Search"));
    let new_button = gtk4::Button::with_label("✎");
    new_button.add_css_class("flat");
    new_button.set_tooltip_text(Some("New chat"));
    tools.append(&search_button);
    tools.append(&new_button);
    side.append(&tools);
    let sidebar = gtk4::ListBox::new();
    for name in pages::PAGES {
        let row = gtk4::ListBoxRow::new();
        let label = gtk4::Label::new(Some(pages::title(name)));
        label.set_halign(gtk4::Align::Start);
        row.set_child(Some(&label));
        sidebar.append(&row);
    }
    side.append(&sidebar);
    let section = gtk4::Label::new(Some("History"));
    section.set_halign(gtk4::Align::Start);
    section.add_css_class("muse-section");
    side.append(&section);
    let history = gtk4::ListBox::new();
    history.set_vexpand(true);
    side.append(&history);
    let profile = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let avatar = gtk4::Label::new(Some("✦"));
    avatar.add_css_class("muse-avatar");
    let profile_name = gtk4::Label::new(Some("Sign in"));
    profile_name.set_hexpand(true);
    profile_name.set_halign(gtk4::Align::Start);
    profile_name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    let gear = gtk4::Button::with_label("⚙");
    gear.add_css_class("flat");
    gear.set_tooltip_text(Some("Settings"));
    profile.append(&avatar);
    profile.append(&profile_name);
    profile.append(&gear);
    side.append(&profile);
    split.set_sidebar(Some(&side));
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
        history: history.clone(),
        profile_name: profile_name.clone(),
        updates: std::rc::Rc::new(std::cell::RefCell::new(update::UpdateCheck::default())),
        show_signin: {
            let split = split.clone();
            Rc::new(move || show_signin(&split))
        },
    };
    let ctx = Rc::new(ctx);
    let nav_ctx = ctx.clone();
    sidebar.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        let index = row.index();
        if let Some(name) = pages::PAGES.get(index as usize) {
            pages::show(&nav_ctx, name);
        }
    });
    // The History section opens threads the same way the Threads page
    // does: the row name carries the id, a `None` row would mean new.
    {
        let ctx = ctx.clone();
        history.connect_row_selected(move |list, row| {
            let Some(row) = row else { return };
            // The refill's own mark echoes through this same signal;
            // only a reader press opens anything.
            let marking = pages::MARKING_HISTORY.with(|marking| marking.get());
            if marking {
                return;
            }
            let name = row.widget_name();
            let Some(id) = name.as_str().strip_prefix("thread:") else {
                return;
            };
            let id = id.to_owned();
            list.select_row(None::<&gtk4::ListBoxRow>);
            pages::open_thread(&ctx, Some(id));
        });
    }
    {
        let ctx = ctx.clone();
        new_button.connect_clicked(move |_| {
            pages::open_thread(&ctx, None);
        });
    }
    {
        let ctx = ctx.clone();
        search_button.connect_clicked(move |_| {
            pages::show(&ctx, "search");
        });
    }
    {
        let ctx = ctx.clone();
        gear.connect_clicked(move |_| {
            pages::show(&ctx, "settings");
        });
    }
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
