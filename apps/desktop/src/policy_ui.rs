//! Policy page: the durable settings the connected account's service holds.
//!
//! `permissions.settings` is a recorded GET on the sentinel service; the
//! bridge projects it to scalar display pairs plus the names in its own
//! connectors section, and this page shows them as the service reports
//! them. Editing a setting stays on the Muse side: the update route
//! exists (`permissions.settings.update`), but its PATCH body is recorded
//! nowhere, and an unrecorded write shape is never guessed.
//!
//! A connector's grants can be reset from here. The reset route
//! (`egress.permissions.connector.reset`) is a path-only DELETE, so one
//! armed press is one reset, addressed by a name the settings themselves
//! published; whether the connector then reads as reset is proven by the
//! re-read that follows, never by the reset's own answer.

use crate::local_host::AccountContext;
use gtk::prelude::*;
use gtk4 as gtk;
use muse_account::{AccountClient, AccountError, ErrorCode, PolicySettings};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

pub struct PolicyPage {
    pub root: gtk::Box,
    /// Loads (or re-reads) the policy; fired by the shell when the
    /// Policy page is opened from the Menu.
    pub load: Rc<dyn Fn()>,
    #[cfg(test)]
    pub(super) ui: Rc<Ui>,
}

pub(super) struct Ui {
    epoch: Cell<u64>,
    busy: Cell<bool>,
    acting: Cell<bool>,
    context: AccountContext,
    list: gtk::ListBox,
    connectors: gtk::ListBox,
    notice: gtk::Label,
    spinner: gtk::Spinner,
    /// The last settings read, kept whole: rendering and the
    /// connector rows both work from the service's own answer.
    settings: RefCell<Option<PolicySettings>>,
    /// The connector whose Reset press is armed, if any. Arming
    /// is the confirmation: the second press on the same row sends.
    armed: RefCell<Option<String>>,
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_selectable(true);
    label
}

/// The count line after a read: the service's settings, and --
/// when it holds values the projection could not show as pairs --
/// that they were counted rather than silently dropped.
pub(super) fn summary_text(settings: &PolicySettings) -> String {
    let mut text = match settings.entries.len() {
        0 => "The service reports no readable policy settings.".to_string(),
        1 => "The service reports 1 policy setting.".to_string(),
        n => format!("The service reports {n} policy settings."),
    };
    if settings.withheld > 0 {
        text.push_str(&format!(
            " {} further value{} the service holds could not be shown as a setting and {} not listed.",
            settings.withheld,
            if settings.withheld == 1 { "" } else { "s" },
            if settings.withheld == 1 { "was" } else { "were" },
        ));
    }
    text
}

/// What arming a reset tells the reader before the second press.
pub(super) fn reset_arm_text(connector: &str) -> String {
    format!(
        "Resetting \"{connector}\" clears the grants that connector holds; the connector itself stays connected. Press Confirm reset on that row to send it."
    )
}

/// Map a typed failure to a sentence. The error's own text never reaches
/// the page: the bridge's codes are fixed, its details are not, and a
/// detail could quote service output.
pub(super) fn read_failure(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::AuthRequired => "the saved session is not signed in any more",
        ErrorCode::AccountChanged => "the saved session now belongs to a different account",
        ErrorCode::BridgeUnavailable => "the community account bridge is not available",
        ErrorCode::Timeout => "the service did not answer in time",
        ErrorCode::InsecureConfig => "the account directory is not private",
        ErrorCode::InvalidRequest => "the request was refused before it was sent",
        _ => "the service answered in a shape this app cannot read",
    }
}

fn set_busy(ui: &Ui, busy: bool) {
    ui.busy.set(busy);
    if busy {
        ui.spinner.start();
    } else {
        ui.spinner.stop();
    }
}

