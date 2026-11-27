//! Desktop notifications over `org.freedesktop.Notifications`.
//!
//! The app posts one notification per scheduled run it settles, and a click on
//! any of them runs the handler the app installed at startup (the Tasks page).
//! Everything here is best-effort: a session bus with no notification daemon, or
//! a daemon that refuses the call, must leave the run untouched, so a failure is
//! logged once and then the next attempt is a few minutes away, never a per-run
//! error line.
//!
//! The click signal is a session-bus broadcast: every app's notifications report
//! their actions on it, so the set of daemon ids this app was given is what makes
//! a click ours. That set is never pruned: the card is only drawn for a few
//! seconds, and a reader who comes back to the tray minutes later still expects
//! their click to land, while the daemon's ids only ever count upward. No new
//! dependency: `gio` already rides along with GTK, and the protocol is one method
//! call plus one signal.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use gtk4::gio;
use gtk4::glib;
use gtk4::glib::variant::ToVariant;

/// The name the notification daemon shows (and the desktop entry's own name).
const APP_NAME: &str = "Muse";
const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";
/// The daemon draws the card for this long; a click is still routed after it
/// as long as the daemon keeps the card in its own history.
const EXPIRE_MS: i32 = 8000;
/// After a bus with nobody on the notifications name, wait this long before
/// trying again, so a daemon that starts later is picked up without a retry
/// storm in the meantime.
const RETRY_AFTER_SECS: i64 = 300;

/// What a click runs: the app's own route, installed once at startup.
type ClickHandler = Box<dyn Fn()>;

#[derive(Default)]
struct State {
    connection: Option<gio::DBusConnection>,
    /// Nothing owns the notifications name until this instant (epoch seconds).
    quiet_until: i64,
    /// The missing-daemon line is written once per app run, not per send.
    logged_missing: bool,
    /// The ids the daemon gave this app's notifications: a click is routed only
    /// for one of these, never for another app's.
    posted: HashSet<u32>,
    handler: Option<ClickHandler>,
    /// The click subscription itself: dropping it unsubscribes, so it lives
    /// here for the process's life.
    clicked: Option<gio::SignalSubscription>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// What a click does: the app installs one handler at startup, and it runs for
/// any notification this app posted.
pub(crate) fn set_click_handler<F>(handler: F)
where
    F: Fn() + 'static,
{
    STATE.with(|state| state.borrow_mut().handler = Some(Box::new(handler)));
}

/// The id the daemon answered a `Notify` call with. The reply is a one-field
/// structure (`(u)`), so reading it as a bare `u32` fails on every call -- and a
/// scheduler that cannot name its notifications cannot recognise its own clicks.
/// Zero means the daemon kept nothing, which leaves nothing to route.
fn posted_id(reply: &glib::Variant) -> Option<u32> {
    let (id,) = reply.get::<(u32,)>()?;
    (id != 0).then_some(id)
}

/// The session bus, connected and subscribed once. `None` means this session
/// has no session bus at all, which is just as quiet as no daemon.
fn connection() -> Option<gio::DBusConnection> {
    if let Some(held) = STATE.with(|state| state.borrow().connection.clone()) {
        return Some(held);
    }
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    // The click signal is watched for the life of the process: a click has to
    // reach the app even when the notification was posted minutes ago.
    // No sender filter: a click may be reported by the daemon itself or by the
    // desktop shell that owns it (some do), and the id has to be one this app
    // posted before a click routes anywhere -- the set of ids is the filter.
    let clicked = connection.subscribe_to_signal(
        None,
        Some(INTERFACE),
        Some("ActionInvoked"),
        Some(OBJECT_PATH),
        None,
        gio::DBusSignalFlags::NONE,
        |signal: gio::DBusSignalRef| {
            // A signal whose parameters are not the spec's `(us)` is not a
            // click this app can place: it is dropped, never read as id 0.
            let Some((id, action)) = signal.parameters.get::<(u32, String)>() else {
                return;
            };
            let ours = STATE.with(|state| state.borrow().posted.contains(&id));
            if !ours {
                return;
            }
            eprintln!("muse notify: a notification was clicked (action={action:?})");
            STATE.with(|state| {
                let state = state.borrow();
                if let Some(handler) = state.handler.as_ref() {
                    handler();
                }
            });
        },
    );
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.connection = Some(connection.clone());
        state.clicked = Some(clicked);
    });
    Some(connection)
}

