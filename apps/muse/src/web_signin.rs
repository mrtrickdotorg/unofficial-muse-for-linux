//! The sign-in page's presses, answered by the app's own sign-in module.
//!
//! The page owns the typing; the rules are [`super::signin::validate_link`]'s and
//! [`super::session::save_saved_connection_in`]'s, exactly the ones the native
//! form calls. That is the point of the slice: a page cannot accept a link the
//! app would refuse, and the isolated browser flow is the same worker the
//! native form starts, with the same cancellation.
//!
//! The session folder is the page's own choice: its sheet lists one level at a
//! time through [`crate::webui`]'s folder read, and `choose` records the answer
//! in the held state the form's validation and the next snapshot read. No
//! window opens here, so a page that can choose its folder never drops out of
//! the shell for a native dialog.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use crate::pages::{HeldSignin, signin_update};
use crate::webui::{SigninPress, WebSigninPress};

thread_local! {
    /// The browser flow that is running, if any: Cancel names it.
    static CURRENT: RefCell<Option<Arc<super::login::Cancellation>>> =
        const { RefCell::new(None) };
}

/// Build the page's press route. `on_linked` runs on the main thread once a
/// link validates and saves -- the app's own next step after sign-in.
pub(crate) fn routes(on_linked: impl Fn() + 'static) -> WebSigninPress {
    let on_linked: Rc<dyn Fn()> = Rc::new(on_linked);
    Rc::new(move |press: SigninPress| match press.press.as_str() {
        "connect" => connect(&press, &on_linked),
        "browser" => browser(&press, &on_linked),
        "cancel" => {
            CURRENT.with(|current| {
                if let Some(flow) = current.borrow().as_ref() {
                    flow.cancel();
                }
            });
            signin_update(|held| {
                held.status = "Cancelling the browser sign-in…".to_owned();
            });
            Ok(())
        }
        "choose" => choose(&press),
        other => Err(format!("This page has no {other} press.")),
    })
}

/// The manual link: validate with the app's own rules, save through the app's
/// own writer, and only then call the app's next step.
fn connect(press: &SigninPress, on_linked: &Rc<dyn Fn()>) -> Result<(), String> {
    remember(press);
    let link = super::signin::validate_link(&press.python, &press.folder, press.consent)
        .map_err(str::to_owned)?;
    let Some(dir) = super::session::account_dir() else {
        return refuse("No config home to save the link into.");
    };
    super::session::save_saved_connection_in(&dir, &link)?;
    signin_update(|held| {
        held.error.clear();
        held.status = "Signed in. The app window takes it from here.".to_owned();
    });
    on_linked();
    Ok(())
}

/// The isolated browser flow: the same worker the native form starts, its
/// answer delivered on the main loop.
fn browser(press: &SigninPress, on_linked: &Rc<dyn Fn()>) -> Result<(), String> {
    remember(press);
    if !press.consent {
        return refuse("Tick the consent box first: the app may only use the session you approve.");
    }
    let python = std::path::PathBuf::from(&press.python);
    if !python.is_absolute() || !python.is_file() {
        return refuse("No interpreter at that path. Enter the Python the bridge setup printed.");
    }
    let folder = std::path::PathBuf::from(&press.folder);
    if !folder.is_absolute() {
        return refuse("The session folder path must be absolute.");
    }
    let cancellation = Arc::new(super::login::Cancellation::default());
    CURRENT.with(|current| *current.borrow_mut() = Some(cancellation.clone()));
    signin_update(|held| {
        held.error.clear();
        held.browser_open = true;
        held.status = "Opening the isolated browser — sign in there, then return here.".to_owned();
    });
    let script = super::session::bridge_script("muse_account_login.py");
    let (done_tx, done_rx) =
        std::sync::mpsc::channel::<Result<super::session::SavedConnection, String>>();
    std::thread::spawn(move || {
        let outcome = super::login::run_sign_in(&python, &script, &folder, &cancellation);
        let saved = outcome.map(|()| super::session::SavedConnection {
            python,
            config_dir: folder,
            consent: true,
        });
        let _ = done_tx.send(saved);
    });
    let linked = on_linked.clone();
    gtk4::glib::idle_add_local(move || {
        let answer = match done_rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                return gtk4::glib::ControlFlow::Continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                CURRENT.with(|current| *current.borrow_mut() = None);
                signin_update(|held| {
                    held.browser_open = false;
                    held.status.clear();
                    held.error = "The sign-in helper stopped without answering.".to_owned();
                });
                return gtk4::glib::ControlFlow::Break;
            }
        };
        CURRENT.with(|current| *current.borrow_mut() = None);
        signin_update(|held| {
            held.browser_open = false;
            held.status.clear();
        });
        match answer {
            Ok(link) => {
                let Some(dir) = super::session::account_dir() else {
                    signin_update(|held| {
                        held.error = "No config home to save the link into.".to_owned();
                    });
                    return gtk4::glib::ControlFlow::Break;
                };
                match super::session::save_saved_connection_in(&dir, &link) {
                    Ok(()) => {
                        signin_update(|held| {
                            held.error.clear();
                            held.status =
                                "Signed in. The app window takes it from here.".to_owned();
                        });
                        linked();
                    }
                    Err(message) => signin_update(|held| held.error = message),
                }
            }
            Err(message) => signin_update(|held| held.error = message),
        }
        gtk4::glib::ControlFlow::Break
    });
    Ok(())
}

