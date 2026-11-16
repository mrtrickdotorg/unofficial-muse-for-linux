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
use super::composer::{Composer, ComposerRow};
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
    glib::idle_add_local(move || {
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                return glib::ControlFlow::Continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return glib::ControlFlow::Break;
            }
        };
        if let Some(done) = done.take()
            && let Some(chat) = weak.upgrade()
        {
            done(&chat, answer);
        }
        glib::ControlFlow::Break
    });
}

/// An option button's press, delivered as the reader's next message.
type OptionCallback = std::rc::Rc<dyn Fn(String)>;

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
    client: RefCell<Option<AccountClient>>,
    older_button: gtk4::Button,
    older_exhausted: RefCell<bool>,
    live_tx: RefCell<Option<std::sync::mpsc::Sender<()>>>,
    on_option: RefCell<Option<OptionCallback>>,
    notice: gtk4::Label,
    armed_delete: RefCell<Option<String>>,
    painted: RefCell<Option<(usize, Option<String>)>>,
    thread: RefCell<Option<String>>,
    model_combo: gtk4::ComboBoxText,
}

impl Chat {
    pub fn build(transcript: gtk4::ListBox, composer_row: ComposerRow) -> Rc<Self> {
        let scrolled = gtk4::ScrolledWindow::new();
        scrolled.set_child(Some(&transcript));
        let chat = Rc::new(Self {
            session: Rc::new(RefCell::new(Session::new())),
            composer: Rc::new(RefCell::new(Composer::new())),
            transcript,
            scrolled,
            composer_row,
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
            painted: RefCell::new(None),
            thread: RefCell::new(None),
            model_combo: gtk4::ComboBoxText::new(),
            live_tx: RefCell::new(None),
            older_exhausted: RefCell::new(false),
            on_option: RefCell::new(None),
        });
        let weak = Rc::downgrade(&chat);
        *chat.on_option.borrow_mut() = Some(std::rc::Rc::new(move |text: String| {
            if let Some(chat) = weak.upgrade() {
                // An option press sends its text as the reader's next
                // message -- the same single-press path as the composer.
                chat.send_text(text);
            }
        }) as OptionCallback);
        chat.wire();
        chat
    }

    /// Build the whole chat surface for the chat page: transcript plus
    /// composer, linked to the saved account when one exists. No saved
    /// link, no dispatch -- the page simply waits for a sign-in.
    pub fn install(content: &gtk4::Box) -> Rc<Self> {
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
        chat.model_combo.set_halign(gtk4::Align::Start);
        content.append(&chat.model_combo);
        content.append(&chat.older_button);
        content.append(&chat.scrolled);
        content.append(&chat.notice);
        content.append(chat.composer_row.widget());
        {
            let weak = Rc::downgrade(&chat);
            chat.composer_row.delete.connect_clicked(move |_| {
                if let Some(chat) = weak.upgrade() {
                    chat.delete_pressed();
                }
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
        glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            let mut woke = false;
            while live_rx.try_recv().is_ok() {
                woke = true;
            }
            ticks = ticks.wrapping_add(1);
            let Some(chat) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if woke || ticks >= 8 {
                ticks = 0;
                chat.reload_history();
            }
            glib::ControlFlow::Continue
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
        self.composer_row.entry.connect_changed(move |entry| {
            if let Some(chat) = weak.upgrade() {
                chat.composer.borrow_mut().set_text(&entry.text());
                chat.composer_row.refresh(&chat.composer.borrow());
            }
        });
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
    fn render(&self) {
        let follow = self.at_bottom();
        let fingerprint = self.session.borrow().history_fingerprint();
        // Unchanged history means identical rows: skip the full
        // teardown and rebuild. The labels below are cheap; the
        // ListBox reconstruction is what stalled long threads.
        if *self.painted.borrow() != Some(fingerprint.clone()) {
            if let Some(on_option) = self.on_option.borrow().clone() {
                let rows = build_rows(&self.session.borrow().history);
                for row in &rows {
                    if let super::chat::TranscriptRow::Notice { text } = row {
                        self.session.borrow_mut().note_quota(text.clone());
                    }
                }
                populate(&self.transcript, &rows, &on_option);
            }
            *self.painted.borrow_mut() = Some(fingerprint);
        }
        self.refresh_older();
        self.refresh_notice();
        self.refresh_delete_arm();
        if follow {
            let adjustment = self.scrolled.vadjustment();
            adjustment
                .set_value((adjustment.upper() - adjustment.page_size()).max(adjustment.lower()));
        }
    }

    /// The notice line shows the service's latest limit sentence while
    /// one is remembered, hidden otherwise.
    fn refresh_notice(&self) {
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
        self.composer_row
            .set_delete_armed(armed.is_some() && armed == latest);
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
        let draft = match self.composer.borrow_mut().begin_send() {
            Some(draft) => draft,
            None => return,
        };
        self.composer_row.entry.set_text("");
        self.composer_row.refresh(&self.composer.borrow());
        self.send_text(draft);
    }

    /// Dispatch one message text: the composer's draft or an option
    /// button's text. One press dispatches once; a press while the
    /// slot is held releases the composer instead of queueing.
    fn send_text(self: &Rc<Self>, draft: String) {
        let ticket = match self.session.borrow_mut().try_begin(Op::Send) {
            Some(ticket) => ticket,
            None => {
                self.composer.borrow_mut().finish_send();
                self.composer_row.refresh(&self.composer.borrow());
                return;
            }
        };
        let Some(client) = self.client.borrow().clone() else {
            self.session.borrow_mut().accept(&ticket);
            self.composer.borrow_mut().finish_send();
            self.composer_row.refresh(&self.composer.borrow());
            return;
        };
        let verified = self
            .session
            .borrow()
            .account
            .clone()
            .map(|account| (account.vm_id.clone(), account.identity_name.clone()));
        let scope = self.thread_scope();
        dispatch(
            self,
            move || match verified {
                Some((vm_id, identity)) => {
                    client.send_verified(scope.as_deref(), &draft, &vm_id, &identity)
                }
                None => client.send(scope.as_deref(), &draft),
            },
            move |chat, answer| {
                chat.session.borrow_mut().accept(&ticket);
                chat.composer.borrow_mut().finish_send();
                chat.composer_row.refresh(&chat.composer.borrow());
                if answer.is_ok() {
                    // Prove by re-read: the reply (or the limit notice)
                    // comes from the service, never from the dispatch.
                    // A short quick-poll burst (5 x 2s) watches the reply
                    // land; the 8s poll resumes after. Each round no-ops
                    // while the slot is held.
                    let weak = Rc::downgrade(chat);
                    let rounds: std::rc::Rc<std::cell::Cell<u8>> =
                        std::rc::Rc::new(std::cell::Cell::new(5));
                    glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
                        let left = rounds.get();
                        rounds.set(left.saturating_sub(1));
                        if let Some(chat) = weak.upgrade() {
                            chat.reload_history();
                        }
                        if left > 1 {
                            glib::ControlFlow::Continue
                        } else {
                            glib::ControlFlow::Break
                        }
                    });
                    chat.reload_history();
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
                self.composer_row.set_delete_armed(false);
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

#[cfg(test)]
mod tests {
    use super::*;

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
