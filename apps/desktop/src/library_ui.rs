//! Library page: the artifacts and spaces the connected account has saved.
//!
//! Listing is never consent. The page reads `artifacts.list` when it becomes
//! visible and on explicit refresh, on a worker thread. Paging follows the
//! service's own pagination answer: "Show more" hands the bridge the cursor
//! the last page issued plus the references already shown, and the bridge
//! accepts a follow-up page only when its rows provably continue the list.
//! A space row can be unshared from here — one armed press, one write, then
//! a re-read to prove the state — and previewed from the service's own HTML
//! export in a locked frame like the Feed's. Downloading and deleting stay on
//! the Muse side. Rows are ordered pinned-first because a pin is a choice
//! the person made and a recency sort would bury it, and the notice repeats
//! the service's own total so a page of 50 never reads as the whole Library.

use crate::local_host::AccountContext;
use gtk::prelude::*;
use gtk4 as gtk;
use muse_account::{
    AccountClient, AccountError, ArtifactList, ArtifactSummary, ErrorCode, SpaceExportAnswer,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

pub struct LibraryPage {
    pub root: gtk::Box,
    /// Loads (or re-reads) the artifact list; fired by the shell when
    /// the Library tab is opened.
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
    notice: gtk::Label,
    spinner: gtk::Spinner,
    more: gtk::Button,
    /// Every row read so far, in the service's own order; the
    /// display sorts a copy, so paging appends never reshuffle
    /// what earlier pages established.
    rows: RefCell<Vec<ArtifactSummary>>,
    total: Cell<u64>,
    has_more: Cell<bool>,
    /// The service said there is more, but no candidate paging
    /// parameter continued the list; "Show more" retires for
    /// this read rather than repeating the first page.
    paging_dead: Cell<bool>,
    next_cursor: RefCell<Option<String>>,
    /// The slug whose Unshare press is armed, if any. Arming is
    /// the confirmation: the second press on the same row sends.
    armed: RefCell<Option<String>>,
    /// The open preview window, if any; a new preview replaces
    /// it rather than stacking a window per press.
    preview_window: RefCell<Option<gtk::Window>>,
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_selectable(true);
    label
}

/// Pinned first, then most recently updated, then title.
///
/// An artifact without a timestamp sorts after every dated one: it is unknown,
/// not newest. The ordering is stable, so equal artifacts keep the service's
/// own order rather than shuffling between refreshes.
pub(super) fn order_rows(mut rows: Vec<ArtifactSummary>) -> Vec<ArtifactSummary> {
    rows.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| {
                b.updated_at_ms
                    .unwrap_or(i64::MIN)
                    .cmp(&a.updated_at_ms.unwrap_or(i64::MIN))
            })
            .then_with(|| a.title.cmp(&b.title))
    });
    rows
}

/// One row's text. Format wins over kind because a person recognises "docx"
/// faster than "document"; a row carrying neither still says what it is.
pub(super) fn row_text(row: &ArtifactSummary) -> String {
    let kind = row
        .format
        .as_ref()
        .or(row.kind.as_ref())
        .map(String::as_str)
        .unwrap_or("artifact");
    let when = row
        .updated_at_ms
        .map(|ms| format!(" · updated {}", utc_date(ms)))
        .unwrap_or_default();
    format!(
        "{}{} · {}{}",
        if row.pinned { "★ " } else { "" },
        row.title,
        kind,
        when
    )
}

/// The slug a row's space actions address, when the row is a
/// space: the open target is the recorded discriminator (a file
/// row's target kind is "file"), and the slug is the handle its
/// routes take. A space row without a slug gets no action --
/// there would be nothing honest to address.
pub(super) fn space_slug(row: &ArtifactSummary) -> Option<&str> {
    if row.target_kind.as_deref() == Some("space") {
        row.slug.as_deref().filter(|slug| !slug.is_empty())
    } else {
        None
    }
}