fn render(ui: &Rc<Ui>) {
    while let Some(row) = ui.list.first_child() {
        ui.list.remove(&row);
    }
    while let Some(row) = ui.connectors.first_child() {
        ui.connectors.remove(&row);
    }
    let Some(settings) = ui.settings.borrow().clone() else {
        return;
    };
    for entry in &settings.entries {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = label(&entry.label);
        name.set_hexpand(true);
        line.append(&name);
        line.append(&label(&entry.value));
        ui.list.append(&line);
    }
    let armed = ui.armed.borrow().clone();
    for connector in &settings.connectors {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = label(connector);
        name.set_hexpand(true);
        line.append(&name);
        let is_armed = armed.as_deref() == Some(connector.as_str());
        let button = gtk::Button::with_label(if is_armed { "Confirm reset" } else { "Reset" });
        button.set_widget_name(&format!("connector-reset:{connector}"));
        if is_armed {
            button.add_css_class("destructive-action");
        }
        let connector = connector.clone();
        let row_ui = Rc::clone(ui);
        button.connect_clicked(move |_| reset_press(&row_ui, &connector));
        line.append(&button);
        ui.connectors.append(&line);
    }
}

fn refresh(ui: &Rc<Ui>) {
    if ui.busy.get() || ui.acting.get() {
        ui.notice
            .set_text("The Policy page is already working. Wait for it to finish.");
        return;
    }
    let Some(config) = (ui.context)() else {
        ui.notice.set_text(
            "Connect your Muse account first: the policy belongs to the connected account. Nothing was read.",
        );
        return;
    };
    ui.epoch.set(ui.epoch.get().wrapping_add(1));
    let epoch = ui.epoch.get();
    set_busy(ui, true);
    ui.notice.set_text("Reading the policy\u{2026}");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = AccountClient::new(config).policy_settings();
        let _ = tx.send(result);
    });
    poll(ui, rx, epoch);
}

fn poll(ui: &Rc<Ui>, rx: mpsc::Receiver<Result<PolicySettings, AccountError>>, epoch: u64) {
    let weak = Rc::downgrade(ui);
    gtk::glib::timeout_add_local(Duration::from_millis(80), move || {
        let Some(ui) = weak.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        match rx.try_recv() {
            Ok(result) => {
                if ui.epoch.get() != epoch {
                    return gtk::glib::ControlFlow::Break;
                }
                set_busy(&ui, false);
                match result {
                    Ok(settings) => {
                        let text = summary_text(&settings);
                        *ui.settings.borrow_mut() = Some(settings);
                        *ui.armed.borrow_mut() = None;
                        render(&ui);
                        ui.notice.set_text(&text);
                    }
                    Err(error) => {
                        ui.notice.set_text(&format!(
                            "Policy read failed: {}. Nothing was changed.",
                            read_failure(error.code)
                        ));
                    }
                }
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                if ui.epoch.get() == epoch {
                    set_busy(&ui, false);
                    ui.notice.set_text(
                        "The account worker vanished before answering. Nothing was read.",
                    );
                }
                gtk::glib::ControlFlow::Break
            }
        }
    });
}

