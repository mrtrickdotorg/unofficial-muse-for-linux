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

/// Shared chat state: the session plus every widget a completion
/// touches. Reference-counted because idle callbacks own it.
pub struct Chat {
    session: Rc<RefCell<Session>>,
    composer: Rc<RefCell<Composer>>,
    transcript: gtk4::ListBox,
    composer_row: ComposerRow,
    client: RefCell<Option<AccountClient>>,
}

impl Chat {
    pub fn build(transcript: gtk4::ListBox, composer_row: ComposerRow) -> Rc<Self> {
        let chat = Rc::new(Self {
            session: Rc::new(RefCell::new(Session::new())),
            composer: Rc::new(RefCell::new(Composer::new())),
            transcript,
            composer_row,
            client: RefCell::new(None),
        });
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
        content.append(&scrolled);
        content.append(composer_row.widget());
        let chat = Self::build(transcript, composer_row);
        if let Some(link) = load_saved_link()
            && link.consent
        {
            chat.load(&link.python, &link.config_dir);
        }
        // Liveness is a bounded poll, not a push subscription: every 8s
        // the slot is offered a history re-read, which no-ops while a
        // send or connect holds it. Adopting the service's subscribe
        // surface is a later milestone; the poll keeps this cheap and
        // entirely within the ticket discipline.
        let weak = Rc::downgrade(&chat);
        glib::timeout_add_local(std::time::Duration::from_secs(8), move || {
            if let Some(chat) = weak.upgrade() {
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
                }
            },
        );
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
                    let rows = build_rows(&chat.session.borrow().history);
                    populate(&chat.transcript, &rows);
                }
            },
        );
    }
}