/// The folder the page's sheet returned: recorded in the same held state the
/// native form writes, so the field, the next snapshot and the Connect press's
/// own validation all read one value. The sheet is the page's; this is the
/// app's answer to it, in the app's own words.
fn choose(press: &SigninPress) -> Result<(), String> {
    // The folder is checked before anything is recorded: a refused path must
    // not become the folder the form shows.
    let asked = press.folder.trim();
    if asked.is_empty() {
        return refuse("Choose a folder first.");
    }
    let path = std::path::Path::new(asked);
    if !path.is_absolute() {
        return refuse("The session folder path must be absolute.");
    }
    if !path.is_dir() {
        return refuse("No folder at that path.");
    }
    signin_update(|held| {
        held.python = press.python.clone();
        held.folder = path.display().to_string();
        held.consent = press.consent;
        held.error.clear();
    });
    Ok(())
}

/// Record what was typed, so the next snapshot answers with the values the
/// press used rather than the ones the page opened on.
fn remember(press: &SigninPress) {
    signin_update(|held: &mut HeldSignin| {
        held.python = press.python.clone();
        held.folder = press.folder.clone();
        held.consent = press.consent;
    });
}

/// Put a refusal where every surface reads it (the page's error line and the
/// app's own notice) and answer it to the press that caused it.
fn refuse(message: &str) -> Result<(), String> {
    signin_update(|held| {
        held.error = message.to_owned();
        held.status.clear();
    });
    Err(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The folder press is the page's own sheet answer, recorded in the state
    /// the form reads: no window opens, and the next snapshot carries the
    /// folder the Connect press will validate.
    #[test]
    fn a_chosen_folder_lands_in_the_state_the_form_reads() {
        let dir = std::env::temp_dir().join(format!("muse-signin-choose-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe folder");
        crate::pages::signin_cache(crate::pages::HeldSignin::opening());

        let press = routes(|| {});
        let chosen = press(SigninPress {
            press: "choose".to_owned(),
            folder: dir.display().to_string(),
            ..SigninPress::default()
        });

        assert_eq!(chosen, Ok(()));
        let held = crate::pages::signin_snapshot();
        assert_eq!(held.folder, dir.display().to_string());

        // A folder the app cannot use is refused in its own words, and the
        // field keeps the folder that was there.
        let refused = press(SigninPress {
            press: "choose".to_owned(),
            folder: "relative/folder".to_owned(),
            ..SigninPress::default()
        });
        assert_eq!(
            refused,
            Err("The session folder path must be absolute.".to_owned())
        );
        let held = crate::pages::signin_snapshot();
        assert_eq!(held.folder, dir.display().to_string());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
