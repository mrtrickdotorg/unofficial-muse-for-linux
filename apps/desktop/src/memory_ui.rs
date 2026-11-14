//! Memory page: what the connected account's agent remembers.
//!
//! `memory.search` is a recorded POST, and its body shape was probed and
//! written down on Nov 14, 2026: `{"query": …}` is the only form the service
//! answers — `q`, query parameters, an empty body and an empty query are all
//! refused. `memory.get` resolves one hit's `memory_uri` back to the text it
//! cited. Both calls are reads.
//!
//! There is deliberately no Forget here. The route table carries exactly four
//! memory routes — `memory.search`, `memory.get`, `memory.consolidate` and
//! `memory.reindex` — and none of them deletes or edits an entry, so a memory
//! this page could "forget" would be a claim the service never made. The page
//! reads, searches and shows; its copy says where forgetting really happens
//! rather than offering a button that cannot work.

use crate::local_host::AccountContext;
use gtk::prelude::*;
use gtk4 as gtk;
use muse_account::{AccountClient, ErrorCode, MemoryEntry, MemoryHit, MemorySearch};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

pub struct MemoryPage {
    pub root: gtk::Box,
    /// Readies the page when the shell shows it: the query entry takes
    /// focus, so Memory lands ready to type.
    pub load: Rc<dyn Fn()>,
    #[cfg(test)]
    pub(super) ui: Rc<Ui>,
}

pub(super) struct Ui {
    epoch: Cell<u64>,
    busy: Cell<bool>,
    context: AccountContext,
    entry: gtk::Entry,
    list: gtk::ListBox,
    notice: gtk::Label,
    detail: gtk::Label,
    spinner: gtk::Spinner,
    /// The hits the last search returned, indexed by the row button that
    /// opens them: a press carries the index, never a memory URI parsed
    /// out of a widget name.
    hits: RefCell<Vec<MemoryHit>>,
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_selectable(true);
    label
}

/// The line a hit is scanned by: its first line with any text in it.
pub(super) fn hit_headline(hit: &MemoryHit) -> String {
    match hit.text.lines().find(|line| !line.trim().is_empty()) {
        Some(line) => line.trim().to_string(),
        None => "A remembered passage".to_string(),
    }
}

/// Where the passage came from, in the service's own citation terms.
pub(super) fn citation_text(hit: &MemoryHit) -> Option<String> {
    let path = hit.path.as_deref()?.trim();
    if path.is_empty() {
        return None;
    }
    Some(match (hit.line_start, hit.line_end) {
        (Some(start), Some(end)) if start == end => format!("{path}:{start}"),
        (Some(start), Some(end)) => format!("{path}:{start}\u{2013}{end}"),
        (Some(start), None) => format!("{path}:{start}"),
        _ => path.to_string(),
    })
}

