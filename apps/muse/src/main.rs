//! Mac-parity Muse client for Linux: GTK4 + Libadwaita shell, same
//! account protocol as the shipped Mac client. Milestone 1: empty
//! navigation skeleton with a cold-start budget (first paint fast,
//! pages built lazily).

use std::rc::Rc;

use adw::prelude::*;

mod app;
mod archived;
mod chat;
mod chat_view;
mod composer;
mod images;
mod local_host;
mod login;
mod notify;
mod pages;
mod scheduler;
mod session;
mod signin;
mod update;
mod voice;
mod web_shell;
mod web_signin;
mod webui;
pub(crate) mod widget;
mod window_state;

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
/// The same staging for the shell window's engine: the bundled
/// WebKitGTK 4.1 library is patched to look at this path, and the app
/// points it at the bundle's 4.1 helpers before it spawns the window,
/// whose WebKit spawns the same way this process's does.
pub(crate) const WEBKIT41_STAGING: &str = "/tmp/.webkit2gtk-4.1-umfl";

fn main() {
    crate::webui::mark_start();
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
    // One shell window per process: a second launch forwards its activation
    // here, and the window already on screen is raised instead of a second one
    // being built. The native shell keeps its own behaviour; the Tauri window
    // is the app, so a duplicate would be two apps.
    let opened = Rc::new(std::cell::Cell::new(false));
    app.connect_activate(move |app| {
        if web_shell::tauri() && opened.replace(true) {
            web_shell::raise();
            return;
        }
        opened.set(true);
        build_ui(app);
    });
    app.run();
}

/// Point the staging paths at the bundle's helpers. Inside the
/// AppImage the helpers ride beside the binary (`usr/lib/webkitgtk-6.0`
/// and `usr/lib/webkit2gtk-4.1` next to `usr/bin/muse`); outside it
/// there is nothing to stage and this is a silent no-op. Failures only
/// log: a missing stage means widget frames fall back to text instead
/// of aborting the process, and a missing 4.1 stage means the shell
/// window cannot come up and the native one takes its place.
fn stage_webkit_helpers() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("muse: cannot locate its own binary: {error}");
            return;
        }
    };
    let prefix = match exe.parent().and_then(|bin| bin.parent()) {
        Some(prefix) => prefix,
        None => return,
    };
    stage_helpers(
        &prefix.join("lib/webkitgtk-6.0"),
        std::path::Path::new(WEBKIT_STAGING),
    );
    stage_helpers(
        &prefix.join("lib/webkit2gtk-4.1"),
        std::path::Path::new(WEBKIT41_STAGING),
    );
}

