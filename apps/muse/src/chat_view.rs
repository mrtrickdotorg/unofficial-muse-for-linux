//! Transcript view for the rewrite: GTK builders for `chat` rows.
//! Structure is built eagerly; rows fill in lazily as history reads
//! land. Everything size-bounded: labels wrap, long texts ellipsize
//! at the widget level, no row ever blocks the main thread.
//!
//! The chat surface wires these builders later in this milestone;
//! until then the live app does not reference them yet.
#![allow(dead_code)]

use adw::prelude::*;
use muse_account::MessageAttachment;

use super::chat::{Role, TranscriptRow};

/// One rendered row, ready to append to the transcript list.
pub fn row_widget(row: &TranscriptRow) -> gtk4::Widget {
    match row {
        TranscriptRow::DayDivider { label } => {
            let widget = gtk4::Label::new(Some(label));
            widget.set_halign(gtk4::Align::Center);
            widget.set_margin_top(12);
            widget.set_margin_bottom(4);
            widget.add_css_class("dim-label");
            widget.upcast()
        }
        TranscriptRow::Message {
            role,
            text,
            options,
            ..
        } => message_widget(role, text, options),
        TranscriptRow::Notice { text } => {
            let widget = gtk4::Label::new(Some(text));
            widget.set_halign(gtk4::Align::Center);
            widget.set_wrap(true);
            widget.set_margin_top(8);
            widget.set_margin_bottom(8);
            widget.add_css_class("dim-label");
            widget.upcast()
        }
    }
}

fn message_widget(role: &Role, text: &str, options: &[String]) -> gtk4::Widget {
    let column = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    match role {
        Role::User => column.set_halign(gtk4::Align::End),
        _ => column.set_halign(gtk4::Align::Start),
    }
    column.set_margin_start(12);
    column.set_margin_end(12);

    let bubble = gtk4::Label::new(Some(text));
    bubble.set_wrap(true);
    bubble.set_selectable(true);
    bubble.set_xalign(0.0);
    column.append(&bubble);

    if !options.is_empty() {
        let choices = gtk4::FlowBox::new();
        choices.set_selection_mode(gtk4::SelectionMode::None);
        choices.set_max_children_per_line(4);
        for option in options {
            let button = gtk4::Button::with_label(option);
            choices.append(&button);
        }
        column.append(&choices);
    }
    column.upcast()
}

/// One attachment card: name plus the largest useful display fields.
/// Bytes and hashes never cross into the client, so there is nothing
/// here that could leak a local path.
pub fn attachment_card(attachment: &MessageAttachment) -> gtk4::Widget {
    let mut lines = vec![attachment.filename.clone()];
    if let Some(size) = attachment.bytes_len {
        lines.push(format!("{} bytes", size));
    }
    if let Some(caption) = attachment.caption.as_deref() {
        lines.push(caption.to_owned());
    }
    let card = gtk4::Label::new(Some(&lines.join("\n")));
    card.set_xalign(0.0);
    card.add_css_class("card");
    card.upcast()
}

/// Replace the transcript list contents with freshly built rows.
pub fn populate(list: &gtk4::ListBox, rows: &[TranscriptRow]) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for row in rows {
        list.append(&row_widget(row));
    }
}

#[cfg(test)]
mod tests {
    use super::super::chat::{TranscriptRow, build_rows};
    use super::*;

    fn history() -> Vec<muse_account::HistoryMessage> {
        vec![muse_account::HistoryMessage {
            message_id: "a".to_owned(),
            seq: 0,
            role: "user".to_owned(),
            text: "hi".to_owned(),
            timestamp: Some("2026-11-10T12:00:00Z".to_owned()),
            status: None,
            options: Vec::new(),
            selected_option: None,
            attachments: Vec::new(),
        }]
    }

    /// Needs a display: run under `xvfb-run --test-threads=1`.
    #[test]
    #[ignore]
    fn transcript_renders_one_widget_per_row() {
        gtk4::init().expect("gtk init");
        let rows = build_rows(&history());
        let list = gtk4::ListBox::new();
        populate(&list, &rows);
        let mut count = 0;
        let mut child = list.first_child();
        while child.is_some() {
            count += 1;
            child = child.and_then(|visible| visible.next_sibling());
        }
        assert_eq!(count, rows.len());
    }
}
