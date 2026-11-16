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

/// One rendered row, ready to append to the transcript list. Held link
/// cards ride along so an assistant row paints its previews with the
/// row; urls with no held card stay text-only until a fetch lands.
pub fn row_widget(
    row: &TranscriptRow,
    on_option: &std::rc::Rc<dyn Fn(String)>,
    previews: &std::collections::HashMap<
        String,
        (muse_account::LinkPreview, Option<std::path::PathBuf>),
    >,
) -> gtk4::Widget {
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
            attachments,
            selected_option,
            ..
        } => message_widget(
            role,
            text,
            options,
            selected_option.as_deref(),
            attachments,
            on_option,
            previews,
        ),
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

fn message_widget(
    role: &Role,
    text: &str,
    options: &[String],
    selected: Option<&str>,
    attachments: &[MessageAttachment],
    on_option: &std::rc::Rc<dyn Fn(String)>,
    previews: &std::collections::HashMap<
        String,
        (muse_account::LinkPreview, Option<std::path::PathBuf>),
    >,
) -> gtk4::Widget {
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
    // The Mac panel: the reader's words sit in a grey bubble, the
    // assistant's in plain text. The class carries the Mac surface.
    if matches!(role, Role::User) {
        bubble.add_css_class("muse-user-bubble");
    } else {
        bubble.add_css_class("muse-assistant-text");
    }
    column.append(&bubble);

    if !options.is_empty() {
        let choices = gtk4::FlowBox::new();
        choices.set_selection_mode(gtk4::SelectionMode::None);
        choices.set_max_children_per_line(4);
        for option in options {
            // The web marks the reader's pick with a check; the class
            // draws the blue edge. Both ride the service's own
            // selected_option, never a local guess.
            let picked = selected == Some(option.as_str());
            let label = if picked {
                format!("{option} ✓")
            } else {
                option.clone()
            };
            let button = gtk4::Button::with_label(&label);
            button.add_css_class("muse-option");
            if picked {
                button.add_css_class("muse-option-selected");
            }
            let callback = on_option.clone();
            let text = option.clone();
            button.connect_clicked(move |_| callback(text.clone()));
            choices.append(&button);
        }
        column.append(&choices);
    }
    for attachment in attachments {
        column.append(&attachment_card(attachment));
    }
    // The Mac panel cards linked pages under the assistant's words.
    // Only held cards paint: a url with no fetched card stays a plain
    // link in the text until its fetch lands and repaints the row.
    if !matches!(role, Role::User) {
        for link in super::chat::extract_links(text, 2) {
            if let Some((preview, thumb)) = previews.get(&link) {
                column.append(&link_card(preview, &link, thumb.as_deref()));
            }
        }
    }
    column.upcast()
}

/// One attachment card: name plus the largest useful display fields.
/// Bytes and hashes never cross into the client, so there is nothing
/// here that could leak a local path.
pub fn attachment_card(attachment: &MessageAttachment) -> gtk4::Widget {
    let mut lines = vec![attachment.filename.clone()];
    if let (Some(width), Some(height)) = (attachment.width, attachment.height) {
        lines.push(format!("{width} × {height}"));
    }
    if let Some(size) = attachment.bytes_len {
        lines.push(format!("{} bytes", size));
    }
    if let Some(caption) = attachment.caption.as_deref()
        && !caption.is_empty()
    {
        lines.push(caption.to_owned());
    }
    if let Some(transcription) = attachment.transcription.as_deref()
        && !transcription.is_empty()
    {
        lines.push(transcription.to_owned());
    }
    let card = gtk4::Label::new(Some(&lines.join("\n")));
    card.set_xalign(0.0);
    card.add_css_class("card");
    card.upcast()
}

/// One link-preview card: the thumbnail when its artwork fetched,
/// then the title, the description, and the url. A thin answer is a
/// thin card: whatever fields are missing simply do not paint.
pub fn link_card(
    preview: &muse_account::LinkPreview,
    link: &str,
    thumb: Option<&std::path::Path>,
) -> gtk4::Widget {
    let card = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    card.add_css_class("muse-link-card");
    if let Some(path) = thumb {
        let picture = gtk4::Picture::for_filename(path);
        picture.set_size_request(96, 96);
        picture.set_can_shrink(true);
        card.append(&picture);
    }
    let facts = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    facts.set_hexpand(true);
    let title = preview.title.as_deref().unwrap_or(link);
    let headline = gtk4::Label::new(Some(title));
    headline.set_xalign(0.0);
    headline.set_wrap(true);
    headline.add_css_class("muse-link-title");
    facts.append(&headline);
    if let Some(description) = preview.description.as_deref()
        && !description.is_empty()
    {
        let blurb = gtk4::Label::new(Some(description));
        blurb.set_xalign(0.0);
        blurb.set_wrap(true);
        blurb.set_lines(3);
        blurb.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        facts.append(&blurb);
    }
    let source = gtk4::Label::new(Some(preview.site_name.as_deref().unwrap_or(link)));
    source.set_xalign(0.0);
    source.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    source.add_css_class("dim-label");
    facts.append(&source);
    card.append(&facts);
    card.upcast()
}

/// Replace the transcript list contents with freshly built rows.
pub fn populate(
    list: &gtk4::ListBox,
    rows: &[TranscriptRow],
    on_option: &std::rc::Rc<dyn Fn(String)>,
    previews: &std::collections::HashMap<
        String,
        (muse_account::LinkPreview, Option<std::path::PathBuf>),
    >,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for row in rows {
        list.append(&row_widget(row, on_option, previews));
    }
}

#[cfg(test)]
mod tests {
    use super::super::chat::build_rows;
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

    /// Needs a display: run under `xvfb-run --test-threads=1`. The
    /// suite's only `gtk::init`: GTK binds init to the calling thread,
    /// so every widget assertion lives in this one test.
    #[test]
    #[ignore]
    fn transcript_renders_one_widget_per_row() {
        gtk4::init().expect("gtk init");
        let rows = build_rows(&history());
        let list = gtk4::ListBox::new();
        let on_option: std::rc::Rc<dyn Fn(String)> = std::rc::Rc::new(|_| {});
        let previews = std::collections::HashMap::new();
        populate(&list, &rows, &on_option, &previews);
        let mut count = 0;
        let mut child = list.first_child();
        while child.is_some() {
            count += 1;
            child = child.and_then(|visible| visible.next_sibling());
        }
        assert_eq!(count, rows.len());

        let built = super::super::composer::ComposerRow::build();
        let mut composer = super::super::composer::Composer::new();
        composer.set_text("hi");
        built.refresh(&composer);
        assert!(built.send.is_sensitive());
        assert!(built.send.is_visible());
        assert!(!built.stop.is_sensitive());
        assert!(!built.stop.is_visible());
        composer.begin_send();
        built.refresh(&composer);
        assert!(!built.send.is_sensitive());
        assert!(!built.send.is_visible());
        assert!(built.stop.is_sensitive());
        assert!(built.stop.is_visible());

        // A held card paints beside the row; a thin answer paints
        // only what it carries.
        let preview = muse_account::LinkPreview {
            title: Some("Ovenbird Cookware".to_owned()),
            description: Some("Cast iron goods".to_owned()),
            image_url: None,
            url: Some("https://example.com/pot".to_owned()),
            site_name: Some("example.com".to_owned()),
            keys: vec!["title".to_owned()],
        };
        let card = link_card(&preview, "https://example.com/pot", None);
        let facts = card
            .downcast::<gtk4::Box>()
            .expect("card is a box")
            .last_child()
            .expect("card has facts");
        assert!(facts.first_child().is_some());
    }
}
