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

use adw::prelude::*;
use muse_account::AccountClient;

use super::chat::build_rows;
use super::chat_view::populate;
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

/// The web's right-panel tabs, in the web's order. Mail has no client
/// route yet; its tab owns the honest note, never a fake list.
pub const PANEL_TABS: [&str; 4] = ["Activity", "Approvals", "Identity", "Mail"];

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
fn activity_bucket(day: Option<&str>, today: &str) -> &'static str {
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
fn local_today() -> String {
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
    notice: gtk4::Label,
    send_note: RefCell<Option<String>>,
    armed_delete: RefCell<Option<String>>,
    painted: RefCell<Option<(usize, Option<String>, u64)>>,
    thread: RefCell<Option<String>>,
    model_combo: gtk4::ComboBoxText,
    recording: RefCell<Option<super::voice::Recording>>,
    preview_fetch: RefCell<bool>,
    panel: gtk4::Box,
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
    panel_placed: RefCell<bool>,
}

impl Chat {
    pub fn build(transcript: gtk4::ListBox, composer_row: ComposerRow) -> Rc<Self> {
        let scrolled = gtk4::ScrolledWindow::new();
        scrolled.set_child(Some(&transcript));
        scrolled.add_css_class("muse-transcript");
        // The transcript never scrolls sideways: anything wide wraps
        // to the viewport instead of pushing the column off-screen.
        scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        // The Mac panel names its thread up top, centered, with the
        // turn menu behind a "…" button at the row's end.
        let heading = gtk4::Label::new(Some("New chat"));
        heading.set_hexpand(true);
        heading.set_halign(gtk4::Align::Center);
        heading.add_css_class("title-4");
        let delete = gtk4::Button::with_label("Delete turn");
        let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        header.append(&heading);
        let menu = gtk4::MenuButton::new();
        menu.set_label("…");
        menu.add_css_class("flat");
        menu.set_tooltip_text(Some("Turn options"));
        {
            let popover = gtk4::Popover::new();
            popover.set_child(Some(&delete));
            menu.set_popover(Some(&popover));
        }
        header.append(&menu);
        // The web's right panel: profile, scheduled tasks, Today. It
        // reads the same tasks snapshot as the Tasks page, refreshed
        // beside the transcript instead of on a separate visit.
        let panel = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        panel.add_css_class("muse-panel");
        panel.set_size_request(280, -1);
        panel.set_margin_start(8);
        panel.set_margin_end(12);
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
        let tabs = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        tabs.set_halign(gtk4::Align::Center);
        let tab_bar: Rc<RefCell<Vec<gtk4::Button>>> = Rc::new(RefCell::new(Vec::new()));
        for name in PANEL_TABS {
            let tab = gtk4::Button::with_label(name);
            tab.add_css_class("flat");
            let stack = panel_stack.clone();
            let bar = tab_bar.clone();
            let active = name.to_owned();
            tab.connect_clicked(move |_| {
                stack.set_visible_child_name(&active.to_lowercase());
                for other in bar.borrow().iter() {
                    if other.label().as_deref() == Some(active.as_str()) {
                        other.add_css_class("muse-rail-active");
                    } else {
                        other.remove_css_class("muse-rail-active");
                    }
                }
            });
            tab_bar.borrow_mut().push(tab.clone());
            tabs.append(&tab);
        }
        if let Some(first) = tab_bar.borrow().first() {
            first.add_css_class("muse-rail-active");
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
                notice
            },
            armed_delete: RefCell::new(None),
            send_note: RefCell::new(None),
            painted: RefCell::new(None),
            thread: RefCell::new(None),
            model_combo,
            recording: RefCell::new(None),
            preview_fetch: RefCell::new(false),
            panel,
            panel_name,
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
            panel_placed: RefCell::new(false),
            live_tx: RefCell::new(None),
            older_exhausted: RefCell::new(false),
            on_option: RefCell::new(None),
            on_remove: RefCell::new(None),
            on_react: RefCell::new(None),
        });
        let weak = Rc::downgrade(&chat);
        *chat.on_option.borrow_mut() = Some(std::rc::Rc::new(move |text: String| {
            if let Some(chat) = weak.upgrade() {
                // An option press sends its text as the reader's next
                // message -- the same single-press path as the composer.
                chat.send_text(text, Vec::new());
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
        chat.wire();
        chat
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
        // The divider between transcript and panel drags: the panel
        // opens at 300px and then holds wherever the reader puts it.
        let split = gtk4::Paned::new(gtk4::Orientation::Horizontal);
        split.set_wide_handle(true);
        split.set_start_child(Some(&left));
        split.set_end_child(Some(&chat.panel));
        split.set_shrink_end_child(false);
        content.append(&split);
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
                    // The panel divider opens at 300px once the page
                    // knows its width, then holds wherever dragged.
                    if !*chat.panel_placed.borrow()
                        && let Some(split) = chat.panel.parent()
                        && split.allocated_width() > 400
                    {
                        if let Some(paned) = split.downcast_ref::<gtk4::Paned>() {
                            paned.set_position(split.allocated_width() - 300);
                        }
                        *chat.panel_placed.borrow_mut() = true;
                    }
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
                chat.stop_pressed();
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
            }
        });
        *self.on_remove.borrow_mut() = Some(on_remove);
        self.paint_chips();
        self.composer_row.refresh(&self.composer.borrow());
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
                if let Ok(list) = approvals {
                    Self::paint_approvals(chat, &list);
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
    ) {
        let ticket = match chat.page_begin() {
            Some(ticket) => ticket,
            None => return,
        };
        let Some(client) = chat.client_handle() else {
            chat.page_accept(&ticket);
            return;
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
    }

    /// One connector reset: the first press arms the row, the
    /// second press on the same row dispatches; the re-read proves
    /// the reset. A repaint disarms, which is the safe default.
    fn run_connector_reset(chat: &Rc<Self>, connector: &str) {
        let ticket = match chat.page_begin() {
            Some(ticket) => ticket,
            None => return,
        };
        let Some(client) = chat.client_handle() else {
            chat.page_accept(&ticket);
            return;
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
                    run_decision(&chat, &approval_id, decision);
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
                    let (connector_default, network_default, protocols) = match label.as_str() {
                        "connector_default" => (Some(value.clone()), None, Vec::new()),
                        "network_default" => (None, Some(value.clone()), Vec::new()),
                        _ => (None, None, vec![(label.clone(), value.clone())]),
                    };
                    // Protocol rows arrive labelled "protocol.<key>";
                    // the update takes the bare key.
                    let protocols = protocols
                        .into_iter()
                        .map(|(name, mode)| {
                            (
                                name.strip_prefix("protocol.").unwrap_or(&name).to_owned(),
                                mode,
                            )
                        })
                        .collect();
                    Self::run_settings_update(&chat, connector_default, network_default, protocols);
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
                Self::run_connector_reset(&chat, &name);
            });
            column.append(&press);
            row.set_child(Some(&column));
            chat.panel_policy.append(&row);
        }
    }

    /// The flip a policy row offers, if any: the service's own
    /// alternating words, never an invented third. Defaults swap
    /// ask-for-some with always-ask; protocol rows swap ask with
    /// deny. Anything else is display only.
    fn policy_flip(label: &str, value: &str) -> Option<PolicyFlip> {
        let next = match (label, value) {
            ("connector_default", "auto_allow") => "always_ask",
            ("connector_default", "always_ask") => "auto_allow",
            ("network_default", "auto_allow") => "always_ask",
            ("network_default", "always_ask") => "auto_allow",
            (name, "ask") if name.starts_with("protocol.") => "deny",
            (name, "deny") if name.starts_with("protocol.") => "ask",
            _ => return None,
        };
        Some(PolicyFlip {
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
            let row = gtk4::ListBoxRow::new();
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
            row.set_child(Some(&line));
            self.panel_schedules.append(&row);
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
                head.add_css_class("muse-section");
                self.panel_today.append(&head);
                section = Some(bucket);
            }
            let row = gtk4::ListBoxRow::new();
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
            row.set_child(Some(&line));
            self.panel_today.append(&row);
        }
    }
    /// True when the transcript shows its newest row: a repaint may
    /// follow to the bottom. Otherwise the reader is back-reading and
    /// the repaint holds still instead of yanking them down.
    fn at_bottom(&self) -> bool {
        let adjustment = self.scrolled.vadjustment();
        adjustment.value() + adjustment.page_size() >= adjustment.upper() - 2.0
    }

    /// Rebuild the transcript rows from the held history. The single
    /// place completions repaint, so option presses and attachments
    /// can never disagree with the rows. Follows to the newest row
    /// when the reader was already there; opening a thread lands at
    /// the bottom because a cleared transcript starts at the bottom.
    fn render(self: &Rc<Self>) {
        let follow = self.at_bottom();
        let fingerprint = self.session.borrow().history_fingerprint();
        // Unchanged history means identical rows: skip the full
        // teardown and rebuild. The labels below are cheap; the
        // ListBox reconstruction is what stalled long threads.
        if *self.painted.borrow() != Some(fingerprint.clone()) {
            if let Some(on_option) = self.on_option.borrow().clone() {
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
                populate(&self.transcript, &rows, &on_option, &on_react, &previews);
            }
            *self.painted.borrow_mut() = Some(fingerprint);
        }
        // Link cards resolve a render behind: the row shows text-only
        // first, the card version repaints when the fetch lands.
        self.fetch_previews();
        self.refresh_older();
        self.refresh_notice();
        self.refresh_title();
        self.refresh_delete_arm();
        if follow {
            let adjustment = self.scrolled.vadjustment();
            adjustment
                .set_value((adjustment.upper() - adjustment.page_size()).max(adjustment.lower()));
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

    /// The heading names the open thread from the service listing, or
    /// "New chat" before anything is picked. Cheap enough to repaint
    /// on every render; the label only changes on a thread switch.
    fn refresh_title(&self) {
        let thread = self.thread.borrow().clone();
        let title = thread.as_deref().and_then(|id| {
            self.session.borrow().account.clone().and_then(|account| {
                account
                    .threads
                    .iter()
                    .find(|row| row.session_id == id)
                    .map(|row| row.title.clone())
            })
        });
        self.heading
            .set_text(title.as_deref().unwrap_or("New chat"));
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

    /// Take the shared page slot for one secondary read. A second
    /// page pressing while the slot is held waits instead of stacking
    /// a duplicate dispatch.
    pub fn page_begin(&self) -> Option<super::session::Ticket> {
        self.session.borrow_mut().try_begin(Op::Page)
    }

    /// Release the page slot for a completion carrying its own ticket.
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
        let dispatch = match self.composer.borrow_mut().begin_send() {
            Some(dispatch) => dispatch,
            None => return,
        };
        self.composer_row.set_input_text("");
        // A fresh send clears yesterday's send note; the outcome of
        // this one writes its own.
        *self.send_note.borrow_mut() = None;
        self.paint_chips();
        self.composer_row.refresh(&self.composer.borrow());
        self.send_text(dispatch.text, dispatch.images);
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
    fn send_text(self: &Rc<Self>, draft: String, images: Vec<StagedImage>) {
        let ticket = match self.session.borrow_mut().try_begin(Op::Send) {
            Some(ticket) => ticket,
            None => {
                self.composer.borrow_mut().finish_send();
                self.composer.borrow_mut().restore(images);
                self.paint_chips();
                self.composer_row.refresh(&self.composer.borrow());
                // A held slot is never silent: the reader sees the
                // message is kept and the log names the contention.
                eprintln!("muse: send pressed while the session slot is held; message kept");
                self.say_send_note("Still working — your message is kept; send again in a moment.");
                return;
            }
        };
        let Some(client) = self.client.borrow().clone() else {
            self.session.borrow_mut().accept(&ticket);
            self.composer.borrow_mut().finish_send();
            self.composer.borrow_mut().restore(images);
            self.paint_chips();
            self.composer_row.refresh(&self.composer.borrow());
            return;
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
            self.say_send_note("Sign in before sending pictures.");
            self.composer_row.refresh(&self.composer.borrow());
            return;
        }
        let scope = self.thread_scope();
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
                    ),
                    Some((vm_id, identity)) => {
                        client.send_verified(scope.as_deref(), &draft, &vm_id, &identity)
                    }
                    None => client.send(scope.as_deref(), &draft),
                };
                // The pictures ride back out with the answer: a failed
                // send restores exactly what it took, dims and all.
                (answer, images, with_images)
            },
            move |chat, (answer, images, with_images)| {
                chat.session.borrow_mut().accept(&ticket);
                chat.composer.borrow_mut().finish_send();
                chat.composer_row.refresh(&chat.composer.borrow());
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
    /// send holds it. Fire-and-forget; the next history read shows
    /// whatever actually landed.
    fn stop_pressed(&self) {
        if let Some(client) = self.client.borrow().clone() {
            std::thread::spawn(move || {
                let _ = client.cancel();
            });
        }
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
/// of the answer alone.
fn run_decision(chat: &Rc<Chat>, approval_id: &str, decision: muse_account::Decision) {
    let ticket = match chat.page_begin() {
        Some(ticket) => ticket,
        None => return,
    };
    let Some(client) = chat.client_handle() else {
        chat.page_accept(&ticket);
        return;
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
        assert_eq!(PANEL_TABS, ["Activity", "Approvals", "Identity", "Mail"]);
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
