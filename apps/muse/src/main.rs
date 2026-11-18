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
pub(crate) mod widget;

const APP_ID: &str = "org.kythos.MuseLinux";
/// Register the bundled icon set. Called once at startup; tests call
/// it too, since resources register per process, not per call site.
pub(crate) fn register_icons() {
    let data =
        gtk4::glib::Bytes::from_static(include_bytes!(concat!(env!("OUT_DIR"), "/muse.gresource")));
    let resource = gtk4::gio::Resource::from_data(&data).expect("bundled icon resource loads");
    gtk4::gio::resources_register(&resource);
}
/// Where the bundled libwebkit looks for its helper processes (see
/// scripts/patch-webkit-path.py): the app points this path at the
/// bundle's helpers on every launch. A WebView with no helpers
/// aborts the process, so frames check it before constructing one.
pub(crate) const WEBKIT_STAGING: &str = "/tmp/.webkitgtk-6.0-umfl";
const DEFAULT_WINDOW_WIDTH: i32 = 1100;
const DEFAULT_WINDOW_HEIGHT: i32 = 700;

fn main() {
    // The AppImage's linuxdeploy Gtk hook exports GTK_THEME=Adwaita:
    // <variant>, a guess at the desktop's scheme. This app follows
    // the system theme -- on Patrick's seat that is Breeze, chosen by
    // KDE in the Gtk settings -- and a forced theme name would
    // replace the desktop's choice with the hook's guess, so it is
    // cleared before Gtk can read it. The settings, the portal, and
    // libadwaita's scheme following then decide the look, like any
    // other app on the seat.
    // The build line ends stale-binary confusion: every diagnosis
    // starts from the exact commit the log came from.
    if let Some(sha) = update::local_sha() {
        eprintln!("muse: build {sha}");
    }
    // SAFETY: main() is single-threaded here and nothing has spawned
    // a thread or touched the environment yet.
    unsafe { std::env::remove_var("GTK_THEME") };
    // WebKit sandboxes its helpers with bubblewrap, whose mount
    // namespace cannot see the FUSE mount this AppImage runs from: the
    // staged helpers then die with "cannot open shared object file"
    // even though the bundle is complete (proven against the release
    // artifact: files, RUNPATH, and $ORIGIN resolution all verified,
    // and a scrubbed unsandboxed probe of the same binaries passes).
    // Outside the bundle the system manages its own helpers and the
    // sandbox stays on.
    // SAFETY: same single-threaded window as above.
    if std::env::var_os("APPIMAGE").is_some() {
        unsafe { std::env::remove_var("WEBKIT_FORCE_SANDBOX") };
        unsafe { std::env::set_var("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS", "1") };
        eprintln!("muse: WebKit sandbox disabled inside the bundle (FUSE mount is sandbox-blind)");
    }
    // The bundled libwebkit finds its helpers only through the
    // staging symlink (see WEBKIT_STAGING): without it the first
    // WebView aborts the process. Staged before Gtk starts, and a
    // no-op everywhere except inside the bundle.
    stage_webkit_helpers();
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

/// Point WEBKIT_STAGING at the bundle's helpers. Inside the AppImage
/// the helpers ride beside the binary (`usr/lib/webkitgtk-6.0` next
/// to `usr/bin/muse`); outside it there is nothing to stage and this
/// is a silent no-op. Failures only log: a missing stage means widget
/// frames fall back to text instead of aborting the process.
fn stage_webkit_helpers() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("muse: cannot locate its own binary: {error}");
            return;
        }
    };
    let helpers = match exe.parent().and_then(|bin| bin.parent()) {
        Some(prefix) => prefix.join("lib/webkitgtk-6.0"),
        None => return,
    };
    if !helpers.join("WebKitNetworkProcess").is_file() {
        return;
    }
    let stage = std::path::Path::new(WEBKIT_STAGING);
    if std::fs::read_link(stage).ok().as_deref() == Some(helpers.as_path()) {
        return;
    }
    let _ = std::fs::remove_file(stage);
    // A stale directory (an older broken stage) must go before the
    // link lands; its absence is the common case, not an error.
    if stage.is_dir() && std::fs::remove_dir_all(stage).is_err() {
        eprintln!("muse: cannot clear {}", stage.display());
        return;
    }
    if std::os::unix::fs::symlink(&helpers, stage).is_err() {
        eprintln!("muse: cannot stage WebKit helpers at {}", stage.display());
    } else {
        eprintln!("muse: staged WebKit helpers from {}", helpers.display());
    }
}

/// Shell only: sidebar navigation plus a content stack. Pages are
/// built on first visit, never up front -- eager construction of
/// every surface is what made the old shell slow to first paint.
fn build_ui(app: &adw::Application) {
    // The look follows the system theme and color scheme; the
    // stylesheet below carries only shape and the brand accents.
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
    // The running build, pinned to the sidebar footer: no more
    // guessing which artifact a screenshot came from.
    let build = gtk4::Label::new(Some(&match update::local_sha() {
        Some(sha) => format!("build {}", sha.get(..7).unwrap_or(sha)),
        None => "dev build".to_owned(),
    }));
    build.set_halign(gtk4::Align::Start);
    build.add_css_class("muse-build-stamp");
    side.append(&build);
    // The icon set rides inside the binary (see build.rs): the rail
    // paints web-shaped glyphs with no files beside the executable.
    register_icons();
    // The web's far-left icon rail: menu pill, one glyph per section,
    // new chat, download. The panel beside it keeps the full sidebar;
    // the rail is a shortcut, never a second source of truth. Glyphs
    // land where the web sends them: book opens Feed, check opens
    // Goals, shapes open Library.
    fn rail_button(icon: &str, tooltip: &str) -> gtk4::Button {
        let image = gtk4::Image::from_resource(&format!("/org/kythos/MuseLinux/icons/{icon}.svg"));
        image.set_pixel_size(26);
        let button = gtk4::Button::new();
        button.set_child(Some(&image));
        button.add_css_class("muse-rail-button");
        button.set_tooltip_text(Some(tooltip));
        button
    }
    let rail = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    rail.add_css_class("muse-rail");
    let mut rail_buttons: Vec<(String, gtk4::Button)> = Vec::new();
    let menu_button = rail_button("menu", "Settings");
    let entries: &[&str] = &["chat", "search", "feed", "ideas", "goals", "library"];
    for page in entries {
        let glyph = pages::rail_glyph(page).unwrap_or("chat");
        let button = rail_button(glyph, pages::title(page));
        rail.append(&button);
        rail_buttons.push(((*page).to_owned(), button));
    }
    // The web separates creation from navigation: new space rides
    // below a divider, downloads and settings hug the bottom.
    let divider = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    rail.append(&divider);
    let new_button = rail_button("new", "New space");
    rail.append(&new_button);
    let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    rail.append(&spacer);
    let download_button = rail_button("download", "Updates");
    rail.append(&download_button);
    rail.append(&menu_button);
    let shell = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    shell.append(&rail);
    shell.append(&side);
    split.set_sidebar(Some(&shell));
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
        rail: rail_buttons,
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
    // The rail shortcuts: plus starts a fresh thread; download opens
    // this copy's update panel; the hamburger opens settings; an icon
    // shows its page.
    {
        let ctx = ctx.clone();
        menu_button.connect_clicked(move |_| {
            pages::show(&ctx, "settings");
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
        download_button.connect_clicked(move |_| {
            pages::show(&ctx, "settings");
        });
    }
    for (page, button) in &ctx.rail {
        let ctx = ctx.clone();
        let page = page.clone();
        button.connect_clicked(move |_| {
            pages::show(&ctx, &page);
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