/// The quiet meta line: the kind of memory and its service status, when
/// the service sent either.
pub(super) fn hit_meta(hit: &MemoryHit) -> Option<String> {
    let parts: Vec<String> = [hit.source_kind.as_deref(), hit.status.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// The count line after a search: the service's own count of readable
/// memories, and — when it sent rows this build could not read — that
/// they were counted as unreadable rather than silently dropped.
pub(super) fn matched_text(search: &MemorySearch, query: &str) -> String {
    let mut text = match search.recognized {
        0 => format!("No memories matched \u{201c}{query}\u{201d}."),
        1 => format!("1 memory matched \u{201c}{query}\u{201d}."),
        n => format!("{n} memories matched \u{201c}{query}\u{201d}."),
    };
    if search.skipped > 0 {
        text.push_str(&format!(
            " {} memor{} the service sent could not be read and {} not listed.",
            search.skipped,
            if search.skipped == 1 { "y" } else { "ies" },
            if search.skipped == 1 { "was" } else { "were" }
        ));
    }
    text
}

/// What the detail pane says about one resolved entry.
pub(super) fn entry_text(entry: &MemoryEntry) -> String {
    if entry.found {
        return entry
            .text
            .clone()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| "The service returned this entry with no text.".to_string());
    }
    "The service no longer has that entry. Nothing was changed.".to_string()
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

fn render(ui: &Rc<Ui>, search: &MemorySearch) {
    while let Some(row) = ui.list.first_child() {
        ui.list.remove(&row);
    }
    *ui.hits.borrow_mut() = search.hits.clone();
    for (index, hit) in search.hits.iter().enumerate() {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.set_widget_name(&format!("memory-hit:{index}"));
        let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
        column.set_hexpand(true);
        let headline = label(&hit_headline(hit));
        headline.add_css_class("search-result-title");
        column.append(&headline);
        if let Some(citation) = citation_text(hit) {
            let citation = label(&citation);
            citation.add_css_class("dim-label");
            column.append(&citation);
        }
        if let Some(meta) = hit_meta(hit) {
            let meta = label(&meta);
            meta.add_css_class("dim-label");
            column.append(&meta);
        }
        button.set_child(Some(&column));
        let row_ui = Rc::clone(ui);
        button.connect_clicked(move |_| open(&row_ui, index));
        ui.list.append(&button);
    }
}

fn open(ui: &Rc<Ui>, index: usize) {
    let Some(hit) = ui.hits.borrow().get(index).cloned() else {
        ui.notice
            .set_text("That result is no longer in the list. Search again.");
        return;
    };
    if ui.busy.get() {
        ui.notice
            .set_text("A memory read is already running. Wait for it to finish.");
        return;
    }
    let Some(config) = (ui.context)() else {
        ui.notice.set_text(
            "Connect your Muse account first: memory belongs to the connected account. Nothing was read.",
        );
        return;
    };
    ui.epoch.set(ui.epoch.get().wrapping_add(1));
    let epoch = ui.epoch.get();
    set_busy(ui, true);
    ui.notice.set_text("Opening the remembered passage\u{2026}");
    let uri = hit.memory_uri.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = AccountClient::new(config).memory_get(&uri);
        let _ = tx.send(result);
    });
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
                    Ok(entry) => {
                        ui.detail.set_text(&entry_text(&entry));
                        ui.notice
                            .set_text("Read the passage the service cited. Nothing was changed.");
                    }
                    Err(error) => {
                        ui.notice.set_text(&format!(
                            "Could not open that memory: {}. Nothing was changed.",
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

fn search(ui: &Rc<Ui>) {
    if ui.busy.get() {
        ui.notice
            .set_text("A memory read is already running. Wait for it to finish.");
        return;
    }
    let query = ui.entry.text().trim().to_string();
    if query.is_empty() {
        ui.notice
            .set_text("Type something to look for in memory first.");
        return;
    }
    let Some(config) = (ui.context)() else {
        ui.notice.set_text(
            "Connect your Muse account first: memory belongs to the connected account. Nothing was read.",
        );
        return;
    };
    ui.epoch.set(ui.epoch.get().wrapping_add(1));
    let epoch = ui.epoch.get();
    set_busy(ui, true);
    ui.notice.set_text("Searching memory\u{2026}");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = AccountClient::new(config).memory_search(&query);
        let _ = tx.send((query, result));
    });
    let weak = Rc::downgrade(ui);
    gtk::glib::timeout_add_local(Duration::from_millis(80), move || {
        let Some(ui) = weak.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        match rx.try_recv() {
            Ok((query, result)) => {
                if ui.epoch.get() != epoch {
                    return gtk::glib::ControlFlow::Break;
                }
                set_busy(&ui, false);
                match result {
                    Ok(found) => {
                        render(&ui, &found);
                        ui.notice.set_text(&matched_text(&found, &query));
                        ui.detail
                            .set_text("Open a result to read the whole passage.");
                    }
                    Err(error) => {
                        ui.notice.set_text(&format!(
                            "Memory search failed: {}. Nothing was changed.",
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

pub fn build(context: AccountContext) -> MemoryPage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("page-content");
    let heading = label("Memory \u{b7} what your Muse remembers");
    heading.add_css_class("page-heading");
    root.append(&heading);
    let intro = label(
        "Asks the service for the passages its agent keeps about you, and shows the line each one came from. This page reads only: the service records no route that forgets or edits a memory entry, so forgetting still happens on the Muse side.",
    );
    intro.add_css_class("page-subtitle");
    root.append(&intro);

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.add_css_class("toolbar");
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("Search what your Muse remembers"));
    entry.set_hexpand(true);
    let go = gtk::Button::with_label("Search memory");
    go.add_css_class("suggested-action");
    let spinner = gtk::Spinner::new();
    toolbar.append(&entry);
    toolbar.append(&go);
    toolbar.append(&spinner);
    root.append(&toolbar);

    let notice = label("Nothing searched yet.");
    root.append(&notice);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_min_content_height(180);
    scroll.set_child(Some(&list));
    root.append(&scroll);

    let detail = label("Open a result to read the whole passage.");
    detail.add_css_class("memory-detail");
    let detail_scroll = gtk::ScrolledWindow::new();
    detail_scroll.set_min_content_height(140);
    detail_scroll.set_child(Some(&detail));
    root.append(&detail_scroll);

    let ui = Rc::new(Ui {
        epoch: Cell::new(0),
        busy: Cell::new(false),
        context,
        entry,
        list,
        notice,
        detail,
        spinner,
        hits: RefCell::new(Vec::new()),
    });
    {
        let ui = Rc::clone(&ui);
        go.connect_clicked(move |_| search(&ui));
    }
    {
        let ui = Rc::clone(&ui);
        let entry = ui.entry.clone();
        entry.connect_activate(move |_| search(&ui));
    }

    let load = {
        let weak = Rc::downgrade(&ui);
        Rc::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.entry.grab_focus();
            }
        }) as Rc<dyn Fn()>
    };
    MemoryPage {
        root,
        load,
        #[cfg(test)]
        ui,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        citation_text, entry_text, hit_headline, hit_meta, matched_text, open, read_failure,
        render, search,
    };
    use gtk::prelude::*;
    use gtk4 as gtk;
    use muse_account::{ErrorCode, MemoryEntry, MemoryHit, MemorySearch};
    use std::rc::Rc;

    fn hit(text: &str, path: Option<&str>, start: Option<u64>, end: Option<u64>) -> MemoryHit {
        MemoryHit {
            memory_uri: "memory://md:memory/2026-10-22.md:950".into(),
            text: text.into(),
            path: path.map(str::to_string),
            line_start: start,
            line_end: end,
            source_kind: Some("daily".into()),
            status: Some("active".into()),
        }
    }

    fn search_result(recognized: u64, skipped: u64, hits: Vec<MemoryHit>) -> MemorySearch {
        MemorySearch {
            recognized,
            skipped,
            hits,
        }
    }

    #[test]
    fn the_count_line_names_the_query_and_counts_unreadable_rows() {
        let none = matched_text(&search_result(0, 0, vec![]), "budget");
        assert!(none.contains("No memories matched"), "{none}");
        assert!(none.contains("budget"), "{none}");
        let one = matched_text(
            &search_result(1, 0, vec![hit("a", None, None, None)]),
            "budget",
        );
        assert!(one.starts_with("1 memory matched"), "{one}");
        let many = matched_text(
            &search_result(3, 2, vec![hit("a", None, None, None)]),
            "budget",
        );
        assert!(many.starts_with("3 memories matched"), "{many}");
        assert!(
            many.contains("2 memories the service sent could not be read"),
            "{many}"
        );
    }

    #[test]
    fn a_hit_shows_its_first_real_line_and_the_line_it_came_from() {
        let passage = hit(
            "\n  **Slice 7** \u{2014} budget surgery\nsecond line",
            Some("memory/2026-10-22.md"),
            Some(950),
            Some(950),
        );
        assert_eq!(
            hit_headline(&passage),
            "**Slice 7** \u{2014} budget surgery"
        );
        assert_eq!(
            citation_text(&passage).as_deref(),
            Some("memory/2026-10-22.md:950")
        );
        let ranged = hit("x", Some("memory/2026-10-02.md"), Some(12), Some(19));
        assert_eq!(
            citation_text(&ranged).as_deref(),
            Some("memory/2026-10-02.md:12\u{2013}19")
        );
        let path_only = hit("x", Some("memory/2026-10-02.md"), None, None);
        assert_eq!(
            citation_text(&path_only).as_deref(),
            Some("memory/2026-10-02.md")
        );
        assert_eq!(
            hit_headline(&hit("   ", None, None, None)),
            "A remembered passage"
        );
        assert_eq!(hit_meta(&passage).as_deref(), Some("daily \u{b7} active"));
    }

    #[test]
    fn a_missing_entry_says_nothing_changed_instead_of_echoing_the_service() {
        let found = MemoryEntry {
            found: true,
            text: Some("the remembered text".into()),
            ..Default::default()
        };
        assert_eq!(entry_text(&found), "the remembered text");
        let gone = MemoryEntry {
            found: false,
            reason: Some("memory entry not found".into()),
            ..Default::default()
        };
        let text = entry_text(&gone);
        assert!(text.contains("no longer has that entry"), "{text}");
        assert!(
            !text.contains("memory entry not found"),
            "service text leaked: {text}"
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
            ErrorCode::VmUnavailable,
            ErrorCode::BridgeFailure,
            ErrorCode::ResponseTooLarge,
        ] {
            let text = read_failure(code);
            assert!(!text.is_empty());
            assert!(!text.contains("SECRET"));
        }
    }

    fn hit_buttons(root: &gtk::Widget) -> Vec<gtk::Button> {
        let mut found = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(widget) = stack.pop() {
            if let Ok(button) = widget.clone().downcast::<gtk::Button>()
                && button.widget_name().starts_with("memory-hit:")
            {
                found.push(button);
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                stack.push(current.clone());
                child = current.next_sibling();
            }
        }
        found
    }

    #[test]
    #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
    fn an_unconnected_search_reads_nothing_and_says_so() {
        gtk::init().unwrap();
        let page = super::build(Rc::new(|| None));
        page.ui.entry.set_text("budget");
        search(&Rc::clone(&page.ui));
        assert!(
            page.ui
                .notice
                .text()
                .contains("Connect your Muse account first"),
            "{}",
            page.ui.notice.text()
        );
        assert!(page.ui.list.first_child().is_none());
    }

    #[test]
    #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
    fn every_hit_renders_as_the_button_that_opens_it() {
        gtk::init().unwrap();
        let page = super::build(Rc::new(|| None));
        let results = search_result(
            2,
            0,
            vec![
                hit("first", Some("memory/a.md"), Some(1), Some(1)),
                hit("second", Some("memory/b.md"), Some(2), Some(3)),
            ],
        );
        render(&Rc::clone(&page.ui), &results);
        let root: &gtk::Widget = page.root.upcast_ref();
        let buttons = hit_buttons(root);
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0].widget_name(), "memory-hit:0");
        assert_eq!(buttons[1].widget_name(), "memory-hit:1");
    }

    #[test]
    #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
    fn opening_an_unconnected_account_reads_nothing() {
        gtk::init().unwrap();
        let page = super::build(Rc::new(|| None));
        render(
            &Rc::clone(&page.ui),
            &search_result(
                1,
                0,
                vec![hit("first", Some("memory/a.md"), Some(1), Some(1))],
            ),
        );
        open(&Rc::clone(&page.ui), 0);
        assert!(
            page.ui
                .notice
                .text()
                .contains("Connect your Muse account first"),
            "{}",
            page.ui.notice.text()
        );
        assert_eq!(
            page.ui.detail.text(),
            "Open a result to read the whole passage."
        );
    }
}