/// Link one generation's staging path at its bundled helpers, when the
/// bundle carries them.
fn stage_helpers(helpers: &std::path::Path, stage: &std::path::Path) {
    if !helpers.join("WebKitNetworkProcess").is_file() {
        return;
    }
    if std::fs::read_link(stage).ok().as_deref() == Some(helpers) {
        return;
    }
    let _ = std::fs::remove_file(stage);
    // A stale directory (an older broken stage) must go before the
    // link lands; its absence is the common case, not an error.
    if stage.is_dir() && std::fs::remove_dir_all(stage).is_err() {
        eprintln!("muse: cannot clear {}", stage.display());
        return;
    }
    if std::os::unix::fs::symlink(helpers, stage).is_err() {
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
    // Which window this launch opens. The Tauri shell is the app's window when
    // it can be (see `web_shell`); the native window stays the fallback and the
    // reference.
    let tauri = web_shell::tauri();
    // Plain ApplicationWindow: AdwApplicationWindow refuses a manually
    // set titlebar, and without one the window has no drag handle or
    // window controls.
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title("Muse")
        .build();
    // The geometry is the reader's: the last launch's size and maximized state,
    // or the whole primary display on a first launch. No size constant stands
    // in for that, and the window never opens as a small floating panel.
    crate::window_state::apply(&window);
    crate::window_state::remember(&window);
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
    let linked = session::load_saved_link()
        .filter(|link| link.consent)
        .is_some();
    let mut page_signin = false;
    if linked {
        eprintln!("muse: saved link found, opening chat");
        show_main(&split);
    } else if tauri {
        // The window is the sign-in surface: the same page, the same rules, and
        // a link that lands builds the app and switches this window to it.
        eprintln!("muse: no saved link, opening sign-in in the shell window");
        signin_in_shell(&split);
    } else {
        eprintln!("muse: no saved link, showing sign-in");
        page_signin = show_signin(&split);
    }
    window.set_child(Some(&split));
    {
        // Escape folds the page drawer away, the way a slide-over
        // closes. The handler sits on the window because that is where
        // every key press lands, whatever page widget holds focus -- and
        // when the drawer is already folded it answers "not mine", so
        // the press goes on to the page.
        // Capture phase: the composer's own key handling and the text
        // entries' Emacs bindings must not eat Ctrl+K or Ctrl+N before
        // the shell sees them.
        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        keys.connect_key_pressed(|_, key, _, modifiers| {
            if key == gtk4::gdk::Key::Escape
                && FOLD_DRAWER.with(|slot| slot.borrow().as_ref().is_some_and(|fold| fold()))
            {
                return gtk4::glib::Propagation::Stop;
            }
            if SHELL_KEYS.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .is_some_and(|handle| handle(key, modifiers))
            }) {
                return gtk4::glib::Propagation::Stop;
            }
            gtk4::glib::Propagation::Proceed
        });
        window.add_controller(keys);
    }
    // A page surface is its own window; presenting an empty app window behind
    // it would be a black rectangle with no sign-in in it.
    if !page_signin && !tauri {
        window.present();
    }
    // A fresh install pays one-time setup (config folders, helper
    // staging, interpreter probe): a spinner and the reader's sentence
    // cover it, once, instead of a silent stall. Returning installs
    // skip this entirely. The shell window has no first-run surface yet, so it
    // pays the same work without a dialog over the page being built.
    if session::first_run_marker().is_none_or(|marker| !marker.is_file()) {
        if tauri {
            std::thread::spawn(|| {
                session::first_run_setup();
                session::note_first_run_done();
            });
        } else {
            show_first_run(&window);
        }
    }
    if tauri {
        let quit = Rc::new({
            let app = app.clone();
            move || app.quit()
        });
        // A window that starts and never answers is a window that cannot show
        // anything: the native one takes its place, and the app stays native.
        let fallback = Rc::new({
            let window = window.clone();
            let split = split.clone();
            move || {
                if linked {
                    window.present();
                } else {
                    // No window and no link: the native sign-in is the surface
                    // that can still carry one.
                    show_signin(&split);
                }
            }
        });
        if !web_shell::launch(quit, fallback) {
            eprintln!("muse: the shell window could not start; showing the native window");
            if linked {
                window.present();
            } else {
                show_signin(&split);
            }
        }
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
    // The step as both surfaces show it: the page reads this cache, and the
    // modal's two lines are the same words.
    crate::pages::first_run_cache(crate::pages::held_first_run(false));
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        session::first_run_setup();
        session::note_first_run_done();
        let _ = done_tx.send(());
    });
    if crate::webui::available() && crate::webui::open_first_run_window() {
        // A page that is the only surface open reads the answer here, not from
        // a paint it does not have.
        gtk4::glib::idle_add_local(move || {
            use std::sync::mpsc::TryRecvError;
            match done_rx.try_recv() {
                Ok(()) => {
                    crate::pages::first_run_cache(crate::pages::held_first_run(true));
                    // The step is over and the page is the whole of its
                    // window: it closes here, as the modal did, and the
                    // surface that named the step takes the screen.
                    crate::webui::close_page_window("firstrun");
                    gtk4::glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => gtk4::glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => gtk4::glib::ControlFlow::Break,
            }
        });
        return;
    }
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
    // The first-run page reads the step as the modal shows it.
    crate::pages::first_run_cache(crate::pages::held_first_run(false));
    std::thread::spawn(move || {
        session::first_run_setup();
        session::note_first_run_done();
        // The step is over: a page that is still open reads its own answer.
        gtk4::glib::MainContext::default().invoke(|| {
            crate::pages::first_run_cache(crate::pages::held_first_run(true));
        });
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

/// The shell's own key handler: takes a key and its modifiers and
/// answers whether it dealt with the press.
type ShellKeys = std::rc::Rc<dyn Fn(gtk4::gdk::Key, gtk4::gdk::ModifierType) -> bool>;

thread_local! {
    /// The page drawer's Escape action, installed by the shell that owns
    /// the drawer and run by the native window: it sees every key press
    /// wherever focus sits on the page, the split view does not. The
    /// action folds the drawer and answers whether it did.
    static FOLD_DRAWER: std::cell::RefCell<Option<std::rc::Rc<dyn Fn() -> bool>>> =
        std::cell::RefCell::new(None);
    /// The shell's own keys, installed by show_main and run by the same
    /// window handler: the three destinations the rail's tools name, and
    /// the F6 walk between panes. Absent on the sign-in surface, where a
    /// Ctrl+N would open a chat the reader cannot reach yet.
    static SHELL_KEYS: std::cell::RefCell<Option<ShellKeys>> = std::cell::RefCell::new(None);
}

/// The four places the keyboard can live in the main shell: the icon
/// rail, the page drawer, the conversation and the details panel.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Rail,
    Drawer,
    Chat,
    Details,
}

/// Which pane a focused widget belongs to, read from its ancestry: the
/// walk needs to know where the keyboard is without tracking it.
fn pane_of(
    widget: &gtk4::Widget,
    rail: &gtk4::Widget,
    drawer_body: &gtk4::Widget,
    panel: &gtk4::Widget,
) -> Pane {
    let mut node = Some(widget.clone());
    while let Some(current) = node {
        if current == *rail {
            return Pane::Rail;
        }
        if current == *drawer_body {
            return Pane::Drawer;
        }
        if current == *panel {
            return Pane::Details;
        }
        node = current.parent();
    }
    Pane::Chat
}

/// Step the keyboard to the next pane, forward or back. The drawer opens
/// itself when the walk lands on it, because a folded list cannot take
/// the keyboard; the details panel does the same inside Chat.
fn step_pane(
    ctx: &pages::Ctx,
    rail: &gtk4::Box,
    drawer_body: &gtk4::Box,
    pages_list: &gtk4::ListBox,
    drawer: &gtk4::Revealer,
    open_drawer: &Rc<dyn Fn(&gtk4::Button)>,
    back: bool,
) {
    let Some(chat) = ctx.chat.upgrade() else {
        return;
    };
    let panel = chat.panel_widget();
    let focus = rail
        .root()
        .and_then(|root| gtk4::prelude::RootExt::focus(&root));
    let current = focus
        .as_ref()
        .map(|widget| pane_of(widget, rail.upcast_ref(), drawer_body.upcast_ref(), &panel))
        .unwrap_or(Pane::Rail);
    let next = match (current, back) {
        (Pane::Rail, false) => Pane::Drawer,
        (Pane::Rail, true) => Pane::Details,
        (Pane::Drawer, false) => Pane::Chat,
        (Pane::Drawer, true) => Pane::Rail,
        (Pane::Chat, false) => Pane::Details,
        (Pane::Chat, true) => Pane::Drawer,
        (Pane::Details, false) => Pane::Rail,
        (Pane::Details, true) => Pane::Chat,
    };
    // The rail's own button for the open page is the door the reader
    // came in by, so it is where the walk lands and what opens the list.
    let rail_button = ctx
        .rail
        .iter()
        .find(|(_, button)| button.has_css_class("muse-rail-active"))
        .or_else(|| ctx.rail.first())
        .map(|(_, button)| button.clone());
    match next {
        Pane::Rail => {
            if let Some(button) = rail_button {
                button.grab_focus();
            }
        }
        Pane::Drawer => {
            if !drawer.reveals_child()
                && let Some(button) = &rail_button
            {
                open_drawer(button);
            }
            if !pages_list
                .row_at_index(0)
                .is_some_and(|row| row.grab_focus())
            {
                // The slide keeps the rows unmapped for a moment: land
                // on the first page once it settles.
                let list = pages_list.clone();
                gtk4::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(230),
                    move || {
                        if let Some(row) = list.row_at_index(0) {
                            row.grab_focus();
                        }
                    },
                );
            }
        }
        Pane::Chat => {
            chat.focus_chat();
        }
        Pane::Details => {
            chat.focus_panel();
        }
    }
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
    widget::describe(&search_button, "Search");
    let new_button = gtk4::Button::with_label("✎");
    new_button.add_css_class("flat");
    widget::describe(&new_button, "New chat");
    tools.append(&search_button);
    tools.append(&new_button);
    // The way into the web shell: the rail, the page list and every ported page
    // in one window of their own. This button is the part of this window that
    // the port retires last.
    let shell_button = gtk4::Button::with_label("Shell");
    shell_button.set_widget_name("shell");
    shell_button.add_css_class("flat");
    widget::describe(&shell_button, "Open the web shell");
    tools.append(&shell_button);
    side.append(&tools);
    let sidebar = gtk4::ListBox::new();
    for name in pages::PAGES {
        let row = gtk4::ListBoxRow::new();
        // A row is a Tab stop: the keyboard reaches the page list, and
        // the arrow keys move down it without opening anything.
        row.set_focusable(true);
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
    widget::describe(&gear, "Settings");
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
        widget::describe(&button, tooltip);
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
    // The schedules the service holds are this process's job, not a page's:
    // the timer starts here, once, and keeps arming and firing for as long as
    // the app runs, whatever the window is doing.
    scheduler::start(Rc::downgrade(&chat));
    // App-lifetime singletons: the signal handlers and idle sources
    // hold only weak references, so the strong references live here,
    // leaked for the life of the process.
    let weak_chat = Rc::downgrade(&chat);
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
            Rc::new(move || {
                show_signin(&split);
            })
        },
    };
    let ctx = Rc::new(ctx);
    // The Settings page reads the updater from here, so its card offers the
    // presses this app can take even when the window is the only surface.
    pages::hold_updates(ctx.updates.clone());
    // The shell window asks this process for the pages' own answers; see
    // `web_shell`. Installed here, where the chat panel, its routes and the
    // updater's held state are all reachable, so a page press reaches the same
    // sender, chooser, decision path and updater a press in the native window
    // does.
    if web_shell::tauri() {
        web_shell::install(
            chat.client_handle(),
            shell_routes(&chat, &ctx, split.clone()),
        );
    }
    // The panel's owner for the life of this window: sign-out releases it, so
    // each sign-in in one process builds a new panel instead of adding a
    // retired one to the process's memory.
    app::hold_chat(chat);
    // The shell mounts every page, so it carries the same routes the pages'
    // own windows do: wired here, where the app's chat panel is reachable.
    {
        let chat = ctx.chat.clone();
        let split_for_shell = split.clone();
        let ctx_for_shell = ctx.clone();
        if let Some(shell) = pages::named_button(&side, "shell") {
            shell.connect_clicked(move |_| {
                let routes = chat
                    .upgrade()
                    .map(|chat| shell_routes(&chat, &ctx_for_shell, split_for_shell.clone()));
                if !crate::webui::open_shell_window(routes) {
                    eprintln!("muse: the web shell is unavailable in this build");
                }
            });
        }
    }
    let nav_ctx = ctx.clone();
    // A page opens on activation — Return, Space or a press — not on
    // selection: browsing the list with the arrow keys would otherwise
    // swap the page at every step.
    sidebar.connect_row_activated(move |_, row| {
        let index = row.index();
        if let Some(name) = pages::PAGES.get(index as usize) {
            pages::show(&nav_ctx, name);
        }
    });
    // The History section opens threads the same way the Threads page
    // does: the row name carries the id, a `None` row would mean new.
    {
        let ctx = ctx.clone();
        // Activation opens a thread, selection does not: the arrow
        // keys walk History without yanking the reader into a thread.
        history.connect_row_activated(move |list, row| {
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
    // The everyday keys. Ctrl+K, Ctrl+N and Ctrl+, land where the
    // rail's tools land; F6 walks the four panes — rail, drawer, chat,
    // details — so crossing the window never means tabbing through every
    // control on the way, and Shift+F6 walks back.
    {
        let ctx = ctx.clone();
        let rail_pane = rail.clone();
        let drawer_body = side.clone();
        let pages_list = sidebar.clone();
        let drawer_handle = drawer.clone();
        let open: Rc<dyn Fn(&gtk4::Button)> = open_drawer.clone();
        SHELL_KEYS.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(
                move |key: gtk4::gdk::Key, modifiers: gtk4::gdk::ModifierType| {
                    let ctrl = modifiers.contains(gtk4::gdk::ModifierType::CONTROL_MASK);
                    let shift = modifiers.contains(gtk4::gdk::ModifierType::SHIFT_MASK);
                    match key {
                        gtk4::gdk::Key::k if ctrl => {
                            pages::show(&ctx, "search");
                            true
                        }
                        gtk4::gdk::Key::n if ctrl => {
                            pages::open_thread(&ctx, None);
                            true
                        }
                        gtk4::gdk::Key::comma if ctrl => {
                            pages::show(&ctx, "settings");
                            true
                        }
                        gtk4::gdk::Key::F6 => {
                            step_pane(
                                &ctx,
                                &rail_pane,
                                &drawer_body,
                                &pages_list,
                                &drawer_handle,
                                &open,
                                shift,
                            );
                            true
                        }
                        _ => false,
                    }
                },
            )
                as Rc<dyn Fn(gtk4::gdk::Key, gtk4::gdk::ModifierType) -> bool>);
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
    if web_shell::tauri() {
        // With the native window hidden, no visit in this window fills the
        // shell's caches: this is the visit the reader's first page would have
        // made. It is the same `show` a rail press runs, so the chat's own
        // refresh and the drawer's History are the reads the native window
        // makes, not a second set.
        pages::show(&ctx, "chat");
    }
    // The sign-in page hides the window while it stands, and this is the
    // surface that brings it back -- now that its sidebar and its page are in
    // place, so nothing is presented half-built. At startup the split is not in
    // the window yet, so this is a no-op and startup's own present stands. The
    // shell window is the app's surface, so the native one stays hidden there.
    if !web_shell::tauri()
        && let Some(root) = split.root()
        && let Ok(window) = root.downcast::<gtk4::Window>()
    {
        window.present();
    }
}

/// The routes a page in the shell window answers with: the chat panel's own,
/// plus the two presses that belong to the app rather than to one panel --
/// sign-out, which also switches the window, and the updater, whose held state
/// lives with the shell that draws its card. One builder, so the window this
/// launch opens and the one the drawer raises answer identically.
fn shell_routes(
    chat: &Rc<app::Chat>,
    ctx: &Rc<pages::Ctx>,
    split: adw::OverlaySplitView,
) -> crate::webui::WebRoutes {
    let mut routes = chat.web_routes();
    routes.sign_out = sign_out_route(Rc::downgrade(chat), split);
    let (check, install) = pages::update_press_routes(pages::Ctx::clone(ctx));
    routes.update_check = check;
    routes.update_install = install;
    routes
}

/// The press a page's Sign out makes: the app's one link-clearing function,
/// then the surface that can take a new link. The window's page moves with the
/// same `show` line a link uses to reach the shell, so sign-out and sign-in are
/// one mechanism in both directions.
fn sign_out_route(
    chat: std::rc::Weak<app::Chat>,
    split: adw::OverlaySplitView,
) -> crate::webui::WebSignOut {
    Rc::new(move || {
        crate::pages::sign_out()?;
        if let Some(chat) = chat.upgrade() {
            chat.retire();
        }
        // Nothing owns the account now: the shell's account line must not keep
        // the name the retired panel last read.
        crate::pages::chat_cache(crate::pages::HeldChat::default());
        let split = split.clone();
        gtk4::glib::idle_add_local_once(move || {
            if web_shell::tauri() {
                // Installing the sign-in surface drops this session's page
                // routes, and the routes were the last strong reference to the
                // chat panel besides the app's own.
                signin_in_shell(&split);
                web_shell::show_signin();
            } else {
                // The drawer's GTK shell window has no sign-in page: it closes,
                // and the native surface carries the link.
                crate::webui::close_page_window("shell");
                show_signin(&split);
            }
            if let Some(refs) = app::release_chat() {
                eprintln!(
                    "muse: the retired chat panel is released ({refs} strong reference(s) at release)"
                );
            }
        });
        Ok(())
    })
}

/// The sign-in surface in the shell window. A fresh clone has no saved link, so
/// the window opens on the sign-in page (`web_shell::boot_page`) and this is
/// what answers it: the app's own validation, save and isolated browser flow,
/// installed before the window asks its first snapshot. The one-time setup runs
/// here rather than behind a dialog, because the page's own defaults are its
/// product. A link that lands builds the app exactly as the native sign-in
/// surface does, then hands the window the shell document.
fn signin_in_shell(split: &adw::OverlaySplitView) {
    if session::first_run_marker().is_none_or(|marker| !marker.is_file()) {
        session::first_run_setup();
        session::note_first_run_done();
    }
    crate::pages::signin_cache(crate::pages::HeldSignin::opening());
    let split_for_link = split.clone();
    let press = web_signin::routes(move || {
        let split = split_for_link.clone();
        gtk4::glib::idle_add_local_once(move || {
            show_main(&split);
            web_shell::show_shell();
        });
    });
    web_shell::install(None, crate::webui::signin_surface(press));
}

/// The sign-in surface, shown with no usable saved link and after an explicit
/// sign-out. True when a page took it: the page is its own window, so the app
/// window steps aside instead of standing black behind it, and a saved link
/// brings it back through [`show_main`].
fn show_signin(split: &adw::OverlaySplitView) -> bool {
    split.set_show_sidebar(false);
    // No page drawer on this surface: an Escape belongs to the sign-in
    // form, not to a drawer the main shell left behind.
    FOLD_DRAWER.with(|slot| *slot.borrow_mut() = None);
    SHELL_KEYS.with(|slot| *slot.borrow_mut() = None);
    // The sign-in surface in a page, on the app's own rules: the same
    // validation, the same save, the same isolated browser flow. A build
    // without a WebView keeps the GTK form below.
    if crate::webui::available() {
        // The page opens on the app's own starting state, not on the last
        // window's leftovers: the same defaults the native form detects.
        crate::pages::signin_cache(crate::pages::HeldSignin::opening());
        let split_for_link = split.clone();
        // A saved link finishes the surface, it does not add one: the page
        // window closes and the app window takes over. Both happen on the next
        // idle -- the press arrives inside the WebView's own message handler,
        // and closing the window that owns it from there would destroy the
        // view mid-callback.
        let press = web_signin::routes(move || {
            let split = split_for_link.clone();
            gtk4::glib::idle_add_local_once(move || {
                crate::webui::close_page_window("signin");
                show_main(&split);
            });
        });
        let routes = crate::webui::signin_surface(press);
        if crate::webui::open_signin_window(Some(routes)) {
            // One surface at a time: the page is the sign-in, so the window
            // that would otherwise hold an empty split hides until linked.
            if let Some(root) = split.root()
                && let Ok(window) = root.downcast::<gtk4::Window>()
            {
                window.set_visible(false);
            }
            return true;
        }
    }
    let form = signin::SignIn::build({
        let split = split.clone();
        move |_| {
            show_main(&split);
        }
    });
    split.set_content(Some(form.widget()));
    false
}
