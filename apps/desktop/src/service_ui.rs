//! Read-only Ideas and Goals pages: two muse.ai surfaces, one shape.
//! (The Feed page outgrew the shared shape — it renders web-style
//! cards with rich text and hero images — and lives in `feed_ui`.)
//!
//! Each page reads its recorded list route once per explicit refresh, on a
//! worker thread, and renders the rows the bridge projected. Nothing here
//! can act on a row: executing an idea card or editing a goal are writes,
//! and they stay on the Muse side. The notice always reports the
//! service's own total and the bridge's skipped count, so a payload shape
//! this build does not recognize yet reads as a client gap, never as an
//! empty surface.

use crate::local_host::AccountContext;
use gtk::prelude::*;
use gtk4 as gtk;
use muse_account::{AccountClient, AccountError, ListingItem, ServiceListing};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

pub struct ServicePage {
    pub root: gtk::Box,
    /// Loads (or re-reads) the listing. Fired by the shell when the
    /// page's tab is opened, so the surface shows its contents on the
    /// click that opens it; the button remains for a manual re-read.
    pub load: Rc<dyn Fn()>,
}

struct Spec {
    heading: &'static str,
    intro: &'static str,
    refresh_label: &'static str,
    empty_copy: &'static str,
    /// Group the rows under their kind as section headings, the
    /// way the web's Ideas page does (its cards arrive in named
    /// sections). Off for Goals, whose kind is a per-row status.
    group_by_kind: bool,
    /// The one action a row offers, when the surface has one the
    /// recorded routes ground (Ideas: "Try it"). A press runs it
    /// once against the row's own id; the page then re-reads, so
    /// what shows afterwards is the service's state, not the
    /// answer's claim.
    action: Option<SpecAction>,
    fetch: fn(&AccountClient) -> Result<ServiceListing, AccountError>,
}

#[derive(Clone, Copy)]
struct SpecAction {
    label: &'static str,
    run: fn(&AccountClient, &str) -> Result<String, AccountError>,
}

/// A row's action as the card builder takes it: the button's
/// label and the press handler, which receives the row's id.
type RowAction<'a> = Option<(&'a str, &'a Rc<dyn Fn(String)>)>;

struct Ui {
    epoch: Cell<u64>,
    busy: Cell<bool>,
    context: AccountContext,
    spec: Spec,
    list: gtk::ListBox,
    notice: gtk::Label,
    spinner: gtk::Spinner,
    /// A message a completed action leaves for the re-read its
    /// success triggers, so the reader sees the outcome instead of
    /// the read's summary overwriting it.
    after_read: std::cell::RefCell<Option<String>>,
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_selectable(true);
    label.set_max_width_chars(110);
    label
}

pub(super) fn summary_text(page: &ServiceListing, empty_copy: &str) -> String {
    if page.items.is_empty() {
        if page.total == 0 {
            return empty_copy.to_string();
        }
        if page.skipped > 0 && page.recognized == 0 {
            return format!(
                "The service sent {} item(s), but none in a shape this build recognizes yet. That is a client gap, not an empty list.",
                page.total
            );
        }
        return format!(
            "The service reported {} item(s), and this page carried no usable rows.",
            page.total
        );
    }
    let mut summary = format!(
        "{} shown · the service reported {}",
        page.items.len(),
        page.total
    );
    if page.skipped > 0 {
        summary.push_str(&format!(
            " · {} arrived in a shape this build doesn't recognize yet",
            page.skipped
        ));
    }
    summary
}

