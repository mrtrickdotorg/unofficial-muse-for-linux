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
fn dispatch<T, W, D>(chat: &Rc<Chat>, work: W, done: D)
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

/// Shared chat state: the session plus every widget a completion
/// touches. Reference-counted because idle callbacks own it.
pub struct Chat {
    session: Rc<RefCell<Session>>,
    composer: Rc<RefCell<Composer>>,
    transcript: gtk4::ListBox,
    composer_row: ComposerRow,
    client: RefCell<Option<AccountClient>>,
    older_button: gtk4::Button,
    older_exhausted: RefCell<bool>,
    live_tx: RefCell<Option<std::sync::mpsc::Sender<()>>>,
    on_option: RefCell<Option<OptionCallback>>,
}

impl Chat {
    pub fn build(transcript: gtk4::ListBox, composer_row: ComposerRow) -> Rc<Self> {
        let chat = Rc::new(Self {
            session: Rc::new(RefCell::new(Session::new())),
            composer: Rc::new(RefCell::new(Composer::new())),
            transcript,
            composer_row,
            client: RefCell::new(None),
            older_button: gtk4::Button::with_label("Load older"),
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
        let scrolled = gtk4::ScrolledWindow::builder()
            .child(&transcript)
            .vexpand(true)
            .hexpand(true)
            .build();
        let composer_row = ComposerRow::build();
        let chat = Self::build(transcript, composer_row);
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
        content.append(&chat.older_button);
        content.append(&scrolled);
        content.append(chat.composer_row.widget());
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
    }

    /// Rebuild the transcript rows from the held history. The single
    /// place completions repaint, so option presses and attachments
    /// can never disagree with the rows.
    fn render(&self) {
        if let Some(on_option) = self.on_option.borrow().clone() {
            let rows = build_rows(&self.session.borrow().history);
            populate(&self.transcript, &rows, &on_option);
        }
        self.refresh_older();
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
        dispatch(
            self,
            move || client.send(None, &draft),
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
        dispatch(
            self,
            move || client.history_window(None, anchor, 20, 1),
            move |chat, answer| match answer {
                Ok(window) => {
                    if chat.session.borrow_mut().accept(&ticket) {
                        *chat.older_exhausted.borrow_mut() = !window.has_older;
                        chat.session.borrow_mut().prepend_history(window.messages);
                        chat.render();
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
        dispatch(
            self,
            move || client.history(None, 32),
            move |chat, answer| {
                if chat.session.borrow_mut().finish_history(&ticket, answer) {
                    chat.render();
                }
            },
        );
    }
}
