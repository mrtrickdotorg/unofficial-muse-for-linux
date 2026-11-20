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
mod local_host;
mod login;
mod pages;
mod session;
mod signin;
mod update;
mod voice;
mod webui;
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
    {
        // Escape folds the page drawer away, the way a slide-over
        // closes. The handler sits on the window because that is where
        // every key press lands, whatever page widget holds focus -- and
        // when the drawer is already folded it answers "not mine", so
        // the press goes on to the page.
        let escape = gtk4::EventControllerKey::new();
        escape.connect_key_pressed(|_, key, _, _| {
            if key == gtk4::gdk::Key::Escape
                && FOLD_DRAWER.with(|slot| slot.borrow().as_ref().is_some_and(|fold| fold()))
            {
                return gtk4::glib::Propagation::Stop;
            }
            gtk4::glib::Propagation::Proceed
        });
        window.add_controller(escape);
    }
    window.present();
    // A fresh install pays one-time setup (config folders, helper
    // staging, interpreter probe): a spinner and the reader's sentence
    // cover it, once, instead of a silent stall. Returning installs
    // skip this entirely.
    if session::first_run_marker().is_none_or(|marker| !marker.is_file()) {
        show_first_run(&window);
    }
    eprintln!(
        "muse cold-start: first present {:?} after build",
        started.elapsed()
    );
}

/// The first-run setup dialog: a spinner and the reader's sentence
/// while the one-time work runs on a worker thread. The main loop
/// stays free so the spinner animates; the marker lands only after
/// setup returns, so an interrupted setup re-runs instead of
/// pretending it finished.
fn show_first_run(parent: &gtk4::ApplicationWindow) {
    let dialog = gtk4::Window::builder()
        .title("Muse")
        .modal(true)
        .resizable(false)
        .default_width(360)
        .build();
    dialog.set_transient_for(Some(parent));
    let body = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    body.set_margin_start(20);
    body.set_margin_end(20);
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    let spinner = gtk4::Spinner::new();
    spinner.set_valign(gtk4::Align::Start);
    spinner.start();
    body.append(&spinner);
    let words = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    words.set_valign(gtk4::Align::Center);
    words.set_hexpand(true);
    let head = gtk4::Label::new(Some("Performing first-run setup"));
    head.set_xalign(0.0);
    head.add_css_class("title-4");
    words.append(&head);
    let sub = gtk4::Label::new(Some("This may take a few moments."));
    sub.set_xalign(0.0);
    sub.add_css_class("dim-label");
    words.append(&sub);
    body.append(&words);
    dialog.set_child(Some(&body));
    dialog.present();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        session::first_run_setup();
        session::note_first_run_done();
        let _ = done_tx.send(());
    });
    let dialog_weak = dialog.downgrade();
    gtk4::glib::idle_add_local(move || {
        use std::sync::mpsc::TryRecvError;
        match done_rx.try_recv() {
            Ok(()) => {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.close();
                }
                gtk4::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => gtk4::glib::ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => gtk4::glib::ControlFlow::Break,
        }
    });
}

/// The rail's own width (the web's 72px icon column) and the page
/// drawer's (232px). The shell asks the split view for exactly these,
/// so the rail is the only width that is always there.
const RAIL_WIDTH: i32 = 72;
const DRAWER_WIDTH: i32 = 232;

/// The narrowest the chat column may get before the details panel steps
/// aside for the open page drawer: enough for its header row and a
/// transcript that still reads as a column.
const CHAT_MIN_WIDTH: i32 = 560;

