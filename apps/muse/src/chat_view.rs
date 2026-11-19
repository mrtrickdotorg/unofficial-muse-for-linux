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
    on_react: &std::rc::Rc<dyn Fn(String, String)>,
    on_reply: &std::rc::Rc<dyn Fn(String, String)>,
    previews: &std::collections::HashMap<
        String,
        (muse_account::LinkPreview, Option<std::path::PathBuf>),
    >,
    show_head: bool,
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
            selected_option,
            attachments,
            sent_at,
            id,
            ..
        } => {
            let parts = MessageParts {
                role,
                text,
                options,
                selected: selected_option.as_deref(),
                sent_at: sent_at.as_deref(),
                show_head,
                attachments,
                id,
            };
            message_widget(&parts, on_option, on_react, on_reply, previews)
        }
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

/// What one message row needs to paint: the row's own fields plus
/// whether it opens an assistant turn. A struct, not nine
/// parameters.
struct MessageParts<'a> {
    role: &'a Role,
    text: &'a str,
    options: &'a [String],
    selected: Option<&'a str>,
    sent_at: Option<&'a str>,
    show_head: bool,
    attachments: &'a [MessageAttachment],
    id: &'a str,
}

fn message_widget(
    parts: &MessageParts,
    on_option: &std::rc::Rc<dyn Fn(String)>,
    on_react: &std::rc::Rc<dyn Fn(String, String)>,
    on_reply: &std::rc::Rc<dyn Fn(String, String)>,
    previews: &std::collections::HashMap<
        String,
        (muse_account::LinkPreview, Option<std::path::PathBuf>),
    >,
) -> gtk4::Widget {
    let MessageParts {
        role,
        text,
        options,
        selected,
        sent_at,
        show_head,
        attachments,
        id,
    } = parts;
    let column = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    match role {
        Role::User => column.set_halign(gtk4::Align::End),
        _ => column.set_halign(gtk4::Align::Start),
    }
    column.set_margin_start(12);
    column.set_margin_end(12);
    // The web names the assistant above each answer: avatar, name,
    // time. One header opens an assistant turn; follow-up rows in the
    // same turn carry none.
    if matches!(role, Role::Assistant) && *show_head {
        let head = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let avatar = gtk4::Label::new(Some("✦"));
        avatar.add_css_class("muse-avatar-small");
        let name = gtk4::Label::new(Some("Muse"));
        name.add_css_class("dim-label");
        head.append(&avatar);
        head.append(&name);
        if let Some(time) = super::chat::format_time(*sent_at) {
            let when = gtk4::Label::new(Some(&time));
            when.add_css_class("dim-label");
            head.append(&when);
        }
        column.append(&head);
    }

    let bubble = gtk4::Label::new(None);
    bubble.set_markup(&super::chat::render_markup(text));
    bubble.set_wrap(true);
    bubble.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    bubble.set_selectable(true);
    bubble.set_xalign(0.0);
    // The web's split: the reader's words in a blue pill, the
    // assistant's in a dark grey bubble. The classes carry the web
    // surfaces; the text itself renders markdown to Pango above.
    if matches!(role, Role::User) {
        bubble.add_css_class("muse-user-bubble");
    } else {
        bubble.add_css_class("muse-assistant-text");
    }
    column.append(&bubble);

    // The web hangs actions under the answer: a Copy button that
    // writes the response text to the clipboard. Local-only, never
    // a write. Reply stages the row as a quote above the composer;
    // the send itself stays a plain send (the community gateway
    // records no reply param), so the quote is context, not protocol.
    let is_assistant = matches!(role, Role::Assistant);
    if is_assistant || matches!(role, Role::User) {
        let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        actions.set_halign(gtk4::Align::Start);
        if is_assistant {
            let copy = gtk4::Button::with_label("Copy");
            copy.add_css_class("flat");
            copy.set_tooltip_text(Some("Copy response"));
            let text = text.to_string();
            copy.connect_clicked(move |button| {
                button.clipboard().set_text(&text);
            });
            actions.append(&copy);
            // The web hangs a React action beside Copy: one press
            // toggles a heart on the message through the live-probed
            // reactions.toggle shape. The answer names the new state;
            // the row itself carries no reaction display to go stale.
            let react = gtk4::Button::with_label("❤");
            react.add_css_class("flat");
            react.set_tooltip_text(Some("React to this message"));
            {
                let react_on = on_react.clone();
                let id: String = (*id).into();
                react.connect_clicked(move |_| {
                    react_on(id.clone(), "❤".to_owned());
                });
            }
            actions.append(&react);
        }
        let reply = gtk4::Button::with_label("Reply");
        reply.add_css_class("flat");
        reply.set_tooltip_text(Some("Reply to this message"));
        {
            let reply_on = on_reply.clone();
            let id: String = (*id).into();
            let text: String = (*text).into();
            reply.connect_clicked(move |_| {
                reply_on(id.clone(), text.clone());
            });
        }
        actions.append(&reply);
        column.append(&actions);
    }

    if !options.is_empty() {
        // The web stacks options full-width, one row each; the
        // reader's pick carries its check at the row's end.
        let choices = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        for option in options.iter() {
            // The web marks the reader's pick with a check; the class
            // draws the blue edge. Both ride the service's own
            // selected_option, never a local guess.
            let picked = *selected == Some(option.as_str());
            let button = gtk4::Button::new();
            button.add_css_class("muse-option");
            if picked {
                button.add_css_class("muse-option-selected");
                let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                let label = gtk4::Label::new(Some(option));
                label.set_hexpand(true);
                label.set_xalign(0.0);
                let check = gtk4::Label::new(Some("✓"));
                row.append(&label);
                row.append(&check);
                button.set_child(Some(&row));
            } else {
                button.set_label(option);
            }
            let callback = on_option.clone();
            let text = option.clone();
            button.connect_clicked(move |_| callback(text.clone()));
            choices.append(&button);
        }
        column.append(&choices);
    }
    for attachment in attachments.iter() {
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
    on_react: &std::rc::Rc<dyn Fn(String, String)>,
    on_reply: &std::rc::Rc<dyn Fn(String, String)>,
    previews: &std::collections::HashMap<
        String,
        (muse_account::LinkPreview, Option<std::path::PathBuf>),
    >,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    // One assistant header opens each assistant turn: a row shows it
    // when the row before it was not an assistant row.
    let mut prev_assistant = false;
    for row in rows {
        let assistant = matches!(
            row,
            TranscriptRow::Message {
                role: Role::Assistant,
                ..
            }
        );
        list.append(&row_widget(
            row,
            on_option,
            on_react,
            on_reply,
            previews,
            assistant && !prev_assistant,
        ));
        prev_assistant = assistant;
    }
}

#[cfg(test)]
mod tests {
    use super::super::chat::build_rows;
    use super::*;

    fn history() -> Vec<muse_account::HistoryMessage> {
        vec![
            muse_account::HistoryMessage {
                message_id: "a".to_owned(),
                seq: 0,
                role: "user".to_owned(),
                text: "hi".to_owned(),
                timestamp: Some("2026-11-10T12:00:00Z".to_owned()),
                status: None,
                options: Vec::new(),
                selected_option: None,
                attachments: Vec::new(),
            },
            muse_account::HistoryMessage {
                message_id: "b".to_owned(),
                seq: 1,
                role: "assistant".to_owned(),
                text: "hello back".to_owned(),
                timestamp: Some("2026-11-10T12:01:00Z".to_owned()),
                status: None,
                options: Vec::new(),
                selected_option: None,
                attachments: Vec::new(),
            },
        ]
    }

    /// Every button labeled `label` under a widget, however nested.
    fn find_buttons(root: &gtk4::Widget, label: &str) -> Vec<gtk4::Button> {
        let mut found = Vec::new();
        let mut child = root.first_child();
        while let Some(next) = child {
            if let Ok(button) = next.clone().downcast::<gtk4::Button>()
                && button.label().as_deref() == Some(label)
            {
                found.push(button);
            }
            found.extend(find_buttons(&next, label));
            child = next.next_sibling();
        }
        found
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
        let on_react: std::rc::Rc<dyn Fn(String, String)> = std::rc::Rc::new(|_, _| {});
        let on_reply: std::rc::Rc<dyn Fn(String, String)> = std::rc::Rc::new(|_, _| {});
        populate(&list, &rows, &on_option, &on_react, &on_reply, &previews);
        let mut count = 0;
        let mut child = list.first_child();
        while child.is_some() {
            count += 1;
            child = child.and_then(|visible| visible.next_sibling());
        }
        assert_eq!(count, rows.len());

        // The assistant row carries one Copy button; the user row
        // carries none. Both rows carry Reply (quote staging only).
        let copies = find_buttons(list.upcast_ref(), "Copy");
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].tooltip_text().as_deref(), Some("Copy response"));
        let replies = find_buttons(list.upcast_ref(), "Reply");
        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0].tooltip_text().as_deref(),
            Some("Reply to this message")
        );

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

        // Every page builds and refills without an account, the way a
        // sidebar click reaches it: structure first, live reads only
        // with a chat attached. A wrong downcast or a missed nested
        // lookup panics here instead of in the reader's session.
        let pages = super::super::pages::Ctx {
            chat: std::rc::Weak::new(),
            stack: gtk4::Stack::new(),
            sidebar: gtk4::ListBox::new(),
            history: gtk4::ListBox::new(),
            profile_name: gtk4::Label::new(None),
            rail: Vec::new(),
            show_signin: std::rc::Rc::new(|| {}) as std::rc::Rc<dyn Fn()>,
            updates: std::rc::Rc::new(std::cell::RefCell::new(
                super::super::update::UpdateCheck::default(),
            )),
        };
        for name in super::super::pages::PAGES {
            super::super::pages::show(&pages, name);
            assert!(pages.stack.child_by_name(name).is_some());
        }
        // Every bundled rail glyph loads: a missing resource paints a
        // blank button, so the suite fails instead of the reader
        // staring at an empty rail.
        super::super::register_icons();
        for glyph in [
            "menu", "chat", "search", "library", "ideas", "tasks", "shapes", "monitor", "new",
            "download",
        ] {
            let image =
                gtk4::Image::from_resource(&format!("/org/kythos/MuseLinux/icons/{glyph}.svg"));
            image.set_pixel_size(20);
            assert!(image.paintable().is_some(), "rail glyph loads: {glyph}");
        }
        // A widget frame builds without taking the process down: the
        // feed mounts these for live graphics, so construction itself
        // has to be probe-safe on a bare display.
        super::super::app::verify_chat_scroll_navigation();
        let _frame = super::super::widget::widget_frame("<b>hi</b>", Some("fallback"));
    }
}
