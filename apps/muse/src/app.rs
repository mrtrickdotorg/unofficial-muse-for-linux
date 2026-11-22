//! Chat controller for the rewrite: owns the session, the transcript
//! widgets, and the worker-thread dispatch that keeps service reads
//! off the main thread. Replies arrive on main-thread idle callbacks
//! and apply only through the session's ticket slot, so a late reply
//! from a superseded read owns nothing.
//!
//! Quota-limited sends surface the service's own limit sentence as a
//! notice row (via the transcript builders), never as a fake reply.

use std::cell::RefCell;
use std::rc::Rc;

use super::widget::{describe, live_region};
use adw::prelude::*;
use muse_account::AccountClient;

use super::chat::build_rows;
use super::chat_view::{populate, set_newest_text};
use super::composer::{Composer, ComposerRow, StagedImage};
use super::session::{Op, Session, bridge_script, load_saved_link};

/// Run `work` on a worker thread, then `done` on the main thread. The
/// idle source is created on the main thread and owns the `Rc`, so no
/// non-`Send` handle ever crosses threads; the worker only sends its
/// answer back through the channel. A dead worker (disconnect with no
/// answer) simply drops the source.
pub(crate) fn dispatch<T, W, D>(chat: &Rc<Chat>, work: W, done: D)
where
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    D: FnOnce(&Rc<Chat>, T) + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<T>();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    let weak = Rc::downgrade(chat);
    let mut done = Some(done);
    gtk4::glib::idle_add_local(move || {
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                return gtk4::glib::ControlFlow::Continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return gtk4::glib::ControlFlow::Break;
            }
        };
        if let Some(done) = done.take()
            && let Some(chat) = weak.upgrade()
        {
            done(&chat, answer);
        }
        gtk4::glib::ControlFlow::Break
    });
}

/// An option button's press, delivered as the reader's next message.
type OptionCallback = std::rc::Rc<dyn Fn(String)>;
/// A message reaction toggle: the row's id plus the emoji.
type ReactCallback = std::rc::Rc<dyn Fn(String, String)>;
/// A reply press: the quoted row's id plus its text. Stages a quote
/// above the composer; the send itself stays a plain send.
pub(crate) type ReplyCallback = std::rc::Rc<dyn Fn(String, String)>;

// One offered policy flip: the word the button writes.
struct PolicyFlip {
    next: String,
}
pub(crate) type RemoveCallback = std::rc::Rc<dyn Fn(String)>;

/// The delete turn button names its own state: the first press arms
/// ("Confirm delete turn"), the second press on the same turn
/// dispatches. A new turn disarms it. Pure, so the arming rule is
/// testable without a display.
pub fn delete_label(armed: bool) -> &'static str {
    if armed {
        "Confirm delete turn"
    } else {
        "Delete turn"
    }
}

/// The details panel's width, measured on the web (359px): the divider
/// opens there and the header × folds it from there.
pub const PANEL_WIDTH: i32 = 359;

/// The web's right-panel tabs, in the web's order: the name each one
/// means plus the icon it shows. The web's strip is icons only, so the
/// name lives in the tooltip. Mail has no client route yet; its tab
/// owns the honest note, never a fake list.
pub const PANEL_TABS: [(&str, &str); 4] = [
    ("Activity", "utilities-system-monitor-symbolic"),
    ("Approvals", "dialog-question-symbolic"),
    ("Identity", "avatar-default-symbolic"),
    ("Mail", "mail-unread-symbolic"),
];

/// One 32px tile at a panel row's head: a rounded square holding a
/// symbolic glyph, the web's list ornament.
fn panel_tile(icon: &str) -> gtk4::Box {
    let tile = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    tile.add_css_class("muse-panel-tile");
    tile.set_valign(gtk4::Align::Center);
    let glyph = gtk4::Image::from_icon_name(icon);
    glyph.set_pixel_size(16);
    glyph.set_halign(gtk4::Align::Center);
    glyph.set_valign(gtk4::Align::Center);
    tile.append(&glyph);
    tile
}

/// One panel row: the tile, then the row's own text column, sized to
/// the web's 56px pitch by the sheet.
fn panel_row(icon: &str, body: &gtk4::Box) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.set_selectable(false);
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    line.append(&panel_tile(icon));
    body.set_valign(gtk4::Align::Center);
    body.set_hexpand(true);
    line.append(body);
    row.set_child(Some(&line));
    row
}

/// Hold the panel at its 359px width by pinning the divider to its
/// largest position, which is exactly the panel's floor. Once the reader
/// has dragged the divider, their position wins and this does nothing.
fn pin_panel(chat: &Chat, split: &gtk4::Paned) {
    if chat.panel_dragged.get() || !chat.panel.is_visible() {
        return;
    }
    let floor = split.max_position();
    if floor <= 0 || floor == split.position() {
        return;
    }
    split.set_position(floor);
}

/// The paned's own handle: the child that is neither of its two panes.
fn split_handle(split: &gtk4::Paned) -> Option<gtk4::Widget> {
    let start = split.start_child()?;
    let end = split.end_child()?;
    let mut child = split.first_child();
    while let Some(widget) = child {
        if widget != start && widget != end {
            return Some(widget);
        }
        child = widget.next_sibling();
    }
    None
}

/// A decision button's words: the service's outcome in the reader's
/// language. Pure so the wording is unit-tested.
pub fn decision_label(decision: muse_account::Decision) -> &'static str {
    match decision {
        muse_account::Decision::AllowOnce => "Allow once",
        muse_account::Decision::AllowTask => "Allow task",
        muse_account::Decision::AllowSite => "Allow site",
        muse_account::Decision::AllowAlways => "Allow always",
        muse_account::Decision::Deny => "Deny",
    }
}

/// One approval row's headline: the title plus the service's own
/// category words, never an invented paraphrase. Pure so the
/// phrasing is unit-tested.
pub fn approval_headline(approval: &muse_account::Approval) -> String {
    let mut head = if approval.title.is_empty() {
        "(approval)".to_owned()
    } else {
        approval.title.clone()
    };
    let mut tags: Vec<&str> = Vec::new();
    for tag in [
        &approval.kind,
        &approval.decision,
        &approval.always_scope,
        &approval.attention_mode,
    ]
    .into_iter()
    .flatten()
    {
        if !tag.is_empty() {
            tags.push(tag);
        }
    }
    if !tags.is_empty() {
        head.push_str(&format!(" ({})", tags.join(", ")));
    }
    head
}

/// Days since the civil epoch for a calendar date (Howard Hinnant's
/// algorithm): pure date math shared with the feed's relative times.
pub(crate) fn days_from_civil(year: i32, month: u32, day: u32) -> i32 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_p = (month as i32 + 9).rem_euclid(12);
    let day_of_year = (153 * month_p + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

/// Which panel section an activity belongs in against the local
/// today (`YYYY-MM-DD`): today, yesterday, or anything older. Future
/// stamps read as today; undated ones land in Earlier, never dropped.
pub(crate) fn activity_bucket(day: Option<&str>, today: &str) -> &'static str {
    let Some(day) = day else {
        return "Earlier";
    };
    let parse = |date: &str| {
        let (year, rest) = date.split_once('-')?;
        let (month, day) = rest.split_once('-')?;
        Some((
            year.parse::<i32>().ok()?,
            month.parse::<u32>().ok()?,
            day.parse::<u32>().ok()?,
        ))
    };
    let (today, day) = match (parse(today), parse(day)) {
        (Some(today), Some(day)) => (today, day),
        _ => return "Earlier",
    };
    let delta = days_from_civil(today.0, today.1, today.2) - days_from_civil(day.0, day.1, day.2);
    match delta {
        i32::MIN..=0 => "Today",
        1 => "Yesterday",
        _ => "Earlier",
    }
}

/// The reader's local today as `YYYY-MM-DD` for the panel groups.
pub(crate) fn local_today() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut broken = std::mem::MaybeUninit::<libc::tm>::uninit();
    // POSIX localtime_r: one call, no allocation, no timezone crate.
    let broken = unsafe {
        libc::localtime_r(&now, broken.as_mut_ptr());
        &*broken.as_ptr()
    };
    format!(
        "{:04}-{:02}-{:02}",
        broken.tm_year + 1900,
        broken.tm_mon + 1,
        broken.tm_mday
    )
}
/// What a delete-turn press means against the held history and the
/// armed target. The first press arms, the second press on the same
/// latest turn confirms, anything else (no history, a moved-on
/// transcript) does nothing.
#[derive(Debug, PartialEq, Eq)]
enum DeleteAction {
    Nothing,
    Arm(String),
    Confirm(String),
}

fn delete_action(latest: Option<&str>, armed: Option<&str>) -> DeleteAction {
    match (latest, armed) {
        (Some(id), Some(armed)) if id == armed => DeleteAction::Confirm(id.to_owned()),
        (Some(id), _) => DeleteAction::Arm(id.to_owned()),
        (None, _) => DeleteAction::Nothing,
    }
}

/// Shared chat state: the session plus every widget a completion
/// touches. Reference-counted because idle callbacks own it.
pub struct Chat {
    session: Rc<RefCell<Session>>,
    composer: Rc<RefCell<Composer>>,
    transcript: gtk4::ListBox,
    scrolled: gtk4::ScrolledWindow,
    composer_row: ComposerRow,
    header: gtk4::Box,
    heading: gtk4::Label,
    delete: gtk4::Button,
    client: RefCell<Option<AccountClient>>,
    older_button: gtk4::Button,
    older_exhausted: RefCell<bool>,
    live_tx: RefCell<Option<std::sync::mpsc::Sender<()>>>,
    on_option: RefCell<Option<OptionCallback>>,
    on_remove: RefCell<Option<RemoveCallback>>,
    on_react: RefCell<Option<ReactCallback>>,
    on_reply: RefCell<Option<ReplyCallback>>,
    local_host: RefCell<super::local_host::Controller>,
    pub(crate) local_host_sync: RefCell<bool>,
    notice: gtk4::Label,
    send_note: RefCell<Option<String>>,
    armed_delete: RefCell<Option<String>>,
    painted: RefCell<Option<super::session::HistoryFingerprint>>,
    follow_after_render: RefCell<bool>,
    thread: RefCell<Option<String>>,
    model_combo: gtk4::ComboBoxText,
    recording: RefCell<Option<super::voice::Recording>>,
    preview_fetch: RefCell<bool>,
    panel: gtk4::Box,
    /// The header button that opens and folds the panel: the one place
    /// that names the next press, whoever makes it.
    panel_toggle: gtk4::Button,
    panel_name: gtk4::Label,
    panel_status: gtk4::Label,
    panel_sched_head: gtk4::Label,
    panel_schedules: gtk4::ListBox,
    panel_today: gtk4::ListBox,
    panel_approvals: gtk4::ListBox,
    panel_approvals_empty: gtk4::Label,
    panel_decisions: gtk4::ListBox,
    panel_decisions_empty: gtk4::Label,
    panel_policy: gtk4::ListBox,
    panel_policy_empty: gtk4::Label,
    panel_identity_name: gtk4::Label,
    panel_identity_vm: gtk4::Label,
    panel_fetch: RefCell<bool>,
    panel_at: RefCell<Option<std::time::Instant>>,
    panel_tasks: RefCell<Option<muse_account::TasksSnapshot>>,
    /// Set once the reader has moved the divider by hand: their
    /// position then wins over the panel's pinned width.
    panel_dragged: std::cell::Cell<bool>,
    /// Whether a press is being held on the divider, the only way a
    /// position change can be the reader's.
    panel_press: std::cell::Cell<bool>,
}

/// Move the keyboard to the first control under `widget` that will take
/// it. `can_focus` alone is not a promise: a container can announce it
/// and still refuse the grab, so the grab itself is the test.
fn focus_first(widget: &gtk4::Widget) -> bool {
    if widget.is_visible() && widget.is_sensitive() && widget.can_focus() && widget.grab_focus() {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if focus_first(&current) {
            return true;
        }
        child = current.next_sibling();
    }
    false
}