thread_local! {
    /// The page drawer's Escape action, installed by the shell that owns
    /// the drawer and run by the window: the window sees every key press
    /// wherever focus sits on the page, the split view does not. The
    /// action folds the drawer and answers whether it did.
    static FOLD_DRAWER: std::cell::RefCell<Option<std::rc::Rc<dyn Fn() -> bool>>> =
        std::cell::RefCell::new(None);
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
    side.set_size_request(DRAWER_WIDTH, -1);
    side.set_margin_start(6);
    side.set_margin_end(10);
    side.set_margin_top(12);
    side.set_margin_bottom(12);
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
    // The page list and History share one scroller: on a short window
    // they scroll under the tools row while the profile row and build
    // stamp stay pinned to the drawer's foot, the way muse.ai keeps its
    // account row in place as the lists move under it.
    let lists = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    lists.append(&sidebar);
    let section = gtk4::Label::new(Some("History"));
    section.set_halign(gtk4::Align::Start);
    section.add_css_class("muse-section");
    lists.append(&section);
    let history = gtk4::ListBox::new();
    lists.append(&history);
    let lists_scroll = gtk4::ScrolledWindow::new();
    lists_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    lists_scroll.set_overlay_scrolling(true);
    lists_scroll.set_vexpand(true);
    lists_scroll.set_child(Some(&lists));
    side.append(&lists_scroll);
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
        image.set_pixel_size(18);
        let button = gtk4::Button::new();
        button.set_child(Some(&image));
        button.add_css_class("muse-rail-button");
        button.set_tooltip_text(Some(tooltip));
        button
    }
    let rail = gtk4::Box::new(gtk4::Orientation::Vertical, 28);
    rail.add_css_class("muse-rail");
    // The group's pitch is fixed and web-like; expanding spacers
    // above and below it split the slack, so the icons sit centered
    // in the column instead of huddling at the top.
    let top_spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    top_spacer.set_vexpand(true);
    rail.append(&top_spacer);
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
    // The web's pitch is fixed (~75px): one expanding spacer below
    // the group collects the slack, so the icons keep their spacing
    // on any window height instead of stretching apart.
    let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    rail.append(&spacer);
    let download_button = rail_button("download", "Updates");
    rail.append(&download_button);
    rail.append(&menu_button);
    let shell = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    shell.add_css_class("muse-shell");
    // The split view hands the sidebar a width of its own choosing; the
    // shell asks for exactly the rail, so the chat column starts right
    // after the icons whether the drawer is open or folded away.
    shell.set_size_request(RAIL_WIDTH, -1);
    shell.append(&rail);
    // The page list is a drawer, not a column: as on muse.ai the rail is
    // the only permanent navigation, and an icon press slides the list
    // and its History out beside it. A second press on the open page's
    // own icon folds it away again.
    // Without this the split view hands its sidebar a quarter of the
    // window (sidebar-width-fraction's default), which parks a strip
    // of empty sidebar between the icons and the chat column. The
    // shell's own request is the width instead: the rail alone when
    // the drawer is folded, rail plus drawer while it shows.
    split.set_sidebar_width_fraction(0.0);
    split.set_min_sidebar_width(0.0);
    split.set_max_sidebar_width(f64::from(RAIL_WIDTH + DRAWER_WIDTH));
    let drawer = gtk4::Revealer::new();
    drawer.set_transition_type(gtk4::RevealerTransitionType::SlideRight);
    drawer.set_transition_duration(180);
    drawer.set_reveal_child(false);
    drawer.set_child(Some(&side));
    shell.append(&drawer);
    split.set_sidebar(Some(&shell));
    {
        // The drawer's width request falls back to the rail once the
        // fold animation has actually finished, so the slide is never
        // squeezed and the chat column still gets the room.
        let shell = shell.clone();
        drawer.connect_notify_local(Some("child-revealed"), move |drawer, _| {
            if !drawer.is_child_revealed() {
                shell.set_size_request(RAIL_WIDTH, -1);
            }
        });
    }
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
    // Whether the open drawer folded the details panel away: only a fold
    // this code made is undone when the drawer goes back, so a reader
    // who wants the panel gone keeps it gone.
    let panel_folded = Rc::new(std::cell::Cell::new(false));
    // The rail button the open list belongs to. Folding hands it the
    // keyboard back, so a reader who opened the list from the rail is
    // never dropped on a pane that just disappeared.
    let drawer_opener: Rc<std::cell::RefCell<Option<gtk4::Button>>> =
        Rc::new(std::cell::RefCell::new(None));
    // The drawer pushes the page, so the chat column gives up the
    // drawer's width. When that would leave the column too narrow for
    // its header and transcript, the details panel steps aside for as
    // long as the drawer stays out instead: nothing is covered and the
    // window is never asked to grow.
    let open_drawer = {
        let chat = ctx.chat.clone();
        let stack = stack.clone();
        let drawer = drawer.clone();
        let shell = shell.clone();
        let panel_folded = panel_folded.clone();
        let opener = drawer_opener.clone();
        Rc::new(move |button: &gtk4::Button| {
            *opener.borrow_mut() = Some(button.clone());
            shell.set_size_request(RAIL_WIDTH + DRAWER_WIDTH, -1);
            drawer.set_reveal_child(true);
            if let Some(chat) = chat.upgrade()
                && chat.panel_shown()
                && stack.allocated_width() - DRAWER_WIDTH - app::PANEL_WIDTH < CHAT_MIN_WIDTH
            {
                chat.set_panel_shown(false);
                panel_folded.set(true);
            }
        })
    };
    let fold_drawer = {
        let chat = ctx.chat.clone();
        let drawer = drawer.clone();
        let panel_folded = panel_folded.clone();
        let opener = drawer_opener.clone();
        Rc::new(move || {
            drawer.set_reveal_child(false);
            if panel_folded.replace(false)
                && let Some(chat) = chat.upgrade()
            {
                chat.set_panel_shown(true);
            }
            if let Some(button) = opener.borrow().as_ref() {
                button.grab_focus();
            }
        })
    };
    for (page, button) in &ctx.rail {
        let ctx = ctx.clone();
        let page = page.clone();
        let drawer = drawer.clone();
        let stack = stack.clone();
        let open_drawer = open_drawer.clone();
        let fold_drawer = fold_drawer.clone();
        let rail_button = button.clone();
        button.connect_clicked(move |_| {
            let current = stack.visible_child_name();
            if pages::rail_closes(current.as_deref(), &page, drawer.reveals_child()) {
                fold_drawer();
                return;
            }
            open_drawer(&rail_button);
            pages::show(&ctx, &page);
        });
    }
    // The window's Escape handler runs this: it folds the page list away
    // and says it did, or leaves the key alone when the drawer is already
    // folded, so nothing here swallows an Escape meant for the page.
    {
        let drawer = drawer.clone();
        let fold_drawer = fold_drawer.clone();
        FOLD_DRAWER.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(move || {
                if !drawer.reveals_child() {
                    return false;
                }
                fold_drawer();
                true
            }) as Rc<dyn Fn() -> bool>);
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
    // No page drawer on this surface: an Escape belongs to the sign-in
    // form, not to a drawer the main shell left behind.
    FOLD_DRAWER.with(|slot| *slot.borrow_mut() = None);
    let form = signin::SignIn::build({
        let split = split.clone();
        move |_| show_main(&split)
    });
    split.set_content(Some(form.widget()));
}