/// One press on a connector row's Reset: the first arms, the
/// second sends exactly one reset, and the answer is proven by
/// the re-read that follows.
fn reset_press(ui: &Rc<Ui>, connector: &str) {
    if ui.busy.get() || ui.acting.get() {
        return;
    }
    if ui.armed.borrow().as_deref() != Some(connector) {
        *ui.armed.borrow_mut() = Some(connector.to_string());
        ui.notice.set_text(&reset_arm_text(connector));
        render(ui);
        return;
    }
    *ui.armed.borrow_mut() = None;
    let Some(config) = (ui.context)() else {
        ui.notice.set_text(
            "Connect your Muse account first: resetting belongs to the connected account. Nothing was sent.",
        );
        render(ui);
        return;
    };
    ui.acting.set(true);
    ui.epoch.set(ui.epoch.get().wrapping_add(1));
    let epoch = ui.epoch.get();
    ui.spinner.start();
    ui.notice
        .set_text(&format!("Resetting \"{connector}\"\u{2026}"));
    let (tx, rx) = mpsc::channel();
    let connector_owned = connector.to_string();
    let connector_for_thread = connector.to_string();
    std::thread::spawn(move || {
        let result = AccountClient::new(config).connector_reset(&connector_for_thread);
        let _ = tx.send(result);
    });
    let weak = Rc::downgrade(ui);
    gtk::glib::timeout_add_local(Duration::from_millis(80), move || {
        let Some(ui) = weak.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        match rx.try_recv() {
            Ok(Ok(_)) => {
                if ui.epoch.get() != epoch {
                    return gtk::glib::ControlFlow::Break;
                }
                ui.acting.set(false);
                ui.spinner.stop();
                ui.notice.set_text(&format!(
                    "Reset sent for \"{connector_owned}\". Re-reading the policy to show its state\u{2026}"
                ));
                refresh(&ui);
                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                if ui.epoch.get() == epoch {
                    ui.acting.set(false);
                    ui.spinner.stop();
                    render(&ui);
                    ui.notice.set_text(&format!(
                        "Reset failed: {}. Nothing was changed.",
                        read_failure(error.code)
                    ));
                }
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                if ui.epoch.get() == epoch {
                    ui.acting.set(false);
                    ui.spinner.stop();
                    ui.notice.set_text(
                        "The account worker vanished before answering. The reset may or may not have been sent; refresh to see the connector's state.",
                    );
                }
                gtk::glib::ControlFlow::Break
            }
        }
    });
}

pub fn build(context: AccountContext) -> PolicyPage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("page-content");
    let heading = label("Policy \u{b7} the service's durable settings");
    heading.add_css_class("page-heading");
    root.append(&heading);
    let intro = label(
        "The settings your Muse's service holds, read when you open this page and on Refresh, shown in the service's own terms. Changing a setting stays on the Muse side for now. A connector the settings name can be reset from here: resetting clears its grants, and the re-read afterwards shows the state that resulted.",
    );
    intro.add_css_class("page-subtitle");
    root.append(&intro);

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.add_css_class("toolbar");
    let refresh_button = gtk::Button::with_label("Refresh policy");
    refresh_button.add_css_class("suggested-action");
    let spinner = gtk::Spinner::new();
    toolbar.append(&refresh_button);
    toolbar.append(&spinner);
    root.append(&toolbar);

    let notice = label("Nothing read yet.");
    root.append(&notice);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_min_content_height(180);
    scroll.set_child(Some(&list));
    root.append(&scroll);

    let connectors_heading = label("Connector grants");
    root.append(&connectors_heading);
    let connectors = gtk::ListBox::new();
    connectors.set_selection_mode(gtk::SelectionMode::None);
    let connectors_scroll = gtk::ScrolledWindow::new();
    connectors_scroll.set_min_content_height(72);
    connectors_scroll.set_child(Some(&connectors));
    root.append(&connectors_scroll);

    let ui = Rc::new(Ui {
        epoch: Cell::new(0),
        busy: Cell::new(false),
        acting: Cell::new(false),
        context,
        list,
        connectors,
        notice,
        spinner,
        settings: RefCell::new(None),
        armed: RefCell::new(None),
    });
    {
        let ui = Rc::clone(&ui);
        refresh_button.connect_clicked(move |_| refresh(&ui));
    }

    let load = {
        let weak = Rc::downgrade(&ui);
        Rc::new(move || {
            if let Some(ui) = weak.upgrade() {
                refresh(&ui);
            }
        }) as Rc<dyn Fn()>
    };
    PolicyPage {
        root,
        load,
        #[cfg(test)]
        ui,
    }
}

#[cfg(test)]
mod tests {
    use super::{read_failure, render, reset_arm_text, summary_text};
    use gtk::prelude::*;
    use gtk4 as gtk;
    use muse_account::{ErrorCode, PolicyEntry, PolicySettings};
    use std::rc::Rc;

