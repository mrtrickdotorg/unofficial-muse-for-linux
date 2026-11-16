//! Sign-in surface for the rewrite: the manual link form (interpreter
//! plus session folder plus standing consent). Browser-based sign-in
//! stays a later milestone; this is the same Advanced path the shipped
//! client offers, and it reads and writes the same saved link.
//!
//! The form validates before it saves: absolute paths only, the
//! interpreter must exist, consent must be ticked. A fragile session
//! folder relocates through the session module so a working sign-in
//! is never stranded by a cache sweep.

use std::path::Path;

use adw::prelude::*;

use super::session::{
    SavedConnection, account_dir, effective_config_dir, save_saved_connection_in,
};

/// Validate a link the reader typed. Returns the normalized link: the
/// session folder relocated out of fragile roots when needed.
pub fn validate_link(
    python: &str,
    config_dir: &str,
    consent: bool,
) -> Result<SavedConnection, &'static str> {
    if !consent {
        return Err("Tick the consent box: the app may only use the session you approve.");
    }
    if python.is_empty() || python.len() > 4096 || python.contains('\0') {
        return Err("Enter the Python interpreter path.");
    }
    let python_path = Path::new(python);
    if !python_path.is_absolute() {
        return Err("The interpreter path must be absolute.");
    }
    if !python_path.is_file() {
        return Err("No interpreter at that path. Enter the Python the bridge setup printed.");
    }
    if config_dir.is_empty() || config_dir.len() > 4096 || config_dir.contains('\0') {
        return Err("Enter a session folder.");
    }
    let config_path = Path::new(config_dir);
    if !config_path.is_absolute() {
        return Err("The session folder path must be absolute.");
    }
    Ok(SavedConnection {
        python: python_path.to_owned(),
        config_dir: effective_config_dir(python_path, config_path, true),
        consent: true,
    })
}

/// The sign-in form. `on_linked` runs on the main thread with the saved
/// link after the form validates and persists it.
pub struct SignIn {
    page: gtk4::Box,
    python_entry: gtk4::Entry,
    folder_entry: gtk4::Entry,
    consent: gtk4::CheckButton,
    connect: gtk4::Button,
    error: gtk4::Label,
}

impl SignIn {
    pub fn build(on_linked: impl Fn(SavedConnection) + 'static) -> Self {
        let page = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
        page.set_margin_start(24);
        page.set_margin_end(24);
        page.set_margin_top(24);
        page.set_halign(gtk4::Align::Center);
        page.set_valign(gtk4::Align::Center);

        let title = gtk4::Label::new(Some("Sign in to Muse"));
        title.add_css_class("title-1");
        page.append(&title);

        let hint = gtk4::Label::new(Some(
            "Enter the Python interpreter the bridge setup printed and a private session folder. The app's own folder already qualifies.",
        ));
        hint.set_wrap(true);
        page.append(&hint);

        let python_entry = gtk4::Entry::new();
        python_entry.set_placeholder_text(Some("Python interpreter, e.g. /usr/bin/python3"));
        page.append(&python_entry);

        let folder_entry = gtk4::Entry::new();
        folder_entry.set_placeholder_text(Some("Session folder"));
        if let Some(default) = account_dir() {
            folder_entry.set_text(&default.display().to_string());
        }
        page.append(&folder_entry);

        let consent = gtk4::CheckButton::with_label(
            "I approve this app using my Muse session from this folder.",
        );
        page.append(&consent);

        let connect = gtk4::Button::with_label("Connect");
        connect.add_css_class("suggested-action");
        page.append(&connect);

        let error = gtk4::Label::new(None);
        error.set_wrap(true);
        error.add_css_class("error");
        page.append(&error);

        let form = Self {
            page,
            python_entry,
            folder_entry,
            consent,
            connect,
            error,
        };
        let python = form.python_entry.clone();
        let folder = form.folder_entry.clone();
        let consent_box = form.consent.clone();
        let error_label = form.error.clone();
        form.connect.connect_clicked(move |_| {
            match validate_link(&python.text(), &folder.text(), consent_box.is_active()) {
                Ok(link) => {
                    let Some(dir) = account_dir() else {
                        error_label.set_text("No config home to save the link into.");
                        return;
                    };
                    match save_saved_connection_in(&dir, &link) {
                        Ok(()) => on_linked(link),
                        Err(message) => error_label.set_text(&message),
                    }
                }
                Err(message) => error_label.set_text(message),
            }
        });
        form
    }

    pub fn widget(&self) -> &gtk4::Box {
        &self.page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real_interpreter() -> String {
        // The test harness itself: always an absolute path to a real file.
        std::env::current_exe().unwrap().display().to_string()
    }

    #[test]
    fn link_needs_consent_absolute_paths_and_a_real_interpreter() {
        assert!(validate_link(&real_interpreter(), "/tmp/x", false).is_err());
        assert!(validate_link("relative/python", "/tmp/x", true).is_err());
        assert!(validate_link("/nonexistent/python", "/tmp/x", true).is_err());
        assert!(validate_link(&real_interpreter(), "relative/dir", true).is_err());
        assert!(validate_link(&real_interpreter(), "", true).is_err());
    }

    #[test]
    fn valid_link_normalizes_through_the_session_rules() {
        let interpreter = real_interpreter();
        let link =
            validate_link(&interpreter, "/home/test/.config/muse", true).expect("valid link");
        assert!(link.consent);
        assert_eq!(link.python, std::path::PathBuf::from(&interpreter));
    }
}