/// What one press of Send did. The native window ignores this -- its refusals
/// are already notes on the same screen -- and the web mirror reports it back,
/// so a second surface can be honest about a press without owning a second
/// sender.
pub(crate) enum SendStart {
    Dispatched,
    Refused(&'static str),
}

impl Chat {
    pub fn build(transcript: gtk4::ListBox, composer_row: ComposerRow) -> Rc<Self> {
        let scrolled = gtk4::ScrolledWindow::new();
        scrolled.set_child(Some(&transcript));
        scrolled.add_css_class("muse-transcript");
        // No bars are ever drawn: the scrollbar floats over the
        // content instead of reserving a gutter, and the slider
        // itself stays transparent (see the theme). The wheel and
        // follow-to-latest drive the vertical adjustment, which stays
        // fully functional: hiding the bar must never collapse it.
        scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scrolled.set_overlay_scrolling(true);
        // The Mac panel names its thread up top, centered, with the
        // turn menu behind a "…" button at the row's end.
        let column_title = gtk4::Label::new(Some("Chats"));
        column_title.add_css_class("muse-column-title");
        let heading = gtk4::Label::new(Some("New chat"));
        heading.set_hexpand(true);
        heading.set_halign(gtk4::Align::Center);
        heading.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        heading.add_css_class("muse-thread-title");
        let delete = gtk4::Button::with_label("Delete turn");
        let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        header.add_css_class("muse-chat-header");
        header.append(&column_title);
        header.append(&heading);
        let menu = gtk4::MenuButton::new();
        menu.set_label("…");
        menu.add_css_class("muse-header-pill");
        describe(&menu, "Turn options");
        // The web slice, beside the native transcript: a read-only mirror
        // that follows this window's reads. Kept in the turn menu rather
        // than the header row, so the port costs the chat header no pixel.
        let web_view = gtk4::Button::with_label("Open in web view");
        web_view.add_css_class("flat");
        web_view.set_tooltip_text(Some(
            "Open this chat in the web UI slice of the port (read-only)",
        ));
        {
            let popover = gtk4::Popover::new();
            let body = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
            body.append(&web_view);
            body.append(&delete);
            popover.set_child(Some(&body));
            menu.set_popover(Some(&popover));
        }
        let call = gtk4::Button::with_label("Call");
        call.add_css_class("muse-header-pill");
        describe(&call, "Call");
        let invite = gtk4::Button::with_label("Invite");
        invite.add_css_class("muse-header-pill");
        describe(&invite, "Invite");
        let panel_toggle = gtk4::Button::new();
        panel_toggle.add_css_class("muse-header-icon");
        describe(&panel_toggle, "Hide the details panel");
        panel_toggle.set_child(Some(&gtk4::Image::from_icon_name("window-close-symbolic")));
        header.append(&call);
        header.append(&invite);
        header.append(&menu);
        header.append(&panel_toggle);
        // The web's right panel: profile, scheduled tasks, Today. It
        // reads the same tasks snapshot as the Tasks page, refreshed
        // beside the transcript instead of on a separate visit.
        let panel = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        panel.add_css_class("muse-panel");
        // 359px is the panel's smallest width, which is where its
        // divider sits unless the reader drags it somewhere else.
        panel.set_size_request(PANEL_WIDTH, -1);
        panel.set_margin_start(8);
        panel.set_margin_end(0);
        panel.set_margin_top(12);
        panel.set_margin_bottom(12);
        let avatar = gtk4::Label::new(Some("✦"));
        avatar.set_halign(gtk4::Align::Center);
        avatar.add_css_class("muse-avatar-large");
        let panel_name = gtk4::Label::new(Some("Sign in"));
        panel_name.set_halign(gtk4::Align::Center);
        panel_name.add_css_class("title-4");
        let panel_status = gtk4::Label::new(None);
        panel_status.set_halign(gtk4::Align::Center);
        panel_status.add_css_class("dim-label");
        panel.append(&avatar);
        panel.append(&panel_name);
        panel.append(&panel_status);
        // The web's tabs above the panel body: Activity is the
        // profile-and-tasks list already below; Approvals, Identity
        // and Mail swap the body under the strip.
        let panel_stack = gtk4::Stack::new();
        panel_stack.set_vexpand(true);
        // The web's strip is icons only: four equal segments in one
        // raised well, the open one on the lighter overlay. The name
        // each segment means rides its tooltip.
        let tabs = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        tabs.set_halign(gtk4::Align::Fill);
        tabs.add_css_class("muse-panel-tabs");
        let tab_bar: Rc<RefCell<Vec<(&'static str, gtk4::Button)>>> =
            Rc::new(RefCell::new(Vec::new()));
        for (name, icon) in PANEL_TABS {
            let tab = gtk4::Button::new();
            tab.set_child(Some(&gtk4::Image::from_icon_name(icon)));
            describe(&tab, name);
            tab.set_hexpand(true);
            tab.add_css_class("muse-panel-tab");
            let stack = panel_stack.clone();
            let bar = tab_bar.clone();
            tab.connect_clicked(move |_| {
                stack.set_visible_child_name(&name.to_lowercase());
                for (label, other) in bar.borrow().iter() {
                    if *label == name {
                        other.add_css_class("muse-panel-tab-active");
                    } else {
                        other.remove_css_class("muse-panel-tab-active");
                    }
                }
            });
            tab_bar.borrow_mut().push((name, tab.clone()));
            tabs.append(&tab);
        }
        if let Some((_, first)) = tab_bar.borrow().first() {
            first.add_css_class("muse-panel-tab-active");
        }
        panel.append(&tabs);
        let panel_sched_head = gtk4::Label::new(Some("Scheduled tasks"));
        panel_sched_head.set_halign(gtk4::Align::Start);
        panel_sched_head.add_css_class("muse-section");
        // The lists below hold the running day: unbounded natural
        // height would set the window's minimum height, opening the
        // app taller than the screen with no way to shrink it. The
        // body scrolls instead.
        let body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        body.append(&panel_sched_head);
        let panel_schedules = gtk4::ListBox::new();
        panel_schedules.set_selection_mode(gtk4::SelectionMode::None);
        body.append(&panel_schedules);
        let panel_today = gtk4::ListBox::new();
        panel_today.set_selection_mode(gtk4::SelectionMode::None);
        body.append(&panel_today);
        let body_scroll = gtk4::ScrolledWindow::new();
        body_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        body_scroll.set_vexpand(true);
        body_scroll.set_child(Some(&body));
        panel_stack.add_named(&body_scroll, Some("activity"));
        // Approvals: the pending list with the service's own five
        // outcomes on every row. One press maps to one decide write;
        // the refill afterwards proves what the press did.
        let approvals_body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        let panel_approvals_empty = gtk4::Label::new(Some("No pending approvals."));
        panel_approvals_empty.set_halign(gtk4::Align::Start);
        panel_approvals_empty.add_css_class("dim-label");
        panel_approvals_empty.set_wrap(true);
        approvals_body.append(&panel_approvals_empty);
        let panel_approvals = gtk4::ListBox::new();
        panel_approvals.set_selection_mode(gtk4::SelectionMode::None);
        approvals_body.append(&panel_approvals);
        // Recent decisions: the decided past, read-only. Answers
        // "what did it do" without ever becoming a pending choice.
        let panel_decisions_head = gtk4::Label::new(Some("Recent decisions"));
        panel_decisions_head.set_halign(gtk4::Align::Start);
        panel_decisions_head.add_css_class("muse-section");
        approvals_body.append(&panel_decisions_head);
        let panel_decisions_empty = gtk4::Label::new(Some("No recent decisions."));
        panel_decisions_empty.set_halign(gtk4::Align::Start);
        panel_decisions_empty.add_css_class("dim-label");
        panel_decisions_empty.set_wrap(true);
        approvals_body.append(&panel_decisions_empty);
        let panel_decisions = gtk4::ListBox::new();
        panel_decisions.set_selection_mode(gtk4::SelectionMode::None);
        approvals_body.append(&panel_decisions);
        // Policy: the durable settings reads alongside the pending
        // list, with the flips the service accepts on each row. One
        // press maps to one PATCH; the refill afterwards proves
        // what the press did. Connector resets arm first.
        let panel_policy_head = gtk4::Label::new(Some("Policy"));
        panel_policy_head.set_halign(gtk4::Align::Start);
        panel_policy_head.add_css_class("muse-section");
        approvals_body.append(&panel_policy_head);
        let panel_policy_empty = gtk4::Label::new(Some("No policy settings."));
        panel_policy_empty.set_halign(gtk4::Align::Start);
        panel_policy_empty.add_css_class("dim-label");
        panel_policy_empty.set_wrap(true);
        approvals_body.append(&panel_policy_empty);
        let panel_policy = gtk4::ListBox::new();
        panel_policy.set_selection_mode(gtk4::SelectionMode::None);
        approvals_body.append(&panel_policy);
        let approvals_scroll = gtk4::ScrolledWindow::new();
        approvals_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        approvals_scroll.set_vexpand(true);
        approvals_scroll.set_child(Some(&approvals_body));
        panel_stack.add_named(&approvals_scroll, Some("approvals"));
        // Identity: the account's own name and device id, read-only.
        let identity_body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        identity_body.set_margin_top(8);
        let panel_identity_name = gtk4::Label::new(None);
        panel_identity_name.set_halign(gtk4::Align::Start);
        panel_identity_name.add_css_class("title-4");
        panel_identity_name.set_wrap(true);
        identity_body.append(&panel_identity_name);
        let panel_identity_vm = gtk4::Label::new(None);
        panel_identity_vm.set_halign(gtk4::Align::Start);
        panel_identity_vm.add_css_class("dim-label");
        panel_identity_vm.set_wrap(true);
        panel_identity_vm.set_selectable(true);
        identity_body.append(&panel_identity_vm);
        panel_stack.add_named(&identity_body, Some("identity"));
        // Mail has no client route: the tab owns the note, never a
        // fake list.
        let mail_note = gtk4::Label::new(Some("Mail has no client route yet."));
        mail_note.set_halign(gtk4::Align::Start);
        mail_note.add_css_class("dim-label");
        mail_note.set_wrap(true);
        panel_stack.add_named(&mail_note, Some("mail"));
        panel.append(&panel_stack);
        // Thinking lives in the composer card: the pill IS the model
        // picker, filled from the service listing like before.
        let model_combo = composer_row.thinking.clone();
        let chat = Rc::new(Self {
            session: Rc::new(RefCell::new(Session::new())),
            composer: Rc::new(RefCell::new(Composer::new())),
            transcript,
            scrolled,
            composer_row,
            header,
            heading,
            delete,
            client: RefCell::new(None),
            older_button: gtk4::Button::with_label("Load older"),
            notice: {
                let notice = gtk4::Label::new(None);
                notice.set_wrap(true);
                notice.set_halign(gtk4::Align::Center);
                notice.set_visible(false);
                // The status line's own words are the shell's other live
                // region: a refused paste or a failed send replaces the
                // text in place, and the node keeps its identity, so the
                // change is heard instead of passing unseen.
                live_region(&notice);
                notice
            },
            armed_delete: RefCell::new(None),
            send_note: RefCell::new(None),
            painted: RefCell::new(None),
            follow_after_render: RefCell::new(false),
            thread: RefCell::new(None),
            model_combo,
            recording: RefCell::new(None),
            preview_fetch: RefCell::new(false),
            panel,
            panel_name,
            panel_toggle: panel_toggle.clone(),
            panel_status,
            panel_sched_head,
            panel_schedules,
            panel_today,
            panel_approvals,
            panel_approvals_empty,
            panel_decisions,
            panel_decisions_empty,
            panel_policy,
            panel_policy_empty,
            panel_identity_name,
            panel_identity_vm,
            panel_fetch: RefCell::new(false),
            panel_at: RefCell::new(None),
            panel_tasks: RefCell::new(None),
            panel_dragged: std::cell::Cell::new(false),
            panel_press: std::cell::Cell::new(false),
            live_tx: RefCell::new(None),
            older_exhausted: RefCell::new(false),
            on_option: RefCell::new(None),
            on_remove: RefCell::new(None),
            on_react: RefCell::new(None),
            on_reply: RefCell::new(None),
            local_host: RefCell::new(super::local_host::Controller::new()),
            local_host_sync: RefCell::new(false),
        });
        let weak = Rc::downgrade(&chat);
        *chat.on_option.borrow_mut() = Some(std::rc::Rc::new(move |text: String| {
            if let Some(chat) = weak.upgrade() {
                // An option press sends its text as the reader's next
                // message -- the same single-press path as the composer.
                let _ = chat.send_text(text, Vec::new(), None);
            }
        }) as OptionCallback);
        {
            let weak = Rc::downgrade(&chat);
            *chat.on_react.borrow_mut() =
                Some(std::rc::Rc::new(move |message_id: String, emoji: String| {
                    if let Some(chat) = weak.upgrade() {
                        chat.react_message(message_id, emoji);
                    }
                }) as ReactCallback);
        }
        {
            let weak = Rc::downgrade(&chat);
            *chat.on_reply.borrow_mut() =
                Some(std::rc::Rc::new(move |message_id: String, text: String| {
                    if let Some(chat) = weak.upgrade() {
                        chat.stage_reply(message_id, text);
                    }
                }) as ReplyCallback);
        }
        // Call and Invite have no client route: the press says so in
        // the notice line rather than pretending. The × folds the
        // details panel and names the press that brings it back.
        {
            let weak = Rc::downgrade(&chat);
            call.connect_clicked(move |_| {
                if let Some(chat) = weak.upgrade() {
                    chat.say_send_note("Calls have no client route yet.");
                }
            });
        }
        {
            let weak = Rc::downgrade(&chat);
            invite.connect_clicked(move |_| {
                if let Some(chat) = weak.upgrade() {
                    chat.say_send_note("Invites have no client route yet.");
                }
            });
        }
        {
            let weak = Rc::downgrade(&chat);
            panel_toggle.connect_clicked(move |_| {
                if let Some(chat) = weak.upgrade() {
                    chat.toggle_panel();
                }
            });
        }
        {
            // Resolved per press: the mirror is the rows this window
            // holds right now, so pressing after a reply shows the reply.
            let weak = Rc::downgrade(&chat);
            web_view.connect_clicked(move |_| {
                let Some(chat) = weak.upgrade() else {
                    return;
                };
                chat.cache_web_chat();
                let held = crate::pages::chat_snapshot();
                if !crate::webui::open_chat_window(held, chat.client_handle(), chat.web_routes()) {
                    eprintln!("muse: web view unavailable in this build");
                }
            });
        }
        chat.wire();
        chat
    }

    /// Fold the details panel away, or bring it back: the header ×
    /// names what its next press does, so the panel is never lost. The
    /// divider moves too -- a hidden child leaves an empty pane where
    /// the panel was, and the column must take that room back. A
    /// reader who dragged the divider gets it back where it was.
    fn toggle_panel(&self) {
        self.face_panel(!self.panel.is_visible());
    }

    /// Show or fold the panel without a reader press -- the page drawer
    /// steps aside for it while it is open. Same state a press leaves
    /// behind, header button included, so the two never disagree.
    pub fn set_panel_shown(&self, showing: bool) {
        if self.panel.is_visible() == showing {
            return;
        }
        self.face_panel(showing);
    }

    /// Whether the details panel is on screen.
    pub fn panel_shown(&self) -> bool {
        self.panel.is_visible()
    }

    fn face_panel(&self, showing: bool) {
        if !showing && self.panel_holds_focus() {
            self.panel_toggle.grab_focus();
        }
        if showing {
            self.panel_dragged.set(false);
        }
        self.panel.set_visible(showing);
        let (icon, tip) = if showing {
            ("window-close-symbolic", "Hide the details panel")
        } else {
            ("sidebar-show-symbolic", "Show the details panel")
        };
        self.panel_toggle
            .set_child(Some(&gtk4::Image::from_icon_name(icon)));
        describe(&self.panel_toggle, tip);
    }

    /// The panel widget itself, for the shell's pane walk: identity
    /// only, so the walk can tell where the keyboard is.
    pub fn panel_widget(&self) -> gtk4::Widget {
        self.panel.clone().upcast()
    }

    /// Land the keyboard on the conversation: the transcript takes it so
    /// the page keys scroll the messages, and the composer is the
    /// fallback for when the list has nothing to focus yet.
    pub fn focus_chat(&self) -> bool {
        self.transcript.grab_focus() || self.composer_row.input.grab_focus()
    }

    /// Land the keyboard on the details panel, showing it first if it
    /// was folded: a hidden pane cannot take the keyboard, and the walk
    /// asking for it is the reader asking to see it.
    pub fn focus_panel(&self) -> bool {
        if !self.panel.is_visible() {
            self.set_panel_shown(true);
        }
        focus_first(self.panel.upcast_ref()) || self.panel_toggle.grab_focus()
    }

    /// Whether the keyboard is somewhere inside the panel: the panel is
    /// about to disappear, and focus inside it would have nowhere to go.
    fn panel_holds_focus(&self) -> bool {
        let Some(root) = self.panel.root() else {
            return false;
        };
        let Some(focus) = gtk4::prelude::RootExt::focus(&root) else {
            return false;
        };
        let mut widget = Some(focus);
        while let Some(current) = widget {
            if current == self.panel {
                return true;
            }
            widget = current.parent();
        }
        false
    }

    /// Build the whole chat surface for the chat page: transcript plus
    /// composer, linked to the saved account when one exists. No saved
    /// link, no dispatch -- the page simply waits for a sign-in.
    pub fn install(content: &gtk4::Box) -> Rc<Self> {
        content.set_orientation(gtk4::Orientation::Horizontal);
        content.set_spacing(0);
        let transcript = gtk4::ListBox::new();
        transcript.set_selection_mode(gtk4::SelectionMode::None);
        let composer_row = ComposerRow::build();
        let chat = Self::build(transcript, composer_row);
        chat.scrolled.set_vexpand(true);
        chat.scrolled.set_hexpand(true);
        chat.older_button.set_halign(gtk4::Align::Center);
        {
            let weak = Rc::downgrade(&chat);
            chat.older_button.connect_clicked(move |_| {
                if let Some(chat) = weak.upgrade() {
                    chat.load_older();
                }
            });
        }
        chat.refresh_older();
        // The chat page is two columns like the web: the transcript
        // on the left, the profile-and-tasks panel on the right.
        let left = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        left.set_hexpand(true);
        // The column reads as one surface with the transcript: the
        // theme's view color behind everything, never a darker frame
        // around a lighter strip.
        left.add_css_class("muse-chat-column");
        left.append(&chat.header);
        left.append(&chat.older_button);
        // The web's transcript is a centered column, never full-bleed:
        // the clamp caps its width and centers it on wide windows.
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(800);
        clamp.set_child(Some(&chat.scrolled));
        left.append(&clamp);
        left.append(&chat.notice);
        left.append(chat.composer_row.widget());
        // Scrolling anywhere over the middle column moves the
        // transcript: the clamp's side margins sit outside the scrolled
        // viewport, so without this a wheel over the margins does
        // nothing. Events the transcript itself can take are left
        // alone — applying here too would double every notch — and
        // only the dead margins forward.
        {
            let scroll =
                gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::VERTICAL);
            scroll.set_propagation_phase(gtk4::PropagationPhase::Capture);
            let left_weak = left.downgrade();
            let scrolled_weak = chat.scrolled.downgrade();
            scroll.connect_scroll(move |controller, _dx, dy| {
                use gtk4::glib::Propagation;
                let (Some(left), Some(scrolled)) = (left_weak.upgrade(), scrolled_weak.upgrade())
                else {
                    return Propagation::Proceed;
                };
                if let Some(event) = controller.current_event()
                    && let Some((x, y)) = event.position()
                    && let Some(target) = left.pick(x, y, gtk4::PickFlags::DEFAULT)
                    && (target == scrolled.clone().upcast::<gtk4::Widget>()
                        || target.is_ancestor(&scrolled))
                {
                    return Propagation::Proceed;
                }
                let adjustment = scrolled.vadjustment();
                let max = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
                let step = adjustment.step_increment().max(1.0);
                adjustment
                    .set_value((adjustment.value() + dy * step).clamp(adjustment.lower(), max));
                Propagation::Stop
            });
            left.add_controller(scroll);
        }
        // The panel is held at its own 359px by pinning the divider to
        // its largest position, which is exactly the panel's floor, so
        // the transcript gets every pixel the window gains. A position
        // short of that floor is the reader's drag and stops the pin.
        let split = gtk4::Paned::new(gtk4::Orientation::Horizontal);
        split.set_wide_handle(true);
        split.set_resize_start_child(true);
        split.set_shrink_start_child(true);
        split.set_resize_end_child(false);
        split.set_shrink_end_child(false);
        split.set_start_child(Some(&left));
        split.set_end_child(Some(&chat.panel));
        content.append(&split);
        {
            let weak = Rc::downgrade(&chat);
            let split_weak = split.downgrade();
            split.connect_max_position_notify(move |_| {
                if let (Some(chat), Some(split)) = (weak.upgrade(), split_weak.upgrade()) {
                    pin_panel(&chat, &split);
                }
            });
            // Only a press held on the divider makes a position change
            // the reader's: a clamp or a relayout moves it without one,
            // and that belongs to the pin, which puts it straight back
            // rather than giving the width away for good.
            if let Some(handle) = split_handle(&split) {
                let weak = Rc::downgrade(&chat);
                let handle_weak = handle.downgrade();
                let split_weak = split.downgrade();
                let press = gtk4::EventControllerLegacy::new();
                press.set_propagation_phase(gtk4::PropagationPhase::Capture);
                press.connect_event(move |_, event| {
                    if let Some(chat) = weak.upgrade() {
                        match event.event_type() {
                            gtk4::gdk::EventType::ButtonPress => {
                                // Only a press on the divider is the reader
                                // taking the width; one on the column is not.
                                // The event arrives in the window's
                                // coordinates, so the handle's bounds are
                                // measured against the root before the two
                                // are compared.
                                let on_divider = match (
                                    event.position(),
                                    handle_weak.upgrade(),
                                    split_weak.upgrade(),
                                ) {
                                    (Some((x, _)), Some(handle), Some(split)) => split
                                        .root()
                                        .and_then(|root| handle.compute_bounds(&root))
                                        .is_some_and(|rect| {
                                            let left = f64::from(rect.x());
                                            x >= left && x < left + f64::from(rect.width())
                                        }),
                                    _ => false,
                                };
                                chat.panel_press.set(on_divider);
                            }
                            gtk4::gdk::EventType::ButtonRelease => chat.panel_press.set(false),
                            _ => {}
                        }
                    }
                    gtk4::glib::Propagation::Proceed
                });
                split.add_controller(press);
            }
            let weak = Rc::downgrade(&chat);
            split.connect_position_notify(move |split| {
                let Some(chat) = weak.upgrade() else { return };
                if chat.panel_dragged.get() || !chat.panel.is_visible() {
                    return;
                }
                if chat.panel_press.get() {
                    chat.panel_dragged.set(true);
                    return;
                }
                let weak = Rc::downgrade(&chat);
                let split_weak = split.downgrade();
                gtk4::glib::idle_add_local_once(move || {
                    if let (Some(chat), Some(split)) = (weak.upgrade(), split_weak.upgrade()) {
                        pin_panel(&chat, &split);
                    }
                });
            });
        }
        {
            let weak = Rc::downgrade(&chat);
            let split_weak = split.downgrade();
            gtk4::glib::idle_add_local_once(move || {
                if let (Some(chat), Some(split)) = (weak.upgrade(), split_weak.upgrade()) {
                    pin_panel(&chat, &split);
                }
            });
        }

        {
            let weak = Rc::downgrade(&chat);
            chat.delete.connect_clicked(move |_| {
                if let Some(chat) = weak.upgrade() {
                    chat.delete_pressed();
                }
            });
        }
        // Temporary geometry probe: one line with the real widths so a
        // shifted or overflowing column reports itself instead of
        // getting described from a screenshot.
        {
            let weak = Rc::downgrade(&chat);
            let content_weak = content.downgrade();
            gtk4::glib::timeout_add_local(std::time::Duration::from_secs(3), move || {
                if let (Some(chat), Some(content)) = (weak.upgrade(), content_weak.upgrade()) {
                    let hadj = chat.scrolled.hadjustment();
                    eprintln!(
                        "muse layout: page={} panel={} hadj upper={:.0} page={:.0}",
                        content.allocated_width(),
                        chat.panel.allocated_width(),
                        hadj.upper(),
                        hadj.page_size(),
                    );
                }
                gtk4::glib::ControlFlow::Break
            });
        }
        if let Some(link) = load_saved_link()
            && link.consent
        {
            chat.load(&link.python, &link.config_dir);
        }
        // Liveness is push-woken polling: the live worker sends one
        // wake per delivered subscription event, and an 8s fallback
        // re-read covers a dead worker. Every round goes through the
        // ticket slot, so each no-ops while a send or connect holds it.
        let (live_tx, live_rx) = std::sync::mpsc::channel::<()>();
        *chat.live_tx.borrow_mut() = Some(live_tx);
        let weak = Rc::downgrade(&chat);
        let mut ticks = 0u8;
        gtk4::glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            let mut woke = false;
            while live_rx.try_recv().is_ok() {
                woke = true;
            }
            ticks = ticks.wrapping_add(1);
            let Some(chat) = weak.upgrade() else {
                return gtk4::glib::ControlFlow::Break;
            };
            if woke || ticks >= 8 {
                ticks = 0;
                chat.reload_history();
            }
            gtk4::glib::ControlFlow::Continue
        });
        chat
    }

    /// The model picker: the service's listing fills it, a change
    /// dispatches a select. The generation guard in the session keeps
    /// a late listing from moving the picker backwards.
    fn refresh_model(&self) {
        let session = self.session.borrow();
        self.model_combo.remove_all();
        for model in &session.available_models {
            self.model_combo.append_text(model);
        }
        if let Some(selected) = session.selected_model.as_deref() {
            self.model_combo.set_active_id(Some(selected));
        }
    }

    /// Read the selected-model state after connect, then fill the picker.
    fn load_model(self: &Rc<Self>) {
        let ticket = match self.session.borrow_mut().try_begin(Op::Model) {
            Some(ticket) => ticket,
            None => return,
        };
        let Some(client) = self.client.borrow().clone() else {
            self.session.borrow_mut().accept(&ticket);
            return;
        };
        dispatch(
            self,
            move || client.model_get(),
            move |chat, answer| {
                if chat.session.borrow_mut().accept(&ticket)
                    && let Ok(info) = answer
                {
                    chat.session.borrow_mut().note_model(&info);
                    chat.refresh_model();
                }
            },
        );
    }

    /// Select one model by its service id. The id comes from the
    /// picker's listing, never a guess.
    fn set_model(self: &Rc<Self>, model: String) {
        let ticket = match self.session.borrow_mut().try_begin(Op::Model) {
            Some(ticket) => ticket,
            None => return,
        };
        let Some(client) = self.client.borrow().clone() else {
            self.session.borrow_mut().accept(&ticket);
            return;
        };
        dispatch(
            self,
            move || client.model_set(&model),
            move |chat, answer| {
                if chat.session.borrow_mut().accept(&ticket)
                    && let Ok(info) = answer
                {
                    chat.session.borrow_mut().note_model(&info);
                    chat.refresh_model();
                }
            },
        );
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.composer_row.send.connect_clicked(move |_| {
            if let Some(chat) = weak.upgrade() {
                chat.send_pressed();
            }
        });
        let weak = Rc::downgrade(self);
        self.composer_row.stop.connect_clicked(move |_| {
            if let Some(chat) = weak.upgrade() {
                let _ = chat.stop_pressed();
            }
        });
        let weak = Rc::downgrade(self);
        self.composer_row
            .input
            .buffer()
            .connect_changed(move |buffer| {
                if let Some(chat) = weak.upgrade() {
                    let start = buffer.start_iter();
                    let end = buffer.end_iter();
                    chat.composer
                        .borrow_mut()
                        .set_text(&buffer.text(&start, &end, false));
                    chat.composer_row.sync_placeholder();
                    chat.composer_row.refresh(&chat.composer.borrow());
                }
            });
        // Enter sends; Shift+Enter keeps a newline. The web's bar
        // behaves the same way.
        let weak = Rc::downgrade(self);
        let send_key = gtk4::EventControllerKey::new();
        send_key.connect_key_pressed(move |_, keyval, _, modifiers| {
            let Some(chat) = weak.upgrade() else {
                return gtk4::glib::Propagation::Proceed;
            };
            if keyval != gtk4::gdk::Key::Return && keyval != gtk4::gdk::Key::KP_Enter {
                return gtk4::glib::Propagation::Proceed;
            }
            if modifiers.contains(gtk4::gdk::ModifierType::SHIFT_MASK) {
                return gtk4::glib::Propagation::Proceed;
            }
            chat.send_pressed();
            gtk4::glib::Propagation::Stop
        });
        self.composer_row.input.add_controller(send_key);
        // Clipboard pictures: Ctrl+V claims the press when the
        // clipboard offers an image, stages the bytes as a chip, and
        // stops the entry from swallowing it. Text pastes are never
        // touched -- the press falls through to the entry. The `+`
        // button is the same path on demand.
        let weak = Rc::downgrade(self);
        let paste = gtk4::EventControllerKey::new();
        paste.connect_key_pressed(move |_, keyval, _, modifiers| {
            let Some(chat) = weak.upgrade() else {
                return gtk4::glib::Propagation::Proceed;
            };
            if keyval != gtk4::gdk::Key::v
                || !modifiers.contains(gtk4::gdk::ModifierType::CONTROL_MASK)
            {
                return gtk4::glib::Propagation::Proceed;
            }
            if chat.paste_from_clipboard() {
                gtk4::glib::Propagation::Stop
            } else {
                gtk4::glib::Propagation::Proceed
            }
        });
        self.composer_row.input.add_controller(paste);
        let weak = Rc::downgrade(self);
        self.composer_row.attach.connect_clicked(move |_| {
            if let Some(chat) = weak.upgrade()
                && !chat.paste_from_clipboard()
            {
                chat.say_send_note("The clipboard holds no picture to stage.");
            }
        });
        // The mic pill starts and stops a dictation take; the second
        // press sends the take to the service for transcription.
        let weak = Rc::downgrade(self);
        self.composer_row.mic.connect_clicked(move |_| {
            if let Some(chat) = weak.upgrade() {
                chat.toggle_dictation();
            }
        });
        // The chip's remove button hands the picture's name back.
        let weak = Rc::downgrade(self);
        let on_remove: RemoveCallback = std::rc::Rc::new(move |name: String| {
            if let Some(chat) = weak.upgrade() {
                chat.composer.borrow_mut().unstage(&name);
                chat.paint_chips();
                chat.composer_row.refresh(&chat.composer.borrow());
                chat.cache_web_chat();
            }
        });
        *self.on_remove.borrow_mut() = Some(on_remove);
        self.paint_chips();
        self.paint_reply();
        self.composer_row.refresh(&self.composer.borrow());
        // The quote bar's × drops the staged reply without sending.
        let weak = Rc::downgrade(self);
        self.composer_row.reply_cancel.connect_clicked(move |_| {
            if let Some(chat) = weak.upgrade() {
                chat.composer.borrow_mut().clear_reply();
                chat.paint_reply();
                chat.composer_row.refresh(&chat.composer.borrow());
            }
        });
        let weak = Rc::downgrade(self);
        self.model_combo.connect_changed(move |combo| {
            let Some(chat) = weak.upgrade() else {
                return;
            };
            let Some(id) = combo.active_id() else {
                return;
            };
            // Programmatic fills echo the held selection; only a real
            // change dispatches.
            if chat.session.borrow().selected_model.as_deref() != Some(id.as_str()) {
                chat.set_model(id.to_string());
            }
        });
    }

    /// Link cards resolve off the main thread: the first few urls in
    /// assistant messages that hold no card yet fetch in one worker.
    /// Arrivals land in the session and bump its version, which
    /// repaints their rows; failures leave the text-only row standing.
    fn fetch_previews(self: &Rc<Self>) {
        if *self.preview_fetch.borrow() {
            return;
        }
        let missing: Vec<String> = {
            let session = self.session.borrow();
            let mut missing = Vec::new();
            for message in session.history.iter().rev() {
                if missing.len() >= 4 {
                    break;
                }
                if message.role.as_str() == "user" {
                    continue;
                }
                for link in super::chat::extract_links(&message.text, 2) {
                    if missing.len() >= 4 {
                        break;
                    }
                    if !session.previews.contains_key(&link)
                        && !missing.iter().any(|known| known == &link)
                    {
                        missing.push(link);
                    }
                }
            }
            missing
        };
        if missing.is_empty() {
            return;
        }
        let Some(client) = self.client.borrow().clone() else {
            return;
        };
        *self.preview_fetch.borrow_mut() = true;
        dispatch(
            self,
            move || {
                let mut filled = Vec::new();
                for url in missing {
                    let Ok(preview) = client.preview_link(&url) else {
                        continue;
                    };
                    let thumb = preview
                        .image_url
                        .as_deref()
                        .and_then(super::images::fetch_hero);
                    filled.push((url, preview, thumb));
                }
                filled
            },
            move |chat,
                  filled: Vec<(
                String,
                muse_account::LinkPreview,
                Option<std::path::PathBuf>,
            )>| {
                *chat.preview_fetch.borrow_mut() = false;
                if filled.is_empty() {
                    return;
                }
                {
                    let mut session = chat.session.borrow_mut();
                    for (url, preview, thumb) in filled {
                        session.previews.insert(url, (preview, thumb));
                    }
                    session.preview_version += 1;
                }
                chat.render();
            },
        );
    }
    /// The web's right panel beside the transcript: profile, scheduled
    /// tasks, Today. Profile paints from held state on every visit;
    /// the tasks snapshot refreshes beside it at most every two
    /// minutes, so opening chat never waits on a read.
    pub fn refresh_panel(self: &Rc<Self>) {
        let name = self.session_name().unwrap_or_else(|| "Sign in".to_owned());
        self.panel_name.set_text(&name);
        let linked = self.client.borrow().is_some();
        self.panel_status
            .set_text(if linked { "● Connected" } else { "" });
        // The web's green Connected reads at a glance; the class only
        // ever layers on, never replaces the dim base.
        if linked {
            self.panel_status.add_css_class("muse-connected");
        }
        if let Some(snapshot) = self.panel_tasks.borrow().clone() {
            self.paint_panel(&snapshot);
        }
        let stale = self
            .panel_at
            .borrow()
            .map(|at| at.elapsed() > std::time::Duration::from_secs(120))
            .unwrap_or(true);
        if !stale || *self.panel_fetch.borrow() {
            return;
        }
        let Some(client) = self.client.borrow().clone() else {
            return;
        };
        *self.panel_fetch.borrow_mut() = true;
        dispatch(
            self,
            move || client.tasks(),
            move |chat, answer| {
                *chat.panel_fetch.borrow_mut() = false;
                let Ok(snapshot) = answer else {
                    return;
                };
                *chat.panel_at.borrow_mut() = Some(std::time::Instant::now());
                *chat.panel_tasks.borrow_mut() = Some(snapshot.clone());
                chat.paint_panel(&snapshot);
            },
        );
        // Identity and approvals read fresh on every visit beside the
        // cached tasks: approvals are time-sensitive, identity is
        // cheap, and both clear honestly when signed out. Policy
        // rides its own dispatch: it changes rarely, and a slow
        // settings read must never hold up the pending list.
        self.refresh_identity_and_approvals();
        self.refresh_policy();
    }

    /// Identity + approvals, fresh on every chat visit. Unlinked paints
    /// the link-first states; a failed approvals read keeps the last
    /// good list rather than blanking it.
    pub fn refresh_identity_and_approvals(self: &Rc<Self>) {
        let Some(client) = self.client.borrow().clone() else {
            self.paint_identity(None);
            Self::paint_approvals(
                self,
                &muse_account::PendingApprovals {
                    pending: Vec::new(),
                },
            );
            Self::paint_decisions(
                self,
                &muse_account::DecisionHistory {
                    decisions: Vec::new(),
                },
            );
            Self::paint_policy(
                self,
                &muse_account::PolicySettings {
                    entries: Vec::new(),
                    connectors: Vec::new(),
                    withheld: 0,
                },
            );
            self.panel_policy_empty.set_text("Link your account first.");
            self.panel_decisions_empty
                .set_text("Link your account first.");
            self.panel_approvals_empty
                .set_text("Link your account first.");
            // The page reads the panel's own state, so its cards say the same
            // thing: nothing can be pending before there is an account.
            crate::pages::approvals_cache(crate::pages::ApprovalsView {
                cards: Vec::new(),
                loaded: true,
                note: Some("Link your account first.".to_owned()),
            });
            return;
        };
        self.panel_approvals_empty.set_text("No pending approvals.");
        self.panel_decisions_empty.set_text("No recent decisions.");
        dispatch(
            self,
            move || {
                (
                    client.identity(),
                    client.approvals(),
                    client.approval_history(),
                )
            },
            move |chat, (identity, approvals, history)| {
                match identity {
                    Ok(id) => chat.paint_identity(Some(&id)),
                    Err(_) => chat.paint_identity(None),
                }
                match approvals {
                    Ok(list) => Self::paint_approvals(chat, &list),
                    // A failed read keeps the cards already held, and the page
                    // follows this store -- so the failure is named there too
                    // instead of reading as "nothing is waiting".
                    Err(_) => crate::pages::approvals_cache(crate::pages::ApprovalsView {
                        note: Some("The approvals read failed; check again.".to_owned()),
                        ..crate::pages::approvals_view()
                    }),
                }
                if let Ok(log) = history {
                    Self::paint_decisions(chat, &log);
                }
            },
        );
    }

    /// Policy settings, fresh on their own dispatch. Unlinked paints
    /// the link-first state; a failed read keeps the last good
    /// policy rather than blanking it.
    pub fn refresh_policy(self: &Rc<Self>) {
        let Some(client) = self.client.borrow().clone() else {
            Self::paint_policy(
                self,
                &muse_account::PolicySettings {
                    entries: Vec::new(),
                    connectors: Vec::new(),
                    withheld: 0,
                },
            );
            self.panel_policy_empty.set_text("Link your account first.");
            return;
        };
        self.panel_policy_empty.set_text("No policy settings.");
        dispatch(
            self,
            move || client.policy_settings(),
            move |chat, settings| {
                if let Ok(policy) = settings {
                    Self::paint_policy(chat, &policy);
                }
            },
        );
    }

    /// One policy flip: exactly one settings PATCH, then a re-read
    /// that proves what the press did. The editor itself is the
    /// undo: every word it writes can be written back.
    fn run_settings_update(
        chat: &Rc<Self>,
        connector_default: Option<String>,
        network_default: Option<String>,
        protocols: Vec<(String, String)>,
    ) -> Result<(), String> {
        // One settings change at a time, the panel's own slot: a page press
        // that arrives while another is in flight is refused, not queued.
        let Some(ticket) = chat.page_begin(Op::Panel) else {
            return Err("Another settings change is already in flight.".to_owned());
        };
        let Some(client) = chat.client_handle() else {
            chat.page_accept(&ticket);
            return Err("Link your account first.".to_owned());
        };
        let modes: std::collections::HashMap<String, String> = protocols.into_iter().collect();
        let change = if modes.is_empty() { None } else { Some(modes) };
        dispatch(
            chat,
            move || {
                client.settings_update(
                    connector_default.as_deref(),
                    network_default.as_deref(),
                    change,
                )
            },
            move |chat, answer| {
                chat.page_accept(&ticket);
                if answer.is_err() {
                    return;
                }
                chat.refresh_policy();
            },
        );
        Ok(())
    }

    /// One connector reset: the first press arms the row, the
    /// second press on the same row dispatches; the re-read proves
    /// the reset. A repaint disarms, which is the safe default.
    fn run_connector_reset(chat: &Rc<Self>, connector: &str) -> Result<(), String> {
        let Some(ticket) = chat.page_begin(Op::Panel) else {
            return Err("Another settings change is already in flight.".to_owned());
        };
        let Some(client) = chat.client_handle() else {
            chat.page_accept(&ticket);
            return Err("Link your account first.".to_owned());
        };
        let connector = connector.to_owned();
        dispatch(
            chat,
            move || client.connector_reset(&connector),
            move |chat, answer| {
                chat.page_accept(&ticket);
                if answer.is_err() {
                    return;
                }
                chat.refresh_policy();
            },
        );
        Ok(())
    }

    fn paint_identity(&self, identity: Option<&muse_account::AccountIdentity>) {
        match identity {
            Some(id) => {
                self.panel_identity_name
                    .set_text(if id.identity_name.is_empty() {
                        "(unnamed)"
                    } else {
                        &id.identity_name
                    });
                self.panel_identity_vm
                    .set_text(&format!("Device {}", id.vm_id));
            }
            None => {
                self.panel_identity_name
                    .set_text("Link your account first.");
                self.panel_identity_vm.set_text("");
            }
        }
    }

    fn paint_approvals(chat: &Rc<Self>, list: &muse_account::PendingApprovals) {
        // The page shows the same requests, projected from the same list, so
        // the two surfaces cannot offer different cards or outcome words.
        crate::pages::approvals_cache(crate::pages::ApprovalsView {
            cards: crate::pages::held_approvals_from(list),
            loaded: true,
            note: None,
        });
        while let Some(child) = chat.panel_approvals.first_child() {
            chat.panel_approvals.remove(&child);
        }
        chat.panel_approvals_empty
            .set_visible(list.pending.is_empty());
        for approval in &list.pending {
            let row = gtk4::ListBoxRow::new();
            row.set_selectable(false);
            let column = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
            let head = gtk4::Label::new(Some(&approval_headline(approval)));
            head.set_xalign(0.0);
            head.set_wrap(true);
            head.set_max_width_chars(28);
            column.append(&head);
            if !approval.summary.is_empty() {
                let summary = gtk4::Label::new(Some(&approval.summary));
                summary.set_xalign(0.0);
                summary.set_wrap(true);
                summary.set_lines(3);
                summary.set_max_width_chars(28);
                summary.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                summary.add_css_class("dim-label");
                column.append(&summary);
            }
            for detail in &approval.details {
                let line = gtk4::Label::new(Some(&format!("{}: {}", detail.label, detail.value)));
                line.set_xalign(0.0);
                line.set_wrap(true);
                line.set_max_width_chars(28);
                line.add_css_class("dim-label");
                column.append(&line);
            }
            let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
            for decision in muse_account::Decision::all() {
                let press = gtk4::Button::with_label(decision_label(decision));
                let weak = Rc::downgrade(chat);
                let approval_id = approval.approval_id.clone();
                press.connect_clicked(move |_| {
                    let Some(chat) = weak.upgrade() else { return };
                    // The panel has no line for this: a dropped press is
                    // repeatable, and the card stays until a refill says so.
                    let _ = run_decision(&chat, &approval_id, decision);
                });
                buttons.append(&press);
            }
            column.append(&buttons);
            row.set_child(Some(&column));
            chat.panel_approvals.append(&row);
        }
    }

    /// The decided past, read-only rows: what it asked for, the
    /// outcome and who decided it, plus the reason behind it. A
    /// failed read keeps the last good log rather than blanking
    /// it, like the pending list above.
    fn paint_decisions(chat: &Rc<Self>, log: &muse_account::DecisionHistory) {
        while let Some(child) = chat.panel_decisions.first_child() {
            chat.panel_decisions.remove(&child);
        }
        chat.panel_decisions_empty
            .set_visible(log.decisions.is_empty());
        for decided in &log.decisions {
            let row = gtk4::ListBoxRow::new();
            row.set_selectable(false);
            let column = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
            let title = decided
                .display_name
                .as_deref()
                .filter(|text| !text.is_empty())
                .or_else(|| {
                    decided
                        .action_label
                        .as_deref()
                        .filter(|text| !text.is_empty())
                })
                .unwrap_or("(decision)");
            let head = gtk4::Label::new(Some(title));
            head.set_xalign(0.0);
            head.set_wrap(true);
            head.set_max_width_chars(28);
            column.append(&head);
            let verdict = [decided.decision.as_deref(), decided.status.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            if !verdict.is_empty() {
                let line = gtk4::Label::new(Some(&verdict));
                line.set_xalign(0.0);
                line.set_wrap(true);
                line.set_max_width_chars(28);
                line.add_css_class("dim-label");
                column.append(&line);
            }
            if let Some(reason) = decided.decision_reason.as_deref()
                && !reason.is_empty()
            {
                let line = gtk4::Label::new(Some(reason));
                line.set_xalign(0.0);
                line.set_wrap(true);
                line.set_lines(2);
                line.set_max_width_chars(28);
                line.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                line.add_css_class("dim-label");
                column.append(&line);
            }
            row.set_child(Some(&column));
            chat.panel_decisions.append(&row);
        }
    }

    /// The durable policy, one row per projected entry. The two
    /// defaults and each protocol row carry the flip the service
    /// accepts; anything else is display only. Connector rows arm
    /// first: the bridge comment asks for one armed press, and a
    /// repaint disarms. A failed read keeps the last good policy.
    fn paint_policy(chat: &Rc<Self>, policy: &muse_account::PolicySettings) {
        // The settings page reads the same projection: the panel's own flip
        // table decides which rows the page can name as flippable, so a press
        // here and the words there never disagree.
        if crate::pages::settings_policy_cache(crate::pages::held_policy_from(
            policy,
            |label, value| Self::policy_flip(label, value).map(|flip| flip.next),
        )) {
            // The page's reads are answered from this cache, so a Settings
            // window that is already open is told the policy just landed; it
            // re-asks its own snapshot instead of polling for the answer.
            crate::webui::settings_policy_changed();
        }
        while let Some(child) = chat.panel_policy.first_child() {
            chat.panel_policy.remove(&child);
        }
        let actionable = policy
            .entries
            .iter()
            .filter(|entry| Self::policy_flip(&entry.label, &entry.value).is_some())
            .count()
            + policy.connectors.len();
        chat.panel_policy_empty.set_visible(actionable == 0);
        for entry in &policy.entries {
            let row = gtk4::ListBoxRow::new();
            row.set_selectable(false);
            let column = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
            let head = gtk4::Label::new(Some(&format!("{}: {}", entry.label, entry.value)));
            head.set_xalign(0.0);
            head.set_wrap(true);
            head.set_max_width_chars(28);
            column.append(&head);
            if let Some(flip) = Self::policy_flip(&entry.label, &entry.value) {
                let press = gtk4::Button::with_label(&format!("Set to {}", flip.next));
                let weak = Rc::downgrade(chat);
                let (label, value) = (entry.label.clone(), flip.next.clone());
                press.connect_clicked(move |_| {
                    let Some(chat) = weak.upgrade() else { return };
                    // The page's own flip press builds the same call from the
                    // same table; a refusal there is shown, and here the panel
                    // has no notice line to put it in.
                    let (connector_default, network_default, protocols) =
                        crate::pages::policy_update_args(&label, &value);
                    let _ = Self::run_settings_update(
                        &chat,
                        connector_default,
                        network_default,
                        protocols,
                    );
                });
                column.append(&press);
            }
            row.set_child(Some(&column));
            chat.panel_policy.append(&row);
        }
        for connector in &policy.connectors {
            let row = gtk4::ListBoxRow::new();
            row.set_selectable(false);
            let column = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
            let head = gtk4::Label::new(Some(connector));
            head.set_xalign(0.0);
            head.set_wrap(true);
            head.set_max_width_chars(28);
            column.append(&head);
            let press = gtk4::Button::with_label("Reset");
            press.add_css_class("muse-danger");
            let weak = Rc::downgrade(chat);
            let name = connector.clone();
            // Clones are handles to the same row: `armed`
            // rides into the closure while `arm` takes the signal.
            let arm = press.clone();
            let armed = arm.clone();
            arm.connect_clicked(move |_| {
                let Some(chat) = weak.upgrade() else { return };
                if armed.label().as_deref() != Some("Confirm reset") {
                    armed.set_label("Confirm reset");
                    return;
                }
                armed.set_label("Reset");
                // The panel has no notice line: a refusal here is silence,
                // and the re-read that follows a landed reset is the proof.
                let _ = Self::run_connector_reset(&chat, &name);
            });
            column.append(&press);
            row.set_child(Some(&column));
            chat.panel_policy.append(&row);
        }
    }

    /// The flip a policy row offers, if any. The table is `pages::policy_next`,
    /// which the Settings page reads too: the panel and the page cannot offer
    /// different flips.
    fn policy_flip(label: &str, value: &str) -> Option<PolicyFlip> {
        crate::pages::policy_next(label, value).map(|next| PolicyFlip {
            next: next.to_owned(),
        })
    }

    /// Fill the panel lists from a held snapshot: schedules with
    /// their next runs, then agent activity grouped the way the web
    /// groups it — Today, Yesterday, Earlier. Rows are text; the runs
    /// themselves stay on the Tasks page.
    fn paint_panel(&self, snapshot: &muse_account::TasksSnapshot) {
        while let Some(child) = self.panel_schedules.first_child() {
            self.panel_schedules.remove(&child);
        }
        let count = snapshot.schedules.len();
        self.panel_sched_head.set_text(&match count {
            1 => "1 scheduled task".to_owned(),
            _ => format!("{count} scheduled tasks"),
        });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|span| span.as_secs() as i64)
            .unwrap_or(0);
        for schedule in &snapshot.schedules {
            let line = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
            let title = gtk4::Label::new(Some(&schedule.title));
            title.set_xalign(0.0);
            title.set_wrap(true);
            title.set_max_width_chars(28);
            let next = gtk4::Label::new(Some(&super::pages::next_run_label(
                schedule.next_run_at_utc,
                now,
            )));
            next.set_xalign(0.0);
            next.add_css_class("dim-label");
            line.append(&title);
            line.append(&next);
            self.panel_schedules
                .append(&panel_row("alarm-symbolic", &line));
        }
        while let Some(child) = self.panel_today.first_child() {
            self.panel_today.remove(&child);
        }
        let today = local_today();
        let mut section: Option<&str> = None;
        for agent in &snapshot.agents {
            let day = agent
                .created_at
                .as_deref()
                .and_then(|stamp| stamp.split_once('T').map(|(date, _)| date));
            let bucket = activity_bucket(day, &today);
            if section != Some(bucket) {
                let head = gtk4::Label::new(Some(bucket));
                head.set_halign(gtk4::Align::Start);
                head.add_css_class("muse-panel-bucket");
                let head_row = gtk4::ListBoxRow::new();
                head_row.set_selectable(false);
                head_row.add_css_class("muse-panel-heading");
                head_row.set_child(Some(&head));
                self.panel_today.append(&head_row);
                section = Some(bucket);
            }
            let line = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
            let prompt = if agent.prompt.is_empty() {
                "(activity)".to_owned()
            } else {
                agent.prompt.clone()
            };
            let what = gtk4::Label::new(Some(&prompt));
            what.set_xalign(0.0);
            what.set_wrap(true);
            what.set_lines(2);
            what.set_max_width_chars(28);
            what.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            line.append(&what);
            if let Some(status) = agent.status.as_deref()
                && !status.is_empty()
            {
                let state = gtk4::Label::new(Some(status));
                state.set_xalign(0.0);
                state.add_css_class("dim-label");
                line.append(&state);
            }
            self.panel_today
                .append(&panel_row("system-run-symbolic", &line));
        }
    }
    /// True when the transcript shows its newest row: a repaint may
    /// follow to the bottom. Otherwise the reader is back-reading and
    /// the repaint holds still instead of yanking them down.
    fn at_bottom(&self) -> bool {
        let adjustment = self.scrolled.vadjustment();
        adjustment.value() + adjustment.page_size() >= adjustment.upper() - 2.0
    }

    /// A navigation click is an explicit request to read the newest turn,
    /// unlike a background repaint while the reader is back-scrolling.
    pub fn scroll_to_latest(self: &Rc<Self>) {
        *self.follow_after_render.borrow_mut() = true;
        self.defer_scroll_to_bottom();
    }

    fn defer_scroll_to_bottom(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        gtk4::glib::idle_add_local_once(move || {
            if let Some(chat) = weak.upgrade() {
                let adjustment = chat.scrolled.vadjustment();
                adjustment.set_value(
                    (adjustment.upper() - adjustment.page_size()).max(adjustment.lower()),
                );
            }
        });
    }

    /// Rebuild the transcript rows from the held history. The single
    /// place completions repaint, so option presses and attachments
    /// can never disagree with the rows. Follows to the newest row
    /// when the reader was already there; explicit navigation forces
    /// the new thread to the bottom after GTK updates the layout.
    fn render(self: &Rc<Self>) {
        let follow = self.follow_after_render.replace(false) || self.at_bottom();
        let fingerprint = self.session.borrow().history_fingerprint();
        // Unchanged history means identical rows: skip the full
        // teardown and rebuild. The labels below are cheap; the
        // ListBox reconstruction is what stalled long threads.
        if *self.painted.borrow() != Some(fingerprint.clone()) {
            // A reply that streams in keeps its id, its place and the
            // row count: only its words grow. Rewriting that one label
            // leaves every row above it -- and the newest turn's
            // live-region node -- exactly where they were, so a reader
            // hears the sentence change instead of a replacement. A
            // new row, a landing link card or a thread switch still
            // rebuilds the log.
            let grown = {
                let held = self.painted.borrow().clone();
                held.is_some_and(|previous| {
                    previous.0 == fingerprint.0
                        && previous.1 == fingerprint.1
                        && previous.2 == fingerprint.2
                        && previous.3 != fingerprint.3
                })
            };
            let newest_text = self
                .session
                .borrow()
                .history
                .last()
                .map(|row| row.text.clone());
            let rewritten = match newest_text {
                Some(text) if grown => set_newest_text(&self.transcript, &text),
                _ => false,
            };
            if !rewritten {
                let on_option = self.on_option.borrow().clone();
                if let Some(on_option) = on_option {
                    let session = self.session.borrow();
                    let rows = build_rows(&session.history);
                    let quotas: Vec<String> = rows
                        .iter()
                        .filter_map(|row| match row {
                            super::chat::TranscriptRow::Notice { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect();
                    let previews = session.previews.clone();
                    drop(session);
                    for text in quotas {
                        self.session.borrow_mut().note_quota(text);
                    }
                    let on_react = self
                        .on_react
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| std::rc::Rc::new(|_: String, _: String| {}));
                    let on_reply = self
                        .on_reply
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| std::rc::Rc::new(|_: String, _: String| {}));
                    populate(
                        &self.transcript,
                        &rows,
                        &on_option,
                        &on_react,
                        &on_reply,
                        &previews,
                    );
                }
            }
            *self.painted.borrow_mut() = Some(fingerprint);
        }
        // The web slice's copy follows this render: after a rebuild or an
        // in-place rewrite, the page and the list tell the same story.
        self.cache_web_chat();
        // Link cards resolve a render behind: the row shows text-only
        // first, the card version repaints when the fetch lands.
        self.fetch_previews();
        self.refresh_older();
        self.refresh_notice();
        self.refresh_title();
        self.refresh_delete_arm();
        if follow {
            self.defer_scroll_to_bottom();
        }
    }

    /// The notice line names the newest send outcome (paste refused,
    /// picture mismatch, send failed) while one stands; otherwise it
    /// shows the service's latest limit sentence, hidden when neither.
    fn refresh_notice(&self) {
        if let Some(note) = self.send_note.borrow().clone() {
            self.notice.set_text(&note);
            self.notice.set_visible(true);
            return;
        }
        match self.session.borrow().latest_quota.clone() {
            Some(text) => {
                self.notice.set_text(&text);
                self.notice.set_visible(true);
            }
            None => self.notice.set_visible(false),
        }
    }

    /// A new turn disarms a pending delete: the armed press must name
    /// the latest turn, never a stale one.
    fn refresh_delete_arm(&self) {
        let latest = self
            .session
            .borrow()
            .history
            .last()
            .map(|row| row.message_id.clone());
        let armed = self.armed_delete.borrow().clone();
        if armed.is_some() && armed != latest {
            *self.armed_delete.borrow_mut() = None;
        }
        let armed = self.armed_delete.borrow().clone();
        self.delete
            .set_label(delete_label(armed.is_some() && armed == latest));
    }

    /// The open thread's title from the service listing, or "New chat"
    /// before anything is picked. One place, so the heading and the web
    /// slice's copy of it cannot word the thread differently.
    pub(crate) fn thread_title(&self) -> String {
        let thread = self.thread.borrow().clone();
        thread
            .as_deref()
            .and_then(|id| {
                self.session.borrow().account.clone().and_then(|account| {
                    account
                        .threads
                        .iter()
                        .find(|row| row.session_id == id)
                        .map(|row| row.title.clone())
                })
            })
            .unwrap_or_else(|| "New chat".to_owned())
    }

    /// The heading names the open thread from the service listing, or
    /// "New chat" before anything is picked. Cheap enough to repaint
    /// on every render; the label only changes on a thread switch.
    fn refresh_title(&self) {
        self.heading.set_text(&self.thread_title());
    }

    /// Keep the web slice's copy of the transcript in step with the native
    /// list: the same rows, the same thread, the same status line. Runs on
    /// every render -- including the in-place rewrite of a growing reply --
    /// so a page following `chat.snapshot` sees the sentence arrive rather
    /// than a stale copy of its first chunk.
    pub(crate) fn cache_web_chat(&self) {
        let (rows, loaded, account, status) = {
            let session = self.session.borrow();
            (
                build_rows(&session.history),
                !session.history.is_empty() || self.thread.borrow().is_some(),
                self.session_name(),
                self.send_note
                    .borrow()
                    .clone()
                    .or_else(|| session.latest_quota.clone()),
            )
        };
        let title = self.thread_title();
        let thread = self.thread.borrow().clone();
        // The staged pictures travel as chips, never as bytes: the page can
        // ask for a paste and drop one, but the bytes and the send stay here.
        let (sending, staged) = {
            let composer = self.composer.borrow();
            (
                composer.sending(),
                composer
                    .staged()
                    .iter()
                    .map(|image| crate::pages::StagedChip {
                        name: image.name.clone(),
                        width: image.width,
                        height: image.height,
                        bytes: image.bytes.len(),
                    })
                    .collect(),
            )
        };
        let unread = self.thread_unread();
        crate::pages::chat_cache(crate::pages::HeldChat {
            thread,
            title,
            account: account.unwrap_or_else(|| "Sign in".to_owned()),
            status,
            sending,
            staged,
            unread,
            rows: crate::pages::held_chat_from(&rows),
            loaded,
            error: None,
            ..crate::pages::HeldChat::default()
        });
    }

    /// The open thread's unread count from the service listing: the number
    /// the native sidebar badges the row with. Zero when the thread is new or
    /// the listing has not read it -- never an invented count.
    fn thread_unread(&self) -> u64 {
        let thread = self.thread.borrow().clone();
        thread
            .as_deref()
            .and_then(|id| {
                self.session.borrow().account.as_ref().and_then(|account| {
                    account
                        .threads
                        .iter()
                        .find(|row| row.session_id == id)
                        .map(|row| row.unread_count)
                })
            })
            .unwrap_or(0)
    }

    /// The older button names its own state: loading, exhausted, or
    /// ready. Never a second dispatch while one is in flight.
    fn refresh_older(&self) {
        if *self.older_exhausted.borrow() {
            self.older_button.set_label("No older messages");
            self.older_button.set_sensitive(false);
        } else if self.session.borrow().is_busy() {
            self.older_button.set_label("Loading older\u{2026}");
            self.older_button.set_sensitive(false);
        } else {
            self.older_button.set_label("Load older");
            self.older_button
                .set_sensitive(self.client.borrow().is_some());
        }
    }

    /// The linked client, for pages that read on the shared page slot.
    pub fn client_handle(&self) -> Option<AccountClient> {
        self.client.borrow().clone()
    }
    /// The local host controller: folder grants, host session and
    /// approval cards. The Local host page renders its snapshot;
    /// presses go through it so every mutation keeps the invariants.
    pub(crate) fn host(&self) -> &RefCell<super::local_host::Controller> {
        &self.local_host
    }

    /// Take one stream's slot for a secondary read or a panel write.
    /// A second press on the same stream while its slot is held waits
    /// instead of stacking a duplicate dispatch. Unrelated streams
    /// proceed on their own tickets: the chat pump, every page fill
    /// and every panel write run beside each other.
    pub fn page_begin(&self, op: Op) -> Option<super::session::Ticket> {
        self.session.borrow_mut().try_begin(op)
    }

    /// Release a stream's slot for a completion carrying its own
    /// ticket. Only the matching stream releases; a late answer from
    /// a left page owns nothing.
    pub fn page_accept(&self, ticket: &super::session::Ticket) -> bool {
        self.session.borrow_mut().accept(ticket)
    }

    /// The open thread's scope for service calls, if any.
    fn thread_scope(&self) -> Option<String> {
        self.thread.borrow().clone()
    }

    /// The open thread's id, for the sidebar's History mark and the
    /// repeat-press guard. None before anything is picked.
    pub fn current_thread(&self) -> Option<String> {
        self.thread.borrow().clone()
    }

    /// The linked identity's display name, for the sidebar profile row.
    /// None before the account loads.
    pub fn session_name(&self) -> Option<String> {
        self.session.borrow().account.clone().map(|account| {
            if account.identity_name.is_empty() {
                "Muse".to_owned()
            } else {
                account.identity_name
            }
        })
    }

    /// The linked account's thread listing, for the threads page.
    pub fn threads(&self) -> Vec<muse_account::Thread> {
        self.session
            .borrow()
            .account
            .clone()
            .map(|account| account.threads)
            .unwrap_or_default()
    }

    /// Switch the open thread: the held rows clear (they belong to the
    /// old thread) and the new thread loads. Never mixes two threads'
    /// rows in one transcript.
    pub fn switch_thread(self: &Rc<Self>, thread: Option<String>) {
        *self.thread.borrow_mut() = thread;
        *self.follow_after_render.borrow_mut() = true;
        self.session.borrow_mut().history.clear();
        self.session.borrow_mut().latest_quota = None;
        *self.older_exhausted.borrow_mut() = false;
        *self.armed_delete.borrow_mut() = None;
        self.render();
        self.reload_history();
    }

    /// Link the saved account and load the transcript: connect proves
    /// the link, history fills the rows. Each step holds the ticket
    /// slot only while its own read is in flight.
    pub fn load(self: &Rc<Self>, python: &std::path::Path, config_dir: &std::path::Path) {
        let ticket = match self.session.borrow_mut().try_begin(Op::Connect) {
            Some(ticket) => ticket,
            None => return,
        };
        let client = AccountClient::new(muse_account::BridgeConfig {
            python: python.to_owned(),
            script: bridge_script("muse_account_bridge.py"),
            config_dir: config_dir.to_owned(),
        });
        *self.client.borrow_mut() = Some(client.clone());
        dispatch(
            self,
            move || client.connect(),
            move |chat, answer| {
                if chat.session.borrow_mut().finish_connect(&ticket, answer) {
                    chat.reload_history();
                    chat.load_model();
                    chat.start_live();
                }
            },
        );
    }

    /// Hold the conversation subscription open: each delivered event
    /// only wakes a history re-read (the rows stay authoritative --
    /// stream events carry no ids or roles to render from). The 8s poll
    /// remains the fallback; a dead worker just stops waking it. The
    /// loop ends with the chat: the worker holds a weak reference.
    fn start_live(self: &Rc<Self>) {
        let Some(client) = self.client.borrow().clone() else {
            return;
        };
        let Some(live) = self.live_tx.borrow().clone() else {
            return;
        };
        std::thread::spawn(move || {
            // One wake per delivered event, nothing otherwise: the loop
            // learns the page is gone from a failed wake and exits then.
            // Until the first event it idles here, which costs one thread
            // sleeping in 2s rounds for the life of the process.
            loop {
                // One wake per delivered event, nothing otherwise.
                if let Ok(events) = client.subscribe("chat.subscribe", serde_json::json!({}), 50)
                    && !events.is_empty()
                    && live.send(()).is_err()
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        });
    }

    /// One press dispatches once. The entry clears on dispatch; the
    /// reply reloads history, and a quota-limited send lands as the
    /// service's own notice row through the transcript builders.
    fn send_pressed(self: &Rc<Self>) {
        // Snapshot the quote before begin_send takes it: a send that
        // never dispatches hands the quote back like the pictures.
        let staged = self.composer.borrow().reply().cloned();
        let dispatch = match self.composer.borrow_mut().begin_send() {
            Some(dispatch) => dispatch,
            None => return,
        };
        self.composer_row.set_input_text("");
        // A fresh send clears yesterday's send note; the outcome of
        // this one writes its own. The quote clears with its send:
        // one Reply quotes one send.
        *self.send_note.borrow_mut() = None;
        self.paint_chips();
        self.paint_reply();
        self.composer_row.refresh(&self.composer.borrow());
        let _ = self.send_text(dispatch.text, dispatch.images, staged);
    }

    /// Stage the clipboard's picture as a chip. Returns whether the
    /// press was claimed: only when a picture is actually offered.
    /// Text clipboards are never touched.
    fn paste_from_clipboard(self: &Rc<Self>) -> bool {
        if self.composer.borrow().sending() {
            return false;
        }
        let clipboard = self.composer_row.input.clipboard();
        if !clipboard
            .formats()
            .contains_type(gtk4::gdk::Texture::static_type())
        {
            return false;
        }
        let weak = Rc::downgrade(self);
        gtk4::glib::spawn_future_local(async move {
            match clipboard.read_texture_future().await {
                Ok(Some(texture)) => {
                    if let Some(chat) = weak.upgrade() {
                        let bytes = texture.save_to_png_bytes().to_vec();
                        chat.stage_pasted(bytes, texture.width(), texture.height());
                    }
                }
                _ => {
                    if let Some(chat) = weak.upgrade() {
                        chat.say_send_note("The clipboard picture could not be read.");
                    }
                }
            }
        });
        true
    }

    /// The mic pill: first press starts a take, second press stops it
    /// and sends it for transcription. A take runs thirty seconds max,
    /// then stops itself the same way.
    fn toggle_dictation(self: &Rc<Self>) {
        if self.recording.borrow().is_some() {
            self.finish_dictation();
            return;
        }
        match super::voice::Recording::start() {
            Ok(recording) => {
                *self.recording.borrow_mut() = Some(recording);
                self.composer_row.mic.set_label("■");
                self.say_send_note("Listening — press again to stop.");
                let weak = Rc::downgrade(self);
                gtk4::glib::timeout_add_local(std::time::Duration::from_secs(30), move || {
                    if let Some(chat) = weak.upgrade()
                        && chat.recording.borrow().is_some()
                    {
                        chat.finish_dictation();
                    }
                    gtk4::glib::ControlFlow::Break
                });
            }
            Err(reason) => self.say_send_note(&format!("Dictation unavailable: {reason}.")),
        }
    }

    /// Stop the take and ask the service what it heard. Words land in
    /// the composer as an editable draft -- dictation proposes, never
    /// sends. No words, or a refusal, is reported in the same line.
    fn finish_dictation(self: &Rc<Self>) {
        let recording = self.recording.borrow_mut().take();
        self.composer_row.mic.set_label("🎤");
        let Some(recording) = recording else {
            return;
        };
        let (pcm, rate) = match recording.stop() {
            Ok(take) => take,
            Err(reason) => {
                self.say_send_note(&format!("Dictation dropped: {reason}."));
                return;
            }
        };
        self.transcribe(pcm, rate);
    }

    fn transcribe(self: &Rc<Self>, pcm: Vec<u8>, rate: u32) {
        let ticket = match self.session.borrow_mut().try_begin(Op::Voice) {
            Some(ticket) => ticket,
            None => {
                self.say_send_note("A dictation is already running.");
                return;
            }
        };
        let Some(client) = self.client.borrow().clone() else {
            self.session.borrow_mut().accept(&ticket);
            self.say_send_note("Sign in before dictating.");
            return;
        };
        self.say_send_note("Hearing the take back…");
        dispatch(
            self,
            move || client.dictate(&pcm, rate),
            move |chat, answer| {
                chat.session.borrow_mut().accept(&ticket);
                match answer {
                    Ok(verdict) => match verdict.transcript {
                        Some(words) if !words.trim().is_empty() => {
                            let draft = chat.composer.borrow().text().to_owned();
                            let joined = if draft.trim().is_empty() {
                                words.trim().to_owned()
                            } else {
                                format!("{} {}", draft.trim_end(), words.trim())
                            };
                            chat.composer.borrow_mut().set_text(&joined);
                            chat.composer_row.set_input_text(&joined);
                            chat.composer_row.refresh(&chat.composer.borrow());
                            *chat.send_note.borrow_mut() = None;
                            chat.refresh_notice();
                        }
                        _ => {
                            chat.say_send_note("The service took the audio but returned no words.")
                        }
                    },
                    Err(error) => {
                        chat.say_send_note(&format!("Dictation failed ({:?}).", error.code))
                    }
                }
            },
        );
    }

    /// One pasted picture lands as a chip above the composer. The
    /// error names the limit instead of eating the paste.
    fn stage_pasted(&self, bytes: Vec<u8>, width: i32, height: i32) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|span| span.as_nanos())
            .unwrap_or(0);
        let name = super::composer::staged_name(nanos, "image/png");
        let staged = match name {
            Some(name) => self.composer.borrow_mut().stage(name, bytes, width, height),
            None => Err("that picture format is not sendable"),
        };
        if let Err(limit) = staged {
            self.say_send_note(&format!("Picture not staged: {limit}."));
        }
        self.paint_chips();
        self.composer_row.refresh(&self.composer.borrow());
        // A paste lands asynchronously, after the render that opened the
        // window: publish the chip now, or the page would not see it until
        // the next unrelated render.
        self.cache_web_chat();
    }

    /// Repaint the staged picture chips from the composer state.
    fn paint_chips(&self) {
        if let Some(on_remove) = self.on_remove.borrow().clone() {
            self.composer_row
                .paint_chips(self.composer.borrow().staged(), &on_remove);
        }
    }

    /// A send outcome in the reader's own words. Wins over the quota
    /// line until the next send; cleared by a fresh press.
    fn say_send_note(&self, note: &str) {
        *self.send_note.borrow_mut() = Some(note.to_owned());
        self.refresh_notice();
    }

    /// Stage another page's text as an editable chat draft: Discuss
    /// proposes, never sends. The reader reviews and presses send.
    pub(crate) fn propose_draft(&self, text: &str) {
        self.composer.borrow_mut().set_text(text);
        self.composer_row.refresh(&self.composer.borrow());
    }

    /// Dispatch one message: the composer's draft, an option button's
    /// text, staged pictures, or text with pictures. One press
    /// dispatches once; a press while the slot is held releases the
    /// composer instead of queueing.
    fn send_text(
        self: &Rc<Self>,
        draft: String,
        images: Vec<StagedImage>,
        reply: Option<super::composer::ReplyQuote>,
    ) -> SendStart {
        let ticket = match self.session.borrow_mut().try_begin(Op::Send) {
            Some(ticket) => ticket,
            None => {
                self.composer.borrow_mut().finish_send();
                self.composer.borrow_mut().restore(images);
                self.paint_chips();
                self.restore_quote(reply.clone());
                self.composer_row.refresh(&self.composer.borrow());
                // A held slot is never silent: the reader sees the
                // message is kept and the log names the contention.
                eprintln!("muse: send pressed while the session slot is held; message kept");
                self.say_send_note("Still working — your message is kept; send again in a moment.");
                return SendStart::Refused(
                    "Still working — your message is kept; send again in a moment.",
                );
            }
        };
        let Some(client) = self.client.borrow().clone() else {
            self.session.borrow_mut().accept(&ticket);
            self.composer.borrow_mut().finish_send();
            self.composer.borrow_mut().restore(images);
            self.paint_chips();
            self.restore_quote(reply.clone());
            self.composer_row.refresh(&self.composer.borrow());
            return SendStart::Refused("Sign in before sending.");
        };
        let verified = self
            .session
            .borrow()
            .account
            .clone()
            .map(|account| (account.vm_id.clone(), account.identity_name.clone()));
        // Pictures without an identity have nowhere verified to go:
        // hand them back instead of dropping them down the plain path.
        if !images.is_empty() && verified.is_none() {
            self.session.borrow_mut().accept(&ticket);
            self.composer.borrow_mut().finish_send();
            self.composer.borrow_mut().restore(images);
            self.paint_chips();
            self.restore_quote(reply.clone());
            self.say_send_note("Sign in before sending pictures.");
            self.composer_row.refresh(&self.composer.borrow());
            return SendStart::Refused("Sign in before sending pictures.");
        }
        let scope = self.thread_scope();
        let reply_worker = reply.clone();
        // The staged quote's id is what the service records the reply against:
        // the excerpt never travels, and a plain send stays a plain send.
        let reply_target = reply.as_ref().map(|quote| quote.id.clone());
        dispatch(
            self,
            move || {
                let account_images: Vec<muse_account::StagedImage> = images
                    .iter()
                    .map(|image| muse_account::StagedImage {
                        name: image.name.clone(),
                        bytes: image.bytes.clone(),
                    })
                    .collect();
                let with_images = !account_images.is_empty();
                let answer = match verified {
                    Some((vm_id, identity)) if with_images => client.send_images_verified(
                        scope.as_deref(),
                        &draft,
                        &account_images,
                        &vm_id,
                        &identity,
                        reply_target.as_deref(),
                    ),
                    Some((vm_id, identity)) => client.send_verified(
                        scope.as_deref(),
                        &draft,
                        &vm_id,
                        &identity,
                        reply_target.as_deref(),
                    ),
                    None => client.send(scope.as_deref(), &draft, reply_target.as_deref()),
                };
                // The pictures ride back out with the answer: a failed
                // send restores exactly what it took, dims and all.
                // The quote rides the same way.
                (answer, images, with_images, reply_worker)
            },
            move |chat, (answer, images, with_images, reply)| {
                chat.session.borrow_mut().accept(&ticket);
                chat.composer.borrow_mut().finish_send();
                chat.composer_row.refresh(&chat.composer.borrow());
                if answer.is_err() {
                    chat.restore_quote(reply);
                }
                match answer {
                    Ok(receipt) => {
                        // The verified send checks the landed row's
                        // attachments byte-for-byte: a mismatch names
                        // itself instead of claiming success.
                        if receipt.stored_mismatch {
                            chat.say_send_note(
                                "Sent, but the service stored a different picture \
                                 than the one pasted. The text landed; \
                                 re-paste the picture to retry it.",
                            );
                        }
                        // Prove by re-read: the reply (or the limit notice)
                        // comes from the service, never from the dispatch.
                        // A short quick-poll burst (5 x 2s) watches the reply
                        // land; the 8s poll resumes after. Each round no-ops
                        // while the slot is held.
                        let weak = Rc::downgrade(chat);
                        let rounds: std::rc::Rc<std::cell::Cell<u8>> =
                            std::rc::Rc::new(std::cell::Cell::new(5));
                        gtk4::glib::timeout_add_local(
                            std::time::Duration::from_secs(2),
                            move || {
                                let left = rounds.get();
                                rounds.set(left.saturating_sub(1));
                                if let Some(chat) = weak.upgrade() {
                                    chat.reload_history();
                                }
                                if left > 1 {
                                    gtk4::glib::ControlFlow::Continue
                                } else {
                                    gtk4::glib::ControlFlow::Break
                                }
                            },
                        );
                        chat.reload_history();
                    }
                    Err(error) => {
                        // A failed picture send hands the pictures back:
                        // the paste survives to retry.
                        if with_images {
                            chat.composer.borrow_mut().restore(images);
                            chat.paint_chips();
                            chat.composer_row.refresh(&chat.composer.borrow());
                        }
                        chat.say_send_note(&format!(
                            "Send failed ({:?}{}).",
                            error.code,
                            if error.uncertain {
                                "; it may or may not have landed"
                            } else {
                                ""
                            }
                        ));
                    }
                }
            },
        );
        // The flag the card swaps Send for Stop on is only true for the length
        // of the dispatch, and a mirror page reads it from this cache -- so it
        // is published the moment the send goes out rather than at the next
        // render, or a page watching at 300ms would never see it at all.
        self.cache_web_chat();
        SendStart::Dispatched
    }

    /// A send from the web mirror. The page owns the draft, but the dispatch
    /// is the native one -- the same `Send` slot, the same receipt closure,
    /// the same history reload -- so there is exactly one sender in the app.
    /// The answer says whether the app took the press; what the service said
    /// lands in the transcript and the status line the page already follows.
    /// `quote` is the page's Reply: the turn it answers and the line it
    /// showed. It goes through the same guard the composer's own Reply uses,
    /// and then rides `send_text` as the native quote -- the page does not get
    /// a second notion of what a quote is, or a second way to dispatch one.
    pub(crate) fn web_send(
        self: &Rc<Self>,
        text: &str,
        quote: Option<crate::webui::WebQuote>,
    ) -> Result<(), String> {
        let text = text.trim();
        let reply = quote
            .map(|quote| super::composer::staged_quote(quote.id, quote.excerpt))
            .transpose()
            .map_err(|_| "That message cannot be quoted.".to_owned())?;
        // The page's words, the app's staged pictures, or both: a picture
        // alone sends here exactly as it does from the card. The press is
        // armed on the app's own composer, so Stop shows for it too, and the
        // entry's own draft is left where the reader typed it.
        let (armed, sending) = {
            let mut composer = self.composer.borrow_mut();
            let armed = composer.begin_remote(text);
            (armed, composer.sending())
        };
        let Some(dispatch) = armed else {
            if sending {
                return Err(
                    "Still working — your message is kept; send again in a moment.".to_owned(),
                );
            }
            return Err("Type a message before sending.".to_owned());
        };
        // Same as a press here: yesterday's note belongs to the last send.
        *self.send_note.borrow_mut() = None;
        self.paint_chips();
        self.paint_reply();
        self.composer_row.refresh(&self.composer.borrow());
        match self.send_text(dispatch.text, dispatch.images, reply) {
            SendStart::Dispatched => Ok(()),
            SendStart::Refused(sentence) => {
                self.refresh_notice();
                Err(sentence.to_owned())
            }
        }
    }

    /// Stage the clipboard's picture at the page's request: the app's own `+`
    /// press. The page cannot read the clipboard and never carries bytes; it
    /// asks here, then reads the chip back from `chat.snapshot`.
    pub(crate) fn web_attach(self: &Rc<Self>) -> Result<(), String> {
        // The native `+` is inert while a send is in flight (a paste would
        // race the pictures that send already took), and a press that reaches
        // here anyway hears that reason instead of being told the clipboard is
        // empty.
        if self.composer.borrow().sending() {
            return Err(
                "Still working — a picture can be staged when the send finishes.".to_owned(),
            );
        }
        if self.paste_from_clipboard() {
            Ok(())
        } else {
            Err("The clipboard holds no picture to stage.".to_owned())
        }
    }

    /// Drop one staged picture by name: the chip's × on the page, through the
    /// app's own staging state so a press in either window sees the same chips.
    pub(crate) fn web_unstage(self: &Rc<Self>, name: &str) -> Result<(), String> {
        self.composer.borrow_mut().unstage(name);
        self.paint_chips();
        self.composer_row.refresh(&self.composer.borrow());
        self.cache_web_chat();
        Ok(())
    }

    /// Re-read the pending approvals, the panel's own "Check approvals" path.
    /// One dispatch, one repaint; the page follows the rows that read painted.
    pub(crate) fn web_approvals_refresh(self: &Rc<Self>) -> Result<(), String> {
        self.refresh_identity_and_approvals();
        Ok(())
    }

    /// Decide one pending approval from the page's card. The word is checked
    /// against the service's own five before any request, and the press goes
    /// through `run_decision` -- one decide path, one refill that proves it.
    pub(crate) fn web_decide(self: &Rc<Self>, approval_id: &str, word: &str) -> Result<(), String> {
        if approval_id.is_empty() {
            return Err("That approval has no id.".to_owned());
        }
        let Some(decision) = muse_account::Decision::all()
            .into_iter()
            .find(|decision| decision.as_str() == word)
        else {
            return Err("Unknown decision.".to_owned());
        };
        // A held panel slot is the page's to report: the press did not go
        // out, so answering ok would make the card look decided.
        run_decision(self, approval_id, decision)
    }

    /// The Chat window's ways back into this controller: one closure per
    /// control, so the page cannot reach the session slot, the composer or the
    /// service by any other path.
    pub(crate) fn web_routes(self: &Rc<Self>) -> crate::webui::WebRoutes {
        let weak = Rc::downgrade(self);
        let send_weak = weak.clone();
        let attach_weak = weak.clone();
        let unstage_weak = weak.clone();
        let approvals_weak = weak.clone();
        let decide_weak = weak.clone();
        let history_weak = weak.clone();
        let open_thread_weak = weak.clone();
        let policy_weak = weak.clone();
        let reset_weak = weak.clone();
        let grant_weak = weak.clone();
        let start_weak = weak.clone();
        let host_stop_weak = weak.clone();
        crate::webui::WebRoutes {
            send: std::rc::Rc::new(move |text: &str, quote| match send_weak.upgrade() {
                Some(chat) => chat.web_send(text, quote),
                None => Err("The app window is gone.".to_owned()),
            }),
            stop: std::rc::Rc::new(move || match weak.upgrade() {
                Some(chat) => chat.stop_pressed(),
                None => Err("The app window is gone.".to_owned()),
            }),
            attach: std::rc::Rc::new(move || match attach_weak.upgrade() {
                Some(chat) => chat.web_attach(),
                None => Err("The app window is gone.".to_owned()),
            }),
            unstage: std::rc::Rc::new(move |name: &str| match unstage_weak.upgrade() {
                Some(chat) => chat.web_unstage(name),
                None => Err("The app window is gone.".to_owned()),
            }),
            approvals: std::rc::Rc::new(move || match approvals_weak.upgrade() {
                Some(chat) => chat.web_approvals_refresh(),
                None => Err("The app window is gone.".to_owned()),
            }),
            decide: std::rc::Rc::new(move |id: &str, word: &str| match decide_weak.upgrade() {
                Some(chat) => chat.web_decide(id, word),
                None => Err("The app window is gone.".to_owned()),
            }),
            history: std::rc::Rc::new(move || match history_weak.upgrade() {
                Some(chat) => Ok(chat.web_history()),
                None => Err("The app window is gone.".to_owned()),
            }),
            open_thread: std::rc::Rc::new(move |id: Option<&str>| {
                match open_thread_weak.upgrade() {
                    Some(chat) => chat.web_open_thread(id),
                    None => Err("The app window is gone.".to_owned()),
                }
            }),
            set_policy: std::rc::Rc::new(move |label: &str, value: &str| {
                match policy_weak.upgrade() {
                    Some(chat) => chat.web_policy_set(label, value),
                    None => Err("The app window is gone.".to_owned()),
                }
            }),
            reset_connector: std::rc::Rc::new(move |connector: &str| match reset_weak.upgrade() {
                Some(chat) => chat.web_connector_reset(connector),
                None => Err("The app window is gone.".to_owned()),
            }),
            host_grant: std::rc::Rc::new(move |consent: bool, writable: bool| {
                match grant_weak.upgrade() {
                    Some(chat) => chat.web_host_grant(consent, writable),
                    None => Err("The app window is gone.".to_owned()),
                }
            }),
            host_start: std::rc::Rc::new(move |consent: bool| match start_weak.upgrade() {
                Some(chat) => chat.web_host_start(consent),
                None => Err("The app window is gone.".to_owned()),
            }),
            host_stop: std::rc::Rc::new(move || match host_stop_weak.upgrade() {
                Some(chat) => chat.web_host_stop(),
                None => Err("The app window is gone.".to_owned()),
            }),
        }
    }

    /// The drawer's History rows, in the native sidebar's order: pinned first,
    /// then the listing, with the open thread marked.
    fn web_history(&self) -> Vec<crate::pages::HistoryRow> {
        let mut threads = self.threads();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.pinned));
        let current = self.current_thread();
        threads
            .iter()
            .map(|thread| crate::pages::HistoryRow {
                id: thread.session_id.clone(),
                text: crate::pages::history_text(thread),
                current: current.as_deref() == Some(thread.session_id.as_str()),
            })
            .collect()
    }

    /// Open a thread from the drawer's History -- or, with no id, a fresh one.
    /// The native activation's own call, without the page stack the shell does
    /// not have.
    fn web_open_thread(self: &Rc<Self>, id: Option<&str>) -> Result<(), String> {
        let id = id.map(str::to_owned);
        if self.current_thread() != id {
            self.switch_thread(id);
        }
        Ok(())
    }

    /// The Settings page's flip press: the shared table decides what the row
    /// may become, and the app's own update call carries it, so the page gains
    /// no route to the service that the panel does not already have.
    fn web_policy_set(self: &Rc<Self>, label: &str, value: &str) -> Result<(), String> {
        let Some(next) = crate::pages::policy_next(label, value) else {
            return Err(format!("{label} at {value} has no other setting."));
        };
        let (connector_default, network_default, protocols) =
            crate::pages::policy_update_args(label, next);
        Self::run_settings_update(self, connector_default, network_default, protocols)
    }

    /// The Settings page's reset press. The page holds the arming; the write
    /// is the panel's own.
    fn web_connector_reset(self: &Rc<Self>, connector: &str) -> Result<(), String> {
        if connector.trim().is_empty() {
            return Err("Name the connector to reset.".to_owned());
        }
        Self::run_connector_reset(self, connector)
    }

    /// The Local host page's grant press. The chooser stays the app's own
    /// native dialog -- a page cannot browse the disk, and the folder is the
    /// person's choice -- so this answers as soon as the dialog is up; the
    /// grant lands in the page's next snapshot.
    fn web_host_grant(self: &Rc<Self>, consent: bool, writable: bool) -> Result<(), String> {
        let parent = gtk4::prelude::WidgetExt::root(self.panel_policy.upcast_ref::<gtk4::Widget>())
            .and_then(|root| root.downcast::<gtk4::Window>().ok());
        let dialog = gtk4::FileChooserNative::new(
            Some("Grant a folder to Muse"),
            parent.as_ref(),
            gtk4::FileChooserAction::SelectFolder,
            Some("Grant"),
            Some("Cancel"),
        );
        dialog.set_select_multiple(false);
        dialog.set_modal(true);
        let weak = Rc::downgrade(self);
        dialog.connect_response(move |dialog, response| {
            if response != gtk4::ResponseType::Accept {
                return;
            }
            let Some(path) = dialog
                .files()
                .item(0)
                .and_then(|file| file.downcast::<gtk4::gio::File>().ok())
                .and_then(|file| file.path())
            else {
                return;
            };
            let Some(chat) = weak.upgrade() else {
                return;
            };
            let granted = chat.host().borrow_mut().grant(path, consent, writable);
            if let Err(refusal) = granted {
                // A dialog's answer has no reply ticket: the refusal goes into
                // the host's own notice line, where every page reads it.
                chat.host().borrow_mut().note(refusal);
            }
            chat.host().borrow_mut().poll();
        });
        dialog.show();
        Ok(())
    }

    /// Start the host from a page. The consent it carries is the page's own
    /// checked box; the host refuses without one, exactly as it does for the
    /// native press.
    fn web_host_start(self: &Rc<Self>, consent: bool) -> Result<(), String> {
        let Some(client) = self.client_handle() else {
            return Err("Link your account first.".to_owned());
        };
        let config = client.bridge_config();
        self.host()
            .borrow_mut()
            .start(Some(config), consent)
            .map_err(str::to_owned)?;
        self.host().borrow_mut().poll();
        Ok(())
    }

    /// Stop the host and revoke its grants, the native stop's own call.
    fn web_host_stop(self: &Rc<Self>) -> Result<(), String> {
        self.host().borrow_mut().stop();
        self.host().borrow_mut().poll();
        Ok(())
    }

    /// Armed two-press turn delete. The first press arms the latest
    /// turn, the second press on the same turn dispatches; the re-read
    /// proves the removal. Acceptance is not absence.
    fn delete_pressed(self: &Rc<Self>) {
        let latest = self
            .session
            .borrow()
            .history
            .last()
            .map(|row| row.message_id.clone());
        let armed = self.armed_delete.borrow().clone();
        match delete_action(latest.as_deref(), armed.as_deref()) {
            DeleteAction::Nothing => {}
            DeleteAction::Arm(id) => {
                *self.armed_delete.borrow_mut() = Some(id);
                self.refresh_delete_arm();
            }
            DeleteAction::Confirm(id) => {
                *self.armed_delete.borrow_mut() = None;
                self.delete.set_label(delete_label(false));
                let ticket = match self.session.borrow_mut().try_begin(Op::Send) {
                    Some(ticket) => ticket,
                    None => return,
                };
                let Some(client) = self.client.borrow().clone() else {
                    self.session.borrow_mut().accept(&ticket);
                    return;
                };
                dispatch(
                    self,
                    move || client.chat_delete("turn", &id),
                    move |chat, answer| {
                        chat.session.borrow_mut().accept(&ticket);
                        if answer.map(|answer| answer.accepted).unwrap_or(false) {
                            // Prove by re-read: only the refreshed
                            // history shows the turn is gone.
                            chat.reload_history();
                        }
                    },
                );
            }
        }
    }

    /// Stop cancels outside the ticket slot: it must fire while a
    /// send holds it, so no session slot is taken here at all. The answer
    /// names only what the service said it did -- a press with nothing
    /// running is not a stop -- and the next history read shows whatever
    /// actually landed. Shared by the app's own card and the web mirror, so
    /// there is one stop path as well as one sender.
    pub(crate) fn stop_pressed(self: &Rc<Self>) -> Result<(), String> {
        let Some(client) = self.client.borrow().clone() else {
            return Err("Sign in before stopping.".to_owned());
        };
        dispatch(
            self,
            move || client.cancel(),
            move |chat, answer| match answer {
                Ok(verdict) if verdict.cancelled => chat.say_send_note("Stopped."),
                Ok(_) => chat.say_send_note("Nothing was running."),
                Err(_) => chat.say_send_note("The stop did not reach the service."),
            },
        );
        Ok(())
    }

    /// Toggle one heart on one message row: one press, one write.
    /// The answer names the new state, so the note reports recorded
    /// vs removed instead of guessing; the row itself carries no
    /// reaction display to go stale.
    fn react_message(self: &Rc<Self>, message_id: String, emoji: String) {
        let Some(client) = self.client.borrow().clone() else {
            self.say_send_note("Link your account first.");
            return;
        };
        dispatch(
            self,
            move || client.message_react(&message_id, &emoji),
            move |chat, answer| match answer {
                Ok(ok) if ok.accepted && ok.reacted => {
                    chat.say_send_note("❤ recorded.");
                }
                Ok(ok) if ok.accepted => {
                    chat.say_send_note("❤ removed.");
                }
                _ => {
                    chat.say_send_note("The service did not record it.");
                }
            },
        );
    }

    /// Stage a reply quote from a row press: the row's id plus one
    /// capped line above the composer. Reversible — Cancel × drops
    /// it, and the send below stays a plain send (the community
    /// gateway records no reply param, so nothing new rides the wire).
    /// Visible crate-wide for the GUI regression test: the Reply
    /// button reaches it through the row callback.
    pub(crate) fn stage_reply(self: &Rc<Self>, message_id: String, text: String) {
        // The staging borrow ends with this statement: paint_reply
        // and refresh borrow the same composer, so holding the
        // borrow across the match below panics every press.
        let staged = self.composer.borrow_mut().stage_reply(message_id, text);
        match staged {
            Ok(()) => {
                self.paint_reply();
                self.composer_row.refresh(&self.composer.borrow());
            }
            Err(_) => self.say_send_note("That message cannot be quoted."),
        }
    }

    /// Put a quote back after a send that never went out. A held slot, a
    /// signed-out window and a failed dispatch all hand it back here, so the
    /// reader's context is never eaten and the handing-back lives in one
    /// place.
    fn restore_quote(&self, quote: Option<super::composer::ReplyQuote>) {
        if let Some(quote) = quote {
            let _ = self
                .composer
                .borrow_mut()
                .stage_reply(quote.id, quote.excerpt);
            self.paint_reply();
        }
    }

    /// Repaint the staged reply quote from the composer state.
    fn paint_reply(&self) {
        let composer = self.composer.borrow();
        self.composer_row.paint_reply(composer.reply());
    }

    /// Page one older window past what is loaded, anchored at the
    /// oldest held row. The window merges ahead of the transcript; the
    /// reader's place is left alone.
    fn load_older(self: &Rc<Self>) {
        let anchor = match self
            .session
            .borrow()
            .history
            .iter()
            .map(|row| row.seq)
            .min()
        {
            Some(anchor) => anchor,
            None => return,
        };
        let ticket = match self.session.borrow_mut().try_begin(Op::History) {
            Some(ticket) => ticket,
            None => return,
        };
        let Some(client) = self.client.borrow().clone() else {
            self.session.borrow_mut().accept(&ticket);
            return;
        };
        self.refresh_older();
        let scope = self.thread_scope();
        dispatch(
            self,
            move || client.history_window(scope.as_deref(), anchor, 20, 1),
            move |chat, answer| match answer {
                Ok(window) => {
                    if chat.session.borrow_mut().accept(&ticket) {
                        // Hold the reader's place: rows arrive above the
                        // viewport, so shift down by exactly the growth.
                        let adjustment = chat.scrolled.vadjustment();
                        let before = adjustment.upper();
                        *chat.older_exhausted.borrow_mut() = !window.has_older;
                        chat.session.borrow_mut().prepend_history(window.messages);
                        chat.render();
                        let grown = chat.scrolled.vadjustment().upper() - before;
                        if grown > 0.0 {
                            let adjustment = chat.scrolled.vadjustment();
                            adjustment.set_value(adjustment.value() + grown);
                        }
                    }
                }
                Err(_) => {
                    chat.session.borrow_mut().accept(&ticket);
                    chat.refresh_older();
                }
            },
        );
    }

    fn reload_history(self: &Rc<Self>) {
        let ticket = match self.session.borrow_mut().try_begin(Op::History) {
            Some(ticket) => ticket,
            None => return,
        };
        let Some(client) = self.client.borrow().clone() else {
            self.session.borrow_mut().accept(&ticket);
            return;
        };
        let scope = self.thread_scope();
        dispatch(
            self,
            move || client.history(scope.as_deref(), 32),
            move |chat, answer| {
                if chat.session.borrow_mut().finish_history(&ticket, answer) {
                    chat.render();
                }
            },
        );
    }
}

