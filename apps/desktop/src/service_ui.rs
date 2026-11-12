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
    fetch: fn(&AccountClient) -> Result<ServiceListing, AccountError>,
}

struct Ui {
    epoch: Cell<u64>,
    busy: Cell<bool>,
    context: AccountContext,
    spec: Spec,
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

fn row_card(item: &ListingItem) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("card");
    let title = label(&item.title);
    card.append(&title);
    if let Some(body) = &item.body
        && body != &item.title
    {
        let body_label = label(body);
        body_label.add_css_class("dim-label");
        card.append(&body_label);
    }
    let meta: Vec<&str> = [item.kind.as_deref(), item.stamp.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    if !meta.is_empty() {
        let meta_label = label(&meta.join(" · "));
        meta_label.add_css_class("dim-label");
        card.append(&meta_label);
    }
    card
}

impl Ui {
    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.spinner.set_spinning(busy);
        self.spinner.set_visible(busy);
    }

    fn render(&self, items: &[ListingItem]) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        for item in items {
            self.list.append(&row_card(item));
        }
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

    fn finish(&self, result: Result<ServiceListing, AccountError>) {
        match result {
            Ok(page) => {
                self.render(&page.items);
                self.notice
                    .set_text(&summary_text(&page, self.spec.empty_copy));
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

pub fn ideas(context: AccountContext) -> ServicePage {
    build(
        context,
        Spec {
            heading: "Ideas",
            intro: "The idea cards your Muse saved for the connected account, read-only. Trying a card or rating it happens on the Muse side; this page only reads. The list loads when you open this page.",
            refresh_label: "Refresh Ideas",
            empty_copy: "No idea cards yet — the service reported an empty list.",
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
            fetch: AccountClient::goals,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::summary_text;
    use muse_account::{ListingItem, ServiceListing};

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
        };
        let text = summary_text(&page, "EMPTY");
        assert!(text.contains("client gap"), "{text}");
        assert!(text.contains('5'), "{text}");
    }

    #[test]
    fn skips_are_reported_alongside_shown_rows() {
        let page = ServiceListing {
            total: 4,
            recognized: 3,
            skipped: 1,
            items: vec![item("a"), item("b"), item("c")],
        };
        let text = summary_text(&page, "EMPTY");
        assert!(text.contains("3 shown"), "{text}");
        assert!(text.contains("reported 4"), "{text}");
        assert!(text.contains("1 arrived"), "{text}");
    }
}