fn row_card(item: &ListingItem, show_kind: bool, action: RowAction<'_>) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("card");
    // The web draws an icon at the card's left edge; when the
    // listing carries one, the text column sits beside it.
    let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let title = label(&item.title);
    text.append(&title);
    if let Some(body) = &item.body
        && body != &item.title
    {
        let body_label = label(body);
        body_label.add_css_class("dim-label");
        text.append(&body_label);
    }
    let meta: Vec<&str> = [
        if show_kind {
            item.kind.as_deref()
        } else {
            None
        },
        item.stamp.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !meta.is_empty() {
        let meta_label = label(&meta.join(" · "));
        meta_label.add_css_class("dim-label");
        text.append(&meta_label);
    }
    if let Some(icon) = &item.icon {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let icon_label = gtk::Label::new(Some(icon));
        icon_label.set_widget_name("item-icon");
        icon_label.set_valign(gtk::Align::Start);
        icon_label.add_css_class("feed-icon");
        row.append(&icon_label);
        text.set_hexpand(true);
        row.append(&text);
        card.append(&row);
    } else {
        card.append(&text);
    }
    if let (Some((label, run)), Some(id)) = (action, &item.id) {
        let button = gtk::Button::with_label(label);
        button.set_widget_name(&format!("item-action:{id}"));
        button.set_halign(gtk::Align::Start);
        let run = Rc::clone(run);
        let id = id.clone();
        button.connect_clicked(move |_| run(id.clone()));
        card.append(&button);
    }
    card
}

fn section_heading(kind: &str) -> gtk::Label {
    let heading = label(kind);
    heading.set_widget_name(&format!("section-heading:{kind}"));
    heading.add_css_class("section-heading");
    heading
}

impl Ui {
    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.spinner.set_spinning(busy);
        self.spinner.set_visible(busy);
    }

    fn render(&self, items: &[ListingItem], action: Option<Rc<dyn Fn(String)>>) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        let grouped = self.spec.group_by_kind;
        let action = action.map(|run| (self.spec.action.map(|a| a.label).unwrap_or(""), run));
        let mut last_kind: Option<&str> = None;
        for item in items {
            if grouped
                && let Some(kind) = item.kind.as_deref()
                && last_kind != Some(kind)
            {
                self.list.append(&section_heading(kind));
                last_kind = Some(kind);
            }
            self.list.append(&row_card(
                item,
                !grouped,
                action.as_ref().map(|(label, run)| (*label, run)),
            ));
        }
    }

    /// Run the page's row action once for one row id, on a worker
    /// thread; the notice reports the outcome and the page re-reads
    /// so the rows show the service's state afterwards.
    fn run_action(&self, ui: &Rc<Ui>, id: String) {
        let Some(action) = self.spec.action else {
            return;
        };
        if self.busy.get() {
            self.notice
                .set_text("A read is already running. Wait for it to finish.");
            return;
        }
        let Some(config) = (self.context)() else {
            self.notice.set_text(
                "Connect your Muse account first: this surface belongs to the connected account. Nothing was sent.",
            );
            return;
        };
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let epoch = self.epoch.get();
        self.set_busy(true);
        self.notice.set_text("Working…");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (action.run)(&AccountClient::new(config), &id);
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
                    ui.set_busy(false);
                    match result {
                        Ok(message) => {
                            *ui.after_read.borrow_mut() = Some(message);
                            ui.refresh(&ui);
                        }
                        Err(error) => {
                            ui.notice.set_text(&format!(
                                "That didn't go through: {}. Nothing else was changed.",
                                crate::library_ui::read_failure(error.code)
                            ));
                        }
                    }
                    gtk::glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if ui.epoch.get() == epoch {
                        ui.set_busy(false);
                        ui.notice.set_text(
                            "The account worker vanished before answering. Nothing was sent.",
                        );
                    }
                    gtk::glib::ControlFlow::Break
                }
            }
        });
    }

    fn refresh(&self, ui: &Rc<Ui>) {
        if self.busy.get() {
            self.notice
                .set_text("A read is already running. Wait for it to finish.");
            return;
        }
        let Some(config) = (self.context)() else {
            self.notice.set_text(
                "Connect your Muse account first: this surface belongs to the connected account. Nothing was read.",
            );
            return;
        };
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let epoch = self.epoch.get();
        self.set_busy(true);
        self.notice.set_text("Reading…");
        let fetch = self.spec.fetch;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = fetch(&AccountClient::new(config));
            let _ = tx.send(result);
        });
        self.poll(ui, rx, epoch);
    }

    fn poll(
        &self,
        ui: &Rc<Ui>,
        rx: mpsc::Receiver<Result<ServiceListing, AccountError>>,
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
                    ui.finish(&ui, result);
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

    fn finish(&self, ui: &Rc<Ui>, result: Result<ServiceListing, AccountError>) {
        match result {
            Ok(page) => {
                let action: Option<Rc<dyn Fn(String)>> = self.spec.action.map(|_| {
                    let weak = Rc::downgrade(ui);
                    Rc::new(move |id: String| {
                        if let Some(ui) = weak.upgrade() {
                            ui.run_action(&ui, id);
                        }
                    }) as Rc<dyn Fn(String)>
                });
                self.render(&page.items, action);
                if let Some(message) = self.after_read.borrow_mut().take() {
                    self.notice.set_text(&message);
                } else {
                    self.notice
                        .set_text(&summary_text(&page, self.spec.empty_copy));
                }
            }
            Err(error) => {
                self.notice.set_text(&format!(
                    "Read failed: {}. Nothing was changed.",
                    crate::library_ui::read_failure(error.code)
                ));
            }
        }
    }
}