/// Post one notification. Nothing is returned: a bus that refuses it is logged
/// once and then quiet, never an error for the caller -- the run has already
/// happened either way.
pub(crate) fn notify(summary: &str, body: &str) {
    let Some(connection) = connection() else {
        quiet("this session has no session bus");
        return;
    };
    let until = STATE.with(|state| state.borrow().quiet_until);
    if now_secs() < until {
        return;
    }
    // Two actions: the body's own default click and a button that says what it
    // does. Both route to the installed handler.
    let actions: Vec<&str> = vec!["default", "Open Tasks"];
    // `a{sv}`, not a variant around it: the daemon matches the call by its
    // exact signature, and a `v` in this position is rejected as no such
    // method at all.
    let hints: HashMap<String, glib::Variant> = HashMap::new();
    let parameters = (APP_NAME, 0u32, "", summary, body, actions, hints, EXPIRE_MS).to_variant();
    match connection.call_sync(
        Some(BUS_NAME),
        OBJECT_PATH,
        INTERFACE,
        "Notify",
        Some(&parameters),
        None,
        gio::DBusCallFlags::NONE,
        5_000,
        gio::Cancellable::NONE,
    ) {
        Ok(reply) => {
            let id = posted_id(&reply);
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(id) = id {
                    state.posted.insert(id);
                }
                state.logged_missing = false;
            });
            eprintln!("muse notify: posted id={id:?} summary={summary:?}");
        }
        Err(error) => {
            // A daemon that is not there answers `ServiceUnknown`; anything
            // else (a daemon refusing one call) is just as harmless to the run.
            let missing = error.message().contains("ServiceUnknown")
                || error.message().contains("NameHasNoOwner");
            quiet(&format!(
                "no desktop notification was posted ({}): {error}",
                if missing {
                    "nobody owns"
                } else {
                    "call failed"
                }
            ));
            STATE.with(|state| state.borrow_mut().quiet_until = now_secs() + RETRY_AFTER_SECS);
        }
    }
}

/// One line, once per app run, and then silence for the retry window: a
/// schedule that fires on a bus with no daemon must not fill the log.
fn quiet(reason: &str) {
    let first = STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.logged_missing {
            false
        } else {
            state.logged_missing = true;
            true
        }
    });
    if first {
        eprintln!("muse notify: {reason}; scheduled runs still execute");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Notify` answers with a one-field structure: the id is read out of it,
    /// and a zero answer is no id at all.
    #[test]
    fn the_reply_names_the_id_the_daemon_gave() {
        assert_eq!(posted_id(&(15u32,).to_variant()), Some(15));
        assert_eq!(posted_id(&(0u32,).to_variant()), None);
    }

    /// The one line per app run: a second missing daemon inside the quiet
    /// window writes nothing.
    #[test]
    fn a_missing_daemon_is_reported_once_and_then_quiet() {
        STATE.with(|state| {
            *state.borrow_mut() = State::default();
        });
        quiet("nobody owns org.freedesktop.Notifications");
        quiet("nobody owns org.freedesktop.Notifications");
        assert!(
            STATE.with(|state| state.borrow().logged_missing),
            "the first miss is the one that is written"
        );
        // The retry window is what silences the following sends.
        STATE.with(|state| state.borrow_mut().quiet_until = now_secs() + 60);
        assert!(
            STATE.with(|state| now_secs() < state.borrow().quiet_until),
            "a missing daemon is not retried until the window passes"
        );
    }
}