    fn settings(entries: usize, withheld: u64, connectors: &[&str]) -> PolicySettings {
        PolicySettings {
            entries: (0..entries)
                .map(|i| PolicyEntry {
                    label: format!("section.key{i}"),
                    value: "true".into(),
                })
                .collect(),
            connectors: connectors.iter().map(|name| name.to_string()).collect(),
            withheld,
        }
    }

    #[test]
    fn the_summary_counts_settings_and_names_what_was_withheld() {
        assert_eq!(
            summary_text(&settings(0, 0, &[])),
            "The service reports no readable policy settings."
        );
        assert!(summary_text(&settings(1, 0, &[])).contains("1 policy setting."));
        let partial = summary_text(&settings(3, 2, &[]));
        assert!(partial.contains("3 policy settings."), "{partial}");
        assert!(partial.contains("2 further values"), "{partial}");
        assert!(partial.contains("were not listed"), "{partial}");
        let one = summary_text(&settings(3, 1, &[]));
        assert!(one.contains("1 further value "), "{one}");
        assert!(one.contains("was not listed"), "{one}");
    }

    #[test]
    fn arming_a_reset_explains_it_before_anything_is_sent() {
        let text = reset_arm_text("gmail");
        assert!(text.contains("clears the grants"), "{text}");
        assert!(text.contains("stays connected"), "{text}");
        assert!(text.contains("Confirm reset"), "{text}");
    }

    fn named_button(root: &gtk::Widget, name: &str) -> Option<gtk::Button> {
        let mut stack = vec![root.clone()];
        while let Some(widget) = stack.pop() {
            if let Ok(btn) = widget.clone().downcast::<gtk::Button>()
                && btn.widget_name() == name
            {
                return Some(btn);
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                stack.push(current.clone());
                child = current.next_sibling();
            }
        }
        None
    }

    #[test]
    #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
    fn an_unconnected_refresh_reads_nothing_and_says_so() {
        gtk::init().unwrap();
        let page = super::build(Rc::new(|| None));
        (page.load)();
        assert!(
            page.ui
                .notice
                .text()
                .contains("Connect your Muse account first"),
            "{}",
            page.ui.notice.text()
        );
        assert!(page.ui.list.first_child().is_none());
        assert!(page.ui.connectors.first_child().is_none());
    }

    #[test]
    #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
    fn a_connector_row_carries_a_reset_button_that_arms_before_it_sends() {
        gtk::init().unwrap();
        let page = super::build(Rc::new(|| None));
        let ui = Rc::clone(&page.ui);
        *page.ui.settings.borrow_mut() = Some(settings(1, 0, &["gmail"]));
        render(&ui);
        let root: &gtk::Widget = page.root.upcast_ref();
        let reset = named_button(root, "connector-reset:gmail")
            .expect("the connector row carries its reset button");
        assert_eq!(reset.label().as_deref(), Some("Reset"));
        assert!(
            page.ui.list.first_child().is_some(),
            "the setting row renders"
        );
        // Arming is a render state plus the explanation, not a send:
        // the same press target now asks for confirmation.
        reset.emit_clicked();
        let armed = named_button(root, "connector-reset:gmail")
            .expect("the armed row still carries its button");
        assert_eq!(armed.label().as_deref(), Some("Confirm reset"));
        assert!(
            page.ui.notice.text().contains("clears the grants"),
            "{}",
            page.ui.notice.text()
        );
    }

    #[test]
    fn failures_are_named_and_never_echo_service_text() {
        assert!(read_failure(ErrorCode::AuthRequired).contains("signed in"));
        for code in [
            ErrorCode::AuthRequired,
            ErrorCode::AccountChanged,
            ErrorCode::BridgeUnavailable,
            ErrorCode::Timeout,
            ErrorCode::ProtocolError,
            ErrorCode::InsecureConfig,
            ErrorCode::InvalidRequest,
        ] {
            let text = read_failure(code);
            assert!(!text.is_empty());
            assert!(!text.contains("SECRET"));
        }
    }
}