/// What the last read reported, in the service's own terms. The total is the
/// service's, the count is this page's, and "more" is the service's pagination
/// answer rather than a guess from the page size.
pub(super) fn summary_text(page: &ArtifactList, shown: usize) -> String {
    if page.total_count == 0 {
        return "The service reports no artifacts in this account's Library.".into();
    }
    let pinned = page.artifacts.iter().filter(|row| row.pinned).count();
    let mut text = format!("Showing {shown} of {}", page.total_count);
    if pinned > 0 {
        text.push_str(&format!(" · {pinned} pinned"));
    }
    if page.has_more {
        text.push_str(" · the service has more than this page");
    }
    text
}

/// The sentence appended when the service has more rows but
/// paging did not continue: an honest stop, not a silent one.
pub(super) fn paging_note(paging_dead: bool, has_more: bool) -> &'static str {
    if paging_dead && has_more {
        " Paging did not continue past this page, so the rest stays on the Muse side for now."
    } else {
        ""
    }
}

/// Whole kilobytes, rounded up: the magnitude is the point.
fn size_kb(size: u64) -> u64 {
    size.div_ceil(1024)
}

/// The notice a finished export read leaves when it cannot open
/// a preview, or None when the preview opens. The sizes are the
/// service's own byte counts; a withheld page is named as
/// withheld, never as missing.
pub(super) fn preview_notice(answer: &SpaceExportAnswer, title: &str) -> Option<String> {
    if answer.truncated {
        return Some(format!(
            "The export of \"{title}\" is {} KB -- too large to preview in the app. It stays available on the Muse side.",
            size_kb(answer.size)
        ));
    }
    match answer.format.as_str() {
        "html" if answer.html.is_some() => None,
        "binary" => Some(format!(
            "The export of \"{title}\" came back as binary data ({} KB), not a page, so there is nothing to preview.",
            size_kb(answer.size)
        )),
        _ => Some(format!(
            "The export of \"{title}\" did not come back as a page, so there is nothing to preview."
        )),
    }
}

/// Map a typed failure to a sentence. The error's own text never reaches the
/// page: the bridge's codes are fixed, its details are not, and a detail could
/// quote service output.
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

