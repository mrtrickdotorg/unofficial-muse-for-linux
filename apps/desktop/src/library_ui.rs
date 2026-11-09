//! Read-only Library page: the artifacts the connected account has saved.
//!
//! Listing is never consent. This page reads `artifacts.list` once per explicit
//! refresh, on a worker thread, and cannot open, download, rename, share or
//! delete anything: those actions stay on the Muse side. Rows are ordered
//! pinned-first because a pin is a choice the person made and a recency sort
//! would bury it, and the notice repeats the service's own total so a page of
//! 50 never reads as the whole Library.

use crate::local_host::AccountContext;
use gtk::prelude::*;
use gtk4 as gtk;
use muse_account::{AccountClient, AccountError, ArtifactList, ArtifactSummary, ErrorCode};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

pub struct LibraryPage {
    pub root: gtk::Box,
    #[cfg(test)]
    pub(super) ui: Rc<Ui>,
}

pub(super) struct Ui {
    epoch: Cell<u64>,
    busy: Cell<bool>,
    context: AccountContext,
    list: gtk::ListBox,
    notice: gtk::Label,
    spinner: gtk::Spinner,
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

    fn render(&self, rows: &[ArtifactSummary]) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        for row in rows {
            self.list.append(&label(&row_text(row)));
        }
    }

    fn refresh(&self, ui: &Rc<Ui>) {
        if self.busy.get() {
            self.notice
                .set_text("A Library read is already running. Wait for it to finish.");
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
        self.poll(ui, rx, epoch);
    }

    fn poll(
        &self,
        ui: &Rc<Ui>,
        rx: mpsc::Receiver<Result<ArtifactList, AccountError>>,
        epoch: u64,
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
                    ui.finish(result);
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

    fn finish(&self, result: Result<ArtifactList, AccountError>) {
        match result {
            Ok(page) => {
                let rows = order_rows(page.artifacts.clone());
                self.render(&rows);
                let summary = summary_text(&page, rows.len());
                self.notice
                    .set_text(&if rows.is_empty() && page.total_count > 0 {
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
}

pub fn build(context: AccountContext) -> LibraryPage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("page-content");
    let heading = label("Library · saved artifacts, read-only");
    heading.add_css_class("page-heading");
    root.append(&heading);
    let intro = label(
        "One press reads the artifact list for the connected account. This page cannot open, download, rename, share or delete anything: those actions stay on the Muse side. Nothing is fetched until you press Refresh Library, and no row is kept after the window closes.",
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

    let notice = label("Nothing read yet. Press Refresh Library.");
    root.append(&notice);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_min_content_height(220);
    scroll.set_child(Some(&list));
    root.append(&scroll);

    let ui = Rc::new(Ui {
        epoch: Cell::new(0),
        busy: Cell::new(false),
        context,
        list,
        notice,
        spinner,
    });
    {
        let ui = Rc::clone(&ui);
        refresh.connect_clicked(move |_| ui.refresh(&ui));
    }

    LibraryPage {
        root,
        #[cfg(test)]
        ui,
    }
}

#[cfg(test)]
mod tests {
    use super::{order_rows, read_failure, row_text, summary_text, utc_date};
    use gtk::prelude::*;
    use gtk4 as gtk;
    use muse_account::{ArtifactList, ArtifactSummary, ErrorCode};
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