/// Decide exactly one approval: the Page ticket makes a double press
/// wait, and the approvals refill proves what the press did instead
/// of the answer alone. The error is the press that never left this
/// process -- a held panel slot, or no account to decide on -- so a
/// second surface can say that instead of reporting a decision it did
/// not make.
fn run_decision(
    chat: &Rc<Chat>,
    approval_id: &str,
    decision: muse_account::Decision,
) -> Result<(), String> {
    let Some(ticket) = chat.page_begin(Op::Panel) else {
        return Err(
            "The approvals panel is still finishing the last press; try again in a moment."
                .to_owned(),
        );
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return Err("Sign in before deciding approvals.".to_owned());
    };
    let approval_id = approval_id.to_owned();
    dispatch(
        chat,
        move || client.decide(&approval_id, decision),
        move |chat, answer| {
            chat.page_accept(&ticket);
            if answer.is_err() {
                return;
            }
            chat.refresh_identity_and_approvals();
        },
    );
    Ok(())
}

#[cfg(test)]
pub(super) fn verify_chat_scroll_navigation() {
    let transcript = gtk4::ListBox::new();
    let chat = Chat::build(transcript, ComposerRow::build());
    let window = gtk4::Window::new();
    window.set_default_size(500, 320);
    window.set_child(Some(&chat.scrolled));
    window.present();

    let history = |prefix: &str| {
        (0..80)
            .map(|index| muse_account::HistoryMessage {
                message_id: format!("{prefix}-{index}"),
                seq: index,
                role: if index % 2 == 0 { "user" } else { "assistant" }.to_owned(),
                text: format!(
                    "{prefix} message {index}: {}",
                    "long transcript line ".repeat(8)
                ),
                timestamp: Some("2026-11-19T12:00:00Z".to_owned()),
                status: None,
                options: Vec::new(),
                selected_option: None,
                attachments: Vec::new(),
                reply_to_message_id: None,
                reply_to_text: None,
            })
            .collect::<Vec<_>>()
    };
    chat.session.borrow_mut().history = history("old");
    chat.render();
    pump_gtk(150);
    let adjustment = chat.scrolled.vadjustment();
    assert!(adjustment.upper() > adjustment.page_size() + 100.0);
    adjustment.set_value(adjustment.lower());
    pump_gtk(50);
    let ctx = crate::pages::Ctx {
        chat: Rc::downgrade(&chat),
        stack: gtk4::Stack::new(),
        sidebar: gtk4::ListBox::new(),
        history: gtk4::ListBox::new(),
        profile_name: gtk4::Label::new(None),
        rail: Vec::new(),
        show_signin: Rc::new(|| {}),
        updates: Rc::new(RefCell::new(crate::update::UpdateCheck::default())),
    };
    ctx.stack.add_named(
        &gtk4::Box::new(gtk4::Orientation::Vertical, 0),
        Some("chat"),
    );
    crate::pages::show(&ctx, "chat");
    pump_gtk(50);
    assert!(
        adjustment.value() + adjustment.page_size() >= adjustment.upper() - 2.0,
        "main chat navigation should show newest message",
    );
    adjustment.set_value(adjustment.lower());
    pump_gtk(50);

    // A fast history response can replace the cleared list before GTK
    // updates its old scroll bounds; this matches the switch/render path.
    chat.switch_thread(Some("next-thread".to_owned()));
    chat.session.borrow_mut().history = history("new");
    chat.render();
    pump_gtk(150);

    assert!(
        adjustment.value() + adjustment.page_size() >= adjustment.upper() - 2.0,
        "thread switch should show newest message: value={}, page={}, upper={}",
        adjustment.value(),
        adjustment.page_size(),
        adjustment.upper(),
    );

    // A decision press the panel cannot take says so instead of reporting a
    // decision: the page's card must never look decided on a dropped press.
    let held = chat.page_begin(Op::Panel).expect("panel slot is free");
    let refused = run_decision(&chat, "a-1", muse_account::Decision::Deny);
    match refused {
        Err(sentence) => assert!(
            sentence.contains("still finishing"),
            "the busy sentence is the app's own: {sentence}"
        ),
        Ok(()) => panic!("a held panel slot reported a decision"),
    }
    chat.page_accept(&held);
    // The slot is free again, but this window has no account: that press is
    // refused too, rather than sent to a service nobody is signed in to.
    assert_eq!(
        run_decision(&chat, "a-1", muse_account::Decision::Deny),
        Err("Sign in before deciding approvals.".to_owned())
    );
    window.close();
}