fn build(context: AccountContext, spec: Spec) -> ServicePage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("page-content");
    let heading = label(spec.heading);
    heading.add_css_class("page-heading");
    root.append(&heading);
    let intro = label(spec.intro);
    intro.add_css_class("page-subtitle");
    root.append(&intro);

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.add_css_class("toolbar");
    let refresh = gtk::Button::with_label(spec.refresh_label);
    refresh.add_css_class("suggested-action");
    refresh.add_css_class("primary-action");
    let spinner = gtk::Spinner::new();
    spinner.set_visible(false);
    toolbar.append(&refresh);
    toolbar.append(&spinner);
    root.append(&toolbar);

    let notice = label("Nothing read yet.");
    root.append(&notice);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_min_content_height(220);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&list));
    root.append(&scroll);

    let ui = Rc::new(Ui {
        epoch: Cell::new(0),
        busy: Cell::new(false),
        context,
        spec,
        list,
        notice,
        spinner,
        after_read: std::cell::RefCell::new(None),
    });
    {
        let ui = Rc::clone(&ui);
        refresh.connect_clicked(move |_| ui.refresh(&ui));
    }

    let load = {
        let weak = Rc::downgrade(&ui);
        Rc::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.refresh(&ui);
            }
        }) as Rc<dyn Fn()>
    };
    ServicePage { root, load }
}

/// The Ideas page's one action: execute the card ("Try it" on
/// the web). The bridge sends the recorded consumer's body; this
/// reports what the reader can expect to see, not an end state.
fn idea_execute_action(client: &AccountClient, id: &str) -> Result<String, AccountError> {
    let answer = client.idea_execute(id)?;
    Ok(match answer.session_id {
        Some(session) => format!(
            "Started — the work runs on the Muse side now. Look for it in Chat (session {session}). The list above has been re-read."
        ),
        None => "Started — the work runs on the Muse side now. Look for it in Chat. The list above has been re-read.".to_string(),
    })
}

pub fn ideas(context: AccountContext) -> ServicePage {
    build(
        context,
        Spec {
            heading: "Ideas",
            intro: "The idea cards your Muse saved for the connected account. Try it runs a card on the Muse side; rating a card still happens there. The list loads when you open this page.",
            refresh_label: "Refresh Ideas",
            empty_copy: "No idea cards yet — the service reported an empty list.",
            group_by_kind: true,
            action: Some(SpecAction {
                label: "Try it",
                run: idea_execute_action,
            }),
            fetch: AccountClient::ideas,
        },
    )
}