/// UTC calendar date for a millisecond timestamp, without a date-time library.
pub(super) fn utc_date(ms: i64) -> String {
    let days = ms.div_euclid(1000).div_euclid(86_400);
    // Howard Hinnant's civil_from_days: days since 1970-01-01 to y/m/d.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

impl Ui {
    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        if busy {
            self.spinner.start();
        } else {
            self.spinner.stop();
        }
    }

    fn render(&self, ui: &Rc<Ui>) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        let armed = self.armed.borrow().clone();
        for row in order_rows(self.rows.borrow().clone()) {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let text = label(&row_text(&row));
            text.set_hexpand(true);
            line.append(&text);
            if let Some(slug) = space_slug(&row) {
                let preview = gtk::Button::with_label("Preview");
                preview.set_widget_name(&format!("space-preview:{slug}"));
                let preview_slug = slug.to_string();
                let preview_title = row.title.clone();
                let preview_weak = Rc::downgrade(ui);
                preview.connect_clicked(move |_| {
                    if let Some(ui) = preview_weak.upgrade() {
                        ui.preview_press(&ui, &preview_slug, &preview_title);
                    }
                });
                line.append(&preview);
                let is_armed = armed.as_deref() == Some(slug);
                let button = gtk::Button::with_label(if is_armed {
                    "Confirm unshare"
                } else {
                    "Unshare"
                });
                button.set_widget_name(&format!("space-unshare:{slug}"));
                if is_armed {
                    button.add_css_class("destructive-action");
                }
                let slug = slug.to_string();
                let title = row.title.clone();
                let weak = Rc::downgrade(ui);
                button.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.unshare_press(&ui, &slug, &title);
                    }
                });
                line.append(&button);
            }
            self.list.append(&line);
        }
        self.more.set_visible(
            self.has_more.get() && !self.paging_dead.get() && self.next_cursor.borrow().is_some(),
        );
    }

    fn summary(&self) -> String {
        let accumulated = self.rows.borrow();
        let page = ArtifactList {
            total_count: self.total.get(),
            has_more: self.has_more.get() && !self.paging_dead.get(),
            next_cursor: None,
            paging: None,
            artifacts: accumulated.clone(),
        };
        let mut text = summary_text(&page, accumulated.len());
        text.push_str(paging_note(self.paging_dead.get(), self.has_more.get()));
        text
    }

    fn refresh(&self, ui: &Rc<Ui>) {
        if self.busy.get() || self.acting.get() {
            self.notice
                .set_text("The Library is already working. Wait for it to finish.");
            return;
        }
        let Some(config) = (self.context)() else {
            self.notice.set_text(
                "Connect your Muse account first: the Library belongs to the connected account. Nothing was read.",
            );
            return;
        };
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let epoch = self.epoch.get();
        self.set_busy(true);
        self.notice.set_text("Reading the Library…");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = AccountClient::new(config).artifacts();
            let _ = tx.send(result);
        });
        self.poll(ui, rx, epoch, false);
    }

    fn show_more(&self, ui: &Rc<Ui>) {
        if self.busy.get() || self.acting.get() {
            return;
        }
        let Some(cursor) = self.next_cursor.borrow().clone() else {
            return;
        };
        let Some(config) = (self.context)() else {
            self.notice.set_text(
                "Connect your Muse account first: the Library belongs to the connected account. Nothing was read.",
            );
            return;
        };
        let seen: Vec<String> = self
            .rows
            .borrow()
            .iter()
            .map(|row| row.artifact_ref.clone())
            .collect();
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let epoch = self.epoch.get();
        self.set_busy(true);
        self.notice.set_text("Reading the next page…");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = AccountClient::new(config).artifacts_page(&cursor, &seen);
            let _ = tx.send(result);
        });
        self.poll(ui, rx, epoch, true);
    }

    fn poll(
        &self,
        ui: &Rc<Ui>,
        rx: mpsc::Receiver<Result<ArtifactList, AccountError>>,
        epoch: u64,
        append: bool,
    ) {
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
                    ui.set_busy(false);
                    ui.finish(result, append, &ui);
                    gtk::glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if ui.epoch.get() == epoch {
                        ui.set_busy(false);
                        ui.notice.set_text(
                            "The account worker vanished before answering. Nothing was read.",
                        );
                    }
                    gtk::glib::ControlFlow::Break
                }
            }
        });
    }

    fn finish(&self, result: Result<ArtifactList, AccountError>, append: bool, ui: &Rc<Ui>) {
        match result {
            Ok(page) => {
                if append && page.paging.as_deref() == Some("unavailable") {
                    // No candidate parameter continued the list:
                    // keep what is shown, retire the button, and
                    // say so rather than repeating page one.
                    self.paging_dead.set(true);
                    self.has_more.set(true);
                    self.render(ui);
                    self.notice.set_text(&self.summary());
                    return;
                }
                if append {
                    let mut rows = self.rows.borrow_mut();
                    for row in page.artifacts {
                        if !rows
                            .iter()
                            .any(|held| held.artifact_ref == row.artifact_ref)
                        {
                            rows.push(row);
                        }
                    }
                } else {
                    *self.rows.borrow_mut() = page.artifacts.clone();
                    self.paging_dead.set(false);
                    *self.armed.borrow_mut() = None;
                }
                self.total.set(page.total_count);
                self.has_more.set(page.has_more);
                *self.next_cursor.borrow_mut() = page.next_cursor.clone();
                self.render(ui);
                let shown = self.rows.borrow().len();
                let summary = self.summary();
                self.notice
                    .set_text(&if shown == 0 && page.total_count > 0 {
                        format!(
                            "{summary}. This page carried no usable rows, so nothing is listed."
                        )
                    } else {
                        summary
                    });
            }
            Err(error) => {
                self.notice.set_text(&format!(
                    "Library read failed: {}. Nothing was changed.",
                    read_failure(error.code)
                ));
            }
        }
    }

    /// One press on a space row's Unshare: the first arms, the
    /// second sends exactly one write, and the answer is proven
    /// by the re-read that follows.
    fn unshare_press(&self, ui: &Rc<Ui>, slug: &str, title: &str) {
        if self.busy.get() || self.acting.get() {
            return;
        }
        if self.armed.borrow().as_deref() != Some(slug) {
            *self.armed.borrow_mut() = Some(slug.to_string());
            self.notice.set_text(&format!(
                "Unsharing stops sharing \"{title}\" with anyone it was shared with; the space itself stays yours. Press Confirm unshare on that row to send it."
            ));
            self.render(ui);
            return;
        }
        *self.armed.borrow_mut() = None;
        let Some(config) = (self.context)() else {
            self.notice.set_text(
                "Connect your Muse account first: unsharing belongs to the connected account. Nothing was sent.",
            );
            self.render(ui);
            return;
        };
        self.acting.set(true);
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let epoch = self.epoch.get();
        self.spinner.start();
        self.notice.set_text(&format!("Unsharing \"{title}\"…"));
        let (tx, rx) = mpsc::channel();
        let slug_owned = slug.to_string();
        let title_owned = title.to_string();
        std::thread::spawn(move || {
            let result = AccountClient::new(config).space_unshare(&slug_owned);
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
                        "Unshare sent for \"{title_owned}\". Re-reading the Library to show its state…"
                    ));
                    ui.refresh(&ui);
                    gtk::glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    if ui.epoch.get() == epoch {
                        ui.acting.set(false);
                        ui.spinner.stop();
                        ui.render(&ui);
                        ui.notice.set_text(&format!(
                            "Unshare failed: {}. Nothing was changed.",
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
                            "The account worker vanished before answering. The unshare may or may not have been sent; refresh to see the space's state.",
                        );
                    }
                    gtk::glib::ControlFlow::Break
                }
            }
        });
    }

    /// One press on a space row's Preview: one read of the
    /// space's own HTML export, then either the preview window
    /// or the honest reason there is none (see preview_notice).
    fn preview_press(&self, ui: &Rc<Ui>, slug: &str, title: &str) {
        if self.busy.get() || self.acting.get() {
            return;
        }
        let Some(config) = (self.context)() else {
            self.notice.set_text(
                "Connect your Muse account first: previewing belongs to the connected account. Nothing was read.",
            );
            return;
        };
        self.acting.set(true);
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let epoch = self.epoch.get();
        self.spinner.start();
        self.notice
            .set_text(&format!("Reading the export of \"{title}\"..."));
        let (tx, rx) = mpsc::channel();
        let slug_owned = slug.to_string();
        let title_owned = title.to_string();
        std::thread::spawn(move || {
            let result = AccountClient::new(config).space_export(&slug_owned);
            let _ = tx.send(result);
        });
        let weak = Rc::downgrade(ui);
        gtk::glib::timeout_add_local(Duration::from_millis(80), move || {
            let Some(ui) = weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            match rx.try_recv() {
                Ok(Ok(answer)) => {
                    if ui.epoch.get() != epoch {
                        return gtk::glib::ControlFlow::Break;
                    }
                    ui.acting.set(false);
                    ui.spinner.stop();
                    match preview_notice(&answer, &title_owned) {
                        Some(text) => ui.notice.set_text(&text),
                        None => {
                            let html = answer.html.as_deref().unwrap_or_default();
                            ui.open_preview(&ui, &title_owned, html);
                            ui.notice.set_text(&format!(
                                "Previewing \"{title_owned}\" from its export ({} KB).",
                                size_kb(answer.size)
                            ));
                        }
                    }
                    gtk::glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    if ui.epoch.get() == epoch {
                        ui.acting.set(false);
                        ui.spinner.stop();
                        ui.notice.set_text(&format!(
                            "Preview failed: {}. Nothing was changed.",
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
                            "The account worker vanished before answering. Nothing was read.",
                        );
                    }
                    gtk::glib::ControlFlow::Break
                }
            }
        });
    }

    /// Show one space's exported HTML in its own window: a
    /// WebView locked the way the Feed's frames are -- the
    /// exported document itself renders, a real link press hands
    /// its URL to the system browser, and nothing else
    /// navigates. A new preview replaces the old window rather
    /// than stacking them, and a frame that cannot render says
    /// so instead of sitting blank.
    fn open_preview(&self, ui: &Rc<Ui>, title: &str, html: &str) {
        use webkit6::prelude::*;
        use webkit6::{NavigationPolicyDecision, PolicyDecisionType, WebView};

        if let Some(window) = self.preview_window.borrow_mut().take() {
            window.close();
        }
        let window = gtk::Window::new();
        window.set_title(Some(&format!("{title} -- preview")));
        window.set_default_size(920, 660);
        if let Some(root) = self.list.root()
            && let Ok(main_window) = root.downcast::<gtk::Window>()
        {
            window.set_transient_for(Some(&main_window));
        }
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.add_css_class("toolbar");
        let heading = label(&format!("{title} -- exported preview"));
        heading.set_hexpand(true);
        bar.append(&heading);
        let close = gtk::Button::with_label("Close");
        bar.append(&close);
        column.append(&bar);

        let view = WebView::new();
        view.set_hexpand(true);
        view.set_vexpand(true);
        if let Some(settings) = WebViewExt::settings(&view) {
            // JavaScript stays on: a space computes itself at
            // display time, like the web's own rendering of it.
            settings.set_enable_javascript(true);
            settings.set_enable_developer_extras(false);
        }
        view.connect_decide_policy(|_, decision, kind| {
            if kind != PolicyDecisionType::NavigationAction {
                return false;
            }
            let Some(navigation) = decision.downcast_ref::<NavigationPolicyDecision>() else {
                return false;
            };
            let attempted = navigation.navigation_action().and_then(|mut action| {
                let gesture = action.is_user_gesture();
                action
                    .request()
                    .map(|request| (request.uri().unwrap_or_default(), gesture))
            });
            if let Some((uri, gesture)) = attempted {
                if uri == "about:blank" {
                    // The exported document itself (load_html's
                    // base): the one navigation the frame is for.
                    return false;
                }
                if gesture && (uri.starts_with("https://") || uri.starts_with("http://")) {
                    let _ = gtk::gio::AppInfo::launch_default_for_uri(
                        &uri,
                        None::<&gtk::gio::AppLaunchContext>,
                    );
                }
                decision.ignore();
                return true;
            }
            false
        });
        let swapped = Rc::new(Cell::new(false));
        let fallback: Rc<dyn Fn()> = {
            let column = column.clone();
            let view = view.clone();
            let swapped = Rc::clone(&swapped);
            Rc::new(move || {
                if swapped.replace(true) {
                    return;
                }
                column.remove(&view);
                let note = label("The preview could not be rendered in the app.");
                note.set_vexpand(true);
                column.append(&note);
            })
        };
        {
            let fallback = Rc::clone(&fallback);
            view.connect_load_failed(move |_, _, _, _| {
                fallback();
                true
            });
        }
        view.connect_web_process_terminated(move |_, _| fallback());
        column.append(&view);
        {
            let window = window.clone();
            close.connect_clicked(move |_| window.close());
        }
        window.set_child(Some(&column));
        view.load_html(html, None);
        {
            let weak = Rc::downgrade(ui);
            let this = window.clone();
            window.connect_destroy(move |_| {
                if let Some(ui) = weak.upgrade() {
                    let mut slot = ui.preview_window.borrow_mut();
                    if slot.as_ref() == Some(&this) {
                        *slot = None;
                    }
                }
            });
        }
        *self.preview_window.borrow_mut() = Some(window.clone());
        window.present();
    }
}