#[cfg(test)]
pub(crate) fn pump_gtk(milliseconds: u64) {
    let context = gtk4::glib::MainContext::default();
    let until = std::time::Instant::now() + std::time::Duration::from_millis(milliseconds);
    while std::time::Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_groups_read_today_yesterday_earlier() {
        let today = "2026-11-17";
        assert_eq!(activity_bucket(Some("2026-11-17"), today), "Today");
        assert_eq!(activity_bucket(Some("2026-11-16"), today), "Yesterday");
        assert_eq!(activity_bucket(Some("2026-11-10"), today), "Earlier");
        assert_eq!(activity_bucket(Some("2026-11-18"), today), "Today");
        assert_eq!(activity_bucket(None, today), "Earlier");
        assert_eq!(activity_bucket(Some("not a date"), today), "Earlier");
        // Month and year boundaries are real calendar math, not string
        // prefixes: Nov 1's yesterday is Oct 31, Jan 1's is Dec 31.
        assert_eq!(
            activity_bucket(Some("2026-10-31"), "2026-11-01"),
            "Yesterday"
        );
        assert_eq!(
            activity_bucket(Some("2025-12-31"), "2026-01-01"),
            "Yesterday"
        );
    }

    #[test]
    fn delete_button_names_its_armed_state() {
        assert_eq!(delete_label(false), "Delete turn");
        assert_eq!(delete_label(true), "Confirm delete turn");
    }

    #[test]
    fn panel_tabs_match_the_web() {
        assert_eq!(
            PANEL_TABS.map(|(name, _)| name),
            ["Activity", "Approvals", "Identity", "Mail"]
        );
        assert!(
            PANEL_TABS
                .iter()
                .all(|(_, icon)| icon.ends_with("-symbolic"))
        );
    }

    #[test]
    fn decision_labels_speak_the_reader_language() {
        use muse_account::Decision;
        assert_eq!(Decision::all().len(), 5);
        assert_eq!(decision_label(Decision::AllowOnce), "Allow once");
        assert_eq!(decision_label(Decision::AllowTask), "Allow task");
        assert_eq!(decision_label(Decision::AllowSite), "Allow site");
        assert_eq!(decision_label(Decision::AllowAlways), "Allow always");
        assert_eq!(decision_label(Decision::Deny), "Deny");
    }

    #[test]
    fn approval_headline_keeps_title_and_service_words() {
        use muse_account::{Approval, ApprovalDetail};
        let plain = Approval {
            approval_id: "a".to_owned(),
            kind: None,
            decision: None,
            always_scope: None,
            attention_mode: None,
            title: "Run tests".to_owned(),
            summary: String::new(),
            details: Vec::new(),
        };
        assert_eq!(approval_headline(&plain), "Run tests");
        let tagged = Approval {
            kind: Some("shell".to_owned()),
            decision: Some("ask".to_owned()),
            always_scope: None,
            attention_mode: Some("loud".to_owned()),
            ..plain.clone()
        };
        assert_eq!(approval_headline(&tagged), "Run tests (shell, ask, loud)");
        let untitled = Approval {
            title: String::new(),
            ..plain.clone()
        };
        assert_eq!(approval_headline(&untitled), "(approval)");
        let _ = ApprovalDetail {
            label: String::new(),
            value: String::new(),
        };
    }

    #[test]
    fn delete_arms_first_confirms_same_and_ignores_the_rest() {
        assert_eq!(delete_action(None, None), DeleteAction::Nothing);
        assert_eq!(delete_action(None, Some("a")), DeleteAction::Nothing);
        assert_eq!(
            delete_action(Some("a"), None),
            DeleteAction::Arm("a".to_owned())
        );
        assert_eq!(
            delete_action(Some("a"), Some("a")),
            DeleteAction::Confirm("a".to_owned())
        );
        // A moved-on transcript re-arms instead of confirming stale.
        assert_eq!(
            delete_action(Some("b"), Some("a")),
            DeleteAction::Arm("b".to_owned())
        );
    }
}