pub fn goals(context: AccountContext) -> ServicePage {
    build(
        context,
        Spec {
            heading: "Goals",
            intro: "The goals attached to the connected Muse account, read-only. Creating, editing or completing a goal happens on the Muse side; this page only reads. The list loads when you open this page.",
            refresh_label: "Refresh Goals",
            empty_copy: "No goals yet — the service reported an empty list.",
            group_by_kind: false,
            action: None,
            fetch: AccountClient::goals,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{Ui, row_card, section_heading, summary_text};
    use gtk::prelude::*;
    use gtk4 as gtk;
    use muse_account::{ListingItem, ServiceListing};
    use std::cell::Cell;
    use std::rc::Rc;

    fn item(title: &str) -> ListingItem {
        ListingItem {
            id: None,
            title: title.into(),
            body: None,
            kind: None,
            stamp: None,
            kicker: None,
            category: None,
            icon: None,
            image_url: None,
            widget_text: None,
            widget_html: None,
        }
    }

    #[test]
    fn an_empty_service_list_reads_as_empty() {
        let page = ServiceListing::default();
        assert_eq!(summary_text(&page, "EMPTY"), "EMPTY");
    }

    #[test]
    fn an_unrecognized_shape_never_reads_as_empty() {
        let page = ServiceListing {
            total: 5,
            recognized: 0,
            skipped: 5,
            items: vec![],
            ..ServiceListing::default()
        };
        let text = summary_text(&page, "EMPTY");
        assert!(text.contains("client gap"), "{text}");
        assert!(text.contains('5'), "{text}");
    }

    #[test]
    #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
    fn ideas_render_groups_cards_under_section_headings() {
        // The web's Ideas page (Patrick, Nov 13, 2026): cards sit
        // under their section's heading with an icon at the left
        // edge, not in one flat list repeating the section name on
        // every card.
        gtk::init().unwrap();
        fn named(root: &gtk::Widget, name: &str) -> bool {
            if root.widget_name() == name {
                return true;
            }
            let mut child = root.first_child();
            while let Some(current) = child {
                let next = current.next_sibling();
                if named(&current, name) {
                    return true;
                }
                child = next;
            }
            false
        }
        let ui = Rc::new(Ui {
            epoch: Cell::new(0),
            busy: Cell::new(false),
            context: Rc::new(|| None),
            spec: crate::service_ui::Spec {
                heading: "Ideas",
                intro: "",
                refresh_label: "",
                empty_copy: "",
                group_by_kind: true,
                action: Some(super::SpecAction {
                    label: "Try it",
                    run: |_, _| Ok(String::new()),
                }),
                fetch: muse_account::AccountClient::ideas,
            },
            list: gtk::ListBox::new(),
            notice: gtk::Label::new(None),
            spinner: gtk::Spinner::new(),
            after_read: std::cell::RefCell::new(None),
        });
        let mut first = item("Score the offer");
        first.kind = Some("This week's big calls".into());
        first.icon = Some("💡".into());
        let mut second = item("Flip the calendars");
        second.kind = Some("This week's big calls".into());
        let mut third = item("Recover the email");
        third.kind = Some("Productivity".into());
        ui.render(&[first, second, third], None);
        let root = ui.list.clone().upcast::<gtk::Widget>();
        assert!(named(&root, "section-heading:This week's big calls"));
        assert!(named(&root, "section-heading:Productivity"));
        assert!(named(&root, "item-icon"), "the card icon renders");
        // Two sections, three cards: exactly five rows, headings
        // counted once each.
        let mut rows = 0;
        let mut child = ui.list.first_child();
        while let Some(current) = child {
            rows += 1;
            child = current.next_sibling();
        }
        assert_eq!(rows, 5);
        // The heading building block names itself for the walk.
        assert_eq!(section_heading("S").widget_name(), "section-heading:S");
        // A card whose row carries an id gets the page's action
        // button, named for that id; a press hands the id over.
        let pressed: Rc<std::cell::RefCell<Vec<String>>> =
            Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = Rc::clone(&pressed);
        let run: Rc<dyn Fn(String)> = Rc::new(move |id| sink.borrow_mut().push(id));
        let mut with_id = item("Executable");
        with_id.id = Some("c1".into());
        let card = row_card(&with_id, true, Some(("Try it", &run)));
        fn find_button(root: &gtk::Widget, name: &str) -> Option<gtk::Button> {
            if root.widget_name() == name {
                return root.downcast_ref::<gtk::Button>().cloned();
            }
            let mut child = root.first_child();
            while let Some(current) = child {
                let next = current.next_sibling();
                if let Some(button) = find_button(&current, name) {
                    return Some(button);
                }
                child = next;
            }
            None
        }
        let button = find_button(card.upcast_ref(), "item-action:c1")
            .expect("the card carries its action button");
        button.emit_clicked();
        assert_eq!(pressed.borrow().as_slice(), ["c1"]);
        // A card without an id offers no button: there is nothing
        // honest to act on.
        let bare = row_card(&item("x"), true, Some(("Try it", &run)));
        assert!(find_button(bare.upcast_ref(), "item-action:c1").is_none());
    }

    #[test]
    fn skips_are_reported_alongside_shown_rows() {
        let page = ServiceListing {
            total: 4,
            recognized: 3,
            skipped: 1,
            items: vec![item("a"), item("b"), item("c")],
            ..ServiceListing::default()
        };
        let text = summary_text(&page, "EMPTY");
        assert!(text.contains("3 shown"), "{text}");
        assert!(text.contains("reported 4"), "{text}");
        assert!(text.contains("1 arrived"), "{text}");
    }
}