pub fn build(context: AccountContext) -> LibraryPage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("page-content");
    let heading = label("Library · saved artifacts and spaces");
    heading.add_css_class("page-heading");
    root.append(&heading);
    let intro = label(
        "The list loads when you open this page, and Refresh re-reads it. \"Show more\" follows the service's own paging. A space listed here can be previewed from its own HTML export and unshared from this page; downloading and deleting stay on the Muse side. No row is kept after the window closes.",
    );
    intro.add_css_class("page-subtitle");
    root.append(&intro);

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.add_css_class("toolbar");
    let refresh = gtk::Button::with_label("Refresh Library");
    refresh.add_css_class("suggested-action");
    let spinner = gtk::Spinner::new();
    toolbar.append(&refresh);
    toolbar.append(&spinner);
    root.append(&toolbar);

    let notice = label("Nothing read yet.");
    root.append(&notice);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_min_content_height(220);
    scroll.set_child(Some(&list));
    root.append(&scroll);

    let more = gtk::Button::with_label("Show more");
    more.set_visible(false);
    root.append(&more);

    let ui = Rc::new(Ui {
        epoch: Cell::new(0),
        busy: Cell::new(false),
        acting: Cell::new(false),
        context,
        list,
        notice,
        spinner,
        more,
        rows: RefCell::new(Vec::new()),
        total: Cell::new(0),
        has_more: Cell::new(false),
        paging_dead: Cell::new(false),
        next_cursor: RefCell::new(None),
        armed: RefCell::new(None),
        preview_window: RefCell::new(None),
    });
    {
        let ui = Rc::clone(&ui);
        refresh.connect_clicked(move |_| ui.refresh(&ui));
    }
    {
        let ui = Rc::clone(&ui);
        let more_button = ui.more.clone();
        more_button.connect_clicked(move |_| ui.show_more(&ui));
    }

    let load = {
        let weak = Rc::downgrade(&ui);
        Rc::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.refresh(&ui);
            }
        }) as Rc<dyn Fn()>
    };
    LibraryPage {
        root,
        load,
        #[cfg(test)]
        ui,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        order_rows, paging_note, preview_notice, read_failure, row_text, space_slug, summary_text,
        utc_date,
    };
    use gtk::prelude::*;
    use gtk4 as gtk;
    use muse_account::{ArtifactList, ArtifactSummary, ErrorCode, SpaceExportAnswer};
    use std::rc::Rc;

    fn row(title: &str, pinned: bool, updated: Option<i64>) -> ArtifactSummary {
        ArtifactSummary {
            artifact_ref: format!("file:workspace/{title}"),
            title: title.into(),
            kind: Some("document".into()),
            format: None,
            status: Some("ready".into()),
            placement: Some("library".into()),
            path: Some(format!("/workspace/{title}")),
            target_kind: Some("file".into()),
            slug: None,
            updated_at_ms: updated,
            pinned,
            can_open: true,
            can_share: false,
        }
    }

    fn page(total: u64, rows: Vec<ArtifactSummary>, has_more: bool) -> ArtifactList {
        ArtifactList {
            total_count: total,
            has_more,
            next_cursor: None,
            paging: None,
            artifacts: rows,
        }
    }

    #[test]
    fn a_pin_outranks_a_newer_unpinned_artifact() {
        let ordered = order_rows(vec![
            row("newer.txt", false, Some(2_000)),
            row("pinned.txt", true, Some(1_000)),
            row("middle.txt", false, Some(1_500)),
        ]);
        assert_eq!(
            ordered.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
            ["pinned.txt", "newer.txt", "middle.txt"]
        );
    }

    #[test]
    fn undated_artifacts_sort_last_and_titles_break_ties() {
        let ordered = order_rows(vec![
            row("undated.txt", false, None),
            row("b.txt", false, Some(5)),
            row("a.txt", false, Some(5)),
        ]);
        assert_eq!(
            ordered.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
            ["a.txt", "b.txt", "undated.txt"]
        );
    }

    #[test]
    fn the_summary_repeats_the_service_total_and_its_pagination_answer() {
        let rows = vec![row("a.docx", true, Some(1)), row("b.docx", false, Some(2))];
        let text = summary_text(&page(76, rows, true), 50);
        assert!(text.contains("Showing 50 of 76"), "{text}");
        assert!(text.contains("1 pinned"), "{text}");
        assert!(text.contains("more than this page"), "{text}");
        let quiet = summary_text(&page(2, vec![row("a.docx", false, Some(1))], false), 1);
        assert!(!quiet.contains("pinned"), "{quiet}");
        assert!(!quiet.contains("more"), "{quiet}");
    }

    #[test]
    fn an_empty_library_says_so_instead_of_showing_zero_of_zero() {
        let text = summary_text(&page(0, Vec::new(), false), 0);
        assert!(text.contains("no artifacts"), "{text}");
    }

    #[test]
    fn only_a_space_row_with_a_slug_offers_unshare() {
        let file = row("a.docx", false, None);
        assert_eq!(space_slug(&file), None);
        let mut space = row("My space", false, None);
        space.target_kind = Some("space".into());
        assert_eq!(
            space_slug(&space),
            None,
            "a space without a slug has no handle"
        );
        space.slug = Some("my-space".into());
        assert_eq!(space_slug(&space), Some("my-space"));
        space.slug = Some(String::new());
        assert_eq!(space_slug(&space), None, "an empty slug addresses nothing");
    }

    #[test]
    fn the_paging_note_only_speaks_when_paging_died_with_more_to_show() {
        assert!(!paging_note(true, true).is_empty());
        assert!(paging_note(true, false).is_empty());
        assert!(paging_note(false, true).is_empty());
    }

    #[test]
    fn dates_are_utc_and_before_epoch_dates_do_not_wrap() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_700_000_000_000), "2023-11-14");
        assert_eq!(utc_date(1_793_915_135_823), "2026-11-05");
        assert_eq!(utc_date(-86_400_000), "1969-12-31");
    }

    #[test]
    fn a_row_names_its_format_or_kind_and_marks_pins() {
        let mut csv = row("sheet.csv", true, Some(1_700_000_000_000));
        csv.format = Some("csv".into());
        let text = row_text(&csv);
        assert!(text.starts_with("★ sheet.csv"), "{text}");
        assert!(text.contains(" · csv"), "{text}");
        assert!(text.contains("updated 2023-11-14"), "{text}");
        let plain = row_text(&row("undated", false, None));
        assert!(plain.contains(" · document"), "{plain}");
        assert!(!plain.contains("updated"), "{plain}");
    }

    fn button(root: &gtk::Widget, name: &str) -> gtk::Button {
        let mut stack = vec![root.clone()];
        while let Some(widget) = stack.pop() {
            if let Ok(btn) = widget.clone().downcast::<gtk::Button>()
                && btn.label().as_deref() == Some(name)
            {
                return btn;
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                stack.push(current.clone());
                child = current.next_sibling();
            }
        }
        panic!("button {name} not found")
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
        assert!(page.ui.list.first_child().is_none());
        let root: &gtk::Widget = page.root.upcast_ref();
        button(root, "Refresh Library").emit_clicked();
        assert!(
            page.ui
                .notice
                .text()
                .contains("Connect your Muse account first"),
            "{}",
            page.ui.notice.text()
        );
        // No bridge was started, so no row can appear from an unanswered read.
        assert!(page.ui.list.first_child().is_none());
    }

    #[test]
    #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
    fn a_space_row_carries_an_unshare_button_and_a_file_row_does_not() {
        gtk::init().unwrap();
        let page = super::build(Rc::new(|| None));
        let ui = Rc::clone(&page.ui);
        let mut space = row("Shared space", false, Some(9));
        space.target_kind = Some("space".into());
        space.slug = Some("shared-space".into());
        *page.ui.rows.borrow_mut() = vec![space, row("plain.docx", false, Some(8))];
        page.ui.render(&ui);
        let root: &gtk::Widget = page.root.upcast_ref();
        let unshare = named_button(root, "space-unshare:shared-space")
            .expect("the space row carries its unshare button");
        assert_eq!(unshare.label().as_deref(), Some("Unshare"));
        // Arming is a render state, not a send: the same press
        // target now asks for confirmation.
        *page.ui.armed.borrow_mut() = Some("shared-space".into());
        page.ui.render(&ui);
        let armed = named_button(root, "space-unshare:shared-space")
            .expect("the armed row still carries its button");
        assert_eq!(armed.label().as_deref(), Some("Confirm unshare"));
    }

    #[test]
    fn a_preview_opens_only_for_an_html_export_that_carried_its_page() {
        let answer =
            |format: &str, size: u64, truncated: bool, html: Option<&str>| SpaceExportAnswer {
                slug: "s".into(),
                format: format.into(),
                size,
                truncated,
                html: html.map(str::to_string),
            };
        assert_eq!(
            preview_notice(&answer("html", 10, false, Some("<html>")), "T"),
            None
        );
        let large = preview_notice(&answer("html", 900 * 1024, true, None), "T")
            .expect("a truncated export explains itself");
        assert!(large.contains("900 KB"), "{large}");
        assert!(large.contains("too large"), "{large}");
        let binary = preview_notice(&answer("binary", 2048, false, None), "T")
            .expect("a binary export explains itself");
        assert!(binary.contains("binary data (2 KB)"), "{binary}");
        let text = preview_notice(&answer("text", 12, false, Some("words")), "T")
            .expect("a text export explains itself");
        assert!(text.contains("did not come back as a page"), "{text}");
    }

    #[test]
    #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
    fn a_space_row_carries_a_preview_button_and_an_unconnected_press_reads_nothing() {
        gtk::init().unwrap();
        let page = super::build(Rc::new(|| None));
        let ui = Rc::clone(&page.ui);
        let mut space = row("Shared space", false, Some(9));
        space.target_kind = Some("space".into());
        space.slug = Some("shared-space".into());
        *page.ui.rows.borrow_mut() = vec![space, row("plain.docx", false, Some(8))];
        page.ui.render(&ui);
        let root: &gtk::Widget = page.root.upcast_ref();
        let preview = named_button(root, "space-preview:shared-space")
            .expect("the space row carries its preview button");
        assert_eq!(preview.label().as_deref(), Some("Preview"));
        preview.emit_clicked();
        assert!(
            page.ui
                .notice
                .text()
                .contains("Connect your Muse account first"),
            "{}",
            page.ui.notice.text()
        );
        assert!(page.ui.preview_window.borrow().is_none());
    }

    #[test]
    fn failures_are_named_and_never_echo_service_text() {
        assert_ne!(
            read_failure(ErrorCode::AuthRequired),
            read_failure(ErrorCode::Timeout)
        );
        assert!(read_failure(ErrorCode::AuthRequired).contains("signed in"));
        for code in [
            ErrorCode::AuthRequired,
            ErrorCode::AccountChanged,
            ErrorCode::BridgeUnavailable,
            ErrorCode::Timeout,
            ErrorCode::ProtocolError,
            ErrorCode::InsecureConfig,
            ErrorCode::InvalidRequest,
            ErrorCode::VmUnavailable,
            ErrorCode::BridgeFailure,
            ErrorCode::ResponseTooLarge,
        ] {
            let text = read_failure(code);
            assert!(!text.is_empty());
            assert!(!text.contains("SECRET"));
        }
    }
}
