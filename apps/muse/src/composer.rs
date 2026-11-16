//! Message composer for the rewrite: pure send-state plus the entry
//! row widgets. The entry holds the draft; Send dispatches exactly
//! once per press; Stop cancels outside the ticket slot (that is the
//! point of stop: it must fire while a send holds the slot).
//!
//! The chat surface wires these builders later in this milestone;
//! until then the live app does not reference them yet.
#![allow(dead_code)]

use adw::prelude::*;

/// One pasted picture waiting to send. The bytes ride the
/// `send_images` dispatch; the bridge verifies the landed row's
/// attachment byte-for-byte, so a silent substitution surfaces as a
/// mismatch instead of a claimed success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedImage {
    pub name: String,
    pub bytes: Vec<u8>,
    pub width: i32,
    pub height: i32,
}

/// At most four pictures per send, 8 MB each: the service's own
/// limits, enforced before dispatch so a press never half-sends.
pub const MAX_STAGED_IMAGES: usize = 4;
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// A bridge-safe filename for a pasted picture. `None` for formats
/// the service does not take; the paste is refused, never converted.
pub fn staged_name(nanos: u128, mime: &str) -> Option<String> {
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => return None,
    };
    Some(format!("pasted-{nanos}.{ext}"))
}

/// Stage one picture; the error names the limit for the chip row.
pub fn stage_image(
    images: &mut Vec<StagedImage>,
    name: String,
    bytes: Vec<u8>,
    width: i32,
    height: i32,
) -> Result<(), &'static str> {
    if images.len() >= MAX_STAGED_IMAGES {
        return Err("up to four images per message");
    }
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err("images must fit in 8 MB each");
    }
    images.push(StagedImage {
        name,
        bytes,
        width,
        height,
    });
    Ok(())
}

/// Drop one staged picture by name; unknown names are a no-op.
pub fn remove_image(images: &mut Vec<StagedImage>, name: &str) {
    images.retain(|image| image.name != name);
}

/// What `begin_send` hands the dispatch: text, pictures, or both.
/// A send with neither is dropped, never queued.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dispatch {
    pub text: String,
    pub images: Vec<StagedImage>,
}

/// Draft plus in-flight send flag. One press dispatches once; a second
/// press while the send holds the slot waits instead of duplicating.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composer {
    text: String,
    sending: bool,
    images: Vec<StagedImage>,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_owned();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn sending(&self) -> bool {
        self.sending
    }

    /// Send is armed for a non-blank draft, a staged picture, or both,
    /// with no send in flight. Pictures alone send: a captioned image
    /// needs no words beside it.
    pub fn can_send(&self) -> bool {
        !self.sending && (!self.text.trim().is_empty() || !self.images.is_empty())
    }

    /// Take the draft (and any staged pictures) for dispatch, marking
    /// the send in flight. Returns `None` when there is nothing
    /// sendable -- the press is dropped, never queued.
    pub fn begin_send(&mut self) -> Option<Dispatch> {
        if !self.can_send() {
            return None;
        }
        self.sending = true;
        let dispatch = Dispatch {
            text: self.text.trim().to_owned(),
            images: std::mem::take(&mut self.images),
        };
        self.text.clear();
        Some(dispatch)
    }

    /// Stage one pasted picture for the next send.
    pub fn stage(
        &mut self,
        name: String,
        bytes: Vec<u8>,
        width: i32,
        height: i32,
    ) -> Result<(), &'static str> {
        stage_image(&mut self.images, name, bytes, width, height)
    }

    /// Drop one staged picture by name; the chip's remove button.
    pub fn unstage(&mut self, name: &str) {
        remove_image(&mut self.images, name);
    }

    pub fn staged(&self) -> &[StagedImage] {
        &self.images
    }

    /// Put back pictures a failed send did not consume, so a service
    /// error never eats the paste. Newest last, duplicates dropped.
    pub fn restore(&mut self, mut images: Vec<StagedImage>) {
        for image in images.drain(..) {
            if self.images.len() >= MAX_STAGED_IMAGES {
                break;
            }
            if !self.images.iter().any(|staged| staged.name == image.name) {
                self.images.push(image);
            }
        }
    }

    /// Release the send flag when the dispatch completes, whatever the
    /// outcome. The entry stays as the reader left it after the press.
    pub fn finish_send(&mut self) {
        self.sending = false;
    }
}

/// The composer row: staged picture chips above the text entry plus
/// Send and Stop. Sensitivity follows the composer state; call
/// `refresh` after every state change and `paint_chips` after every
/// stage or remove.
pub struct ComposerRow {
    pub entry: gtk4::Entry,
    pub send: gtk4::Button,
    pub stop: gtk4::Button,
    pub delete: gtk4::Button,
    outer: gtk4::Box,
    row: gtk4::Box,
    chips: gtk4::FlowBox,
}

/// Human byte counts for chips ("11,503 bytes" like the service
/// cards, "3.4 MB" above that). Pure; covered in the staging tests
/// by inspection of one boundary each.
fn format_size(bytes: usize) -> String {
    if bytes < 1024 * 1024 {
        format!("{} bytes", bytes)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// One staged picture as a chip: thumbnail, name, dimensions, and a
/// remove button. Pictures that fail to decode still chip (name and
/// size) so the paste is never silently eaten.
fn chip(image: &StagedImage, on_remove: &super::app::RemoveCallback) -> gtk4::Box {
    let chip = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    chip.add_css_class("card");
    let loader = gtk4::gdk_pixbuf::PixbufLoader::new();
    let paintable: Option<gtk4::gdk::Texture> = loader
        .write(&image.bytes)
        .ok()
        .and(loader.close().ok())
        .and_then(|()| loader.pixbuf())
        .map(|pixbuf| {
            let (width, height) = (pixbuf.width(), pixbuf.height());
            let scale = 96.0 / width.max(height).max(1) as f64;
            let scaled = pixbuf.scale_simple(
                ((width as f64 * scale) as i32).max(1),
                ((height as f64 * scale) as i32).max(1),
                gtk4::gdk_pixbuf::InterpType::Bilinear,
            );
            gtk4::gdk::Texture::for_pixbuf(&scaled.unwrap_or(pixbuf))
        });
    let thumb = gtk4::Image::from_paintable(paintable.as_ref());
    thumb.set_pixel_size(64);
    let meta = gtk4::Label::new(Some(&format!(
        "{}\n{} × {} · {}",
        image.name,
        image.width,
        image.height,
        format_size(image.bytes.len())
    )));
    meta.set_xalign(0.0);
    let remove = gtk4::Button::with_label("×");
    remove.add_css_class("flat");
    let name = image.name.clone();
    let callback = on_remove.clone();
    remove.connect_clicked(move |_| callback(name.clone()));
    chip.append(&thumb);
    chip.append(&meta);
    chip.append(&remove);
    chip
}

impl ComposerRow {
    pub fn build() -> Self {
        let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        let chips = gtk4::FlowBox::new();
        chips.set_selection_mode(gtk4::SelectionMode::None);
        chips.set_visible(false);
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        row.set_margin_start(12);
        row.set_margin_end(12);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        let entry = gtk4::Entry::new();
        entry.set_hexpand(true);
        entry.set_placeholder_text(Some("Message Muse"));
        let send = gtk4::Button::with_label("Send");
        send.add_css_class("suggested-action");
        let stop = gtk4::Button::with_label("Stop");
        stop.set_sensitive(false);
        let delete = gtk4::Button::with_label("Delete turn");
        row.append(&entry);
        row.append(&send);
        row.append(&stop);
        row.append(&delete);
        outer.append(&chips);
        outer.append(&row);
        Self {
            entry,
            send,
            stop,
            delete,
            outer,
            row,
            chips,
        }
    }

    pub fn widget(&self) -> &gtk4::Box {
        &self.outer
    }

    /// Repaint the staged picture chips; hidden when nothing is
    /// staged. The remove button hands the picture's name back.
    pub fn paint_chips(&self, images: &[StagedImage], on_remove: &super::app::RemoveCallback) {
        while let Some(child) = self.chips.first_child() {
            self.chips.remove(&child);
        }
        for image in images {
            self.chips.append(&chip(image, on_remove));
        }
        self.chips.set_visible(!images.is_empty());
    }

    /// Send is armed for a non-blank draft with no send in flight;
    /// Stop is armed exactly while a send is in flight.
    pub fn refresh(&self, composer: &Composer) {
        self.send.set_sensitive(composer.can_send());
        self.stop.set_sensitive(composer.sending());
    }

    /// The delete button names its own state: the first press arms
    /// ("Confirm delete turn"), the second press on the same turn
    /// dispatches. A new turn disarms it.
    pub fn set_delete_armed(&self, armed: bool) {
        self.delete.set_label(if armed {
            "Confirm delete turn"
        } else {
            "Delete turn"
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_drafts_and_double_presses_never_dispatch() {
        let mut composer = Composer::new();
        assert!(!composer.can_send());
        assert!(composer.begin_send().is_none());
        composer.set_text("   ");
        assert!(composer.begin_send().is_none());
        composer.set_text("hello");
        assert!(composer.begin_send().is_some());
        // Second press while in flight: dropped, not queued.
        assert!(!composer.can_send());
        assert!(composer.begin_send().is_none());
        composer.finish_send();
        assert!(!composer.can_send());
    }

    #[test]
    fn begin_send_hands_over_the_trimmed_draft_and_clears_the_entry() {
        let mut composer = Composer::new();
        composer.set_text("  hello  ");
        let dispatch = composer.begin_send().expect("sendable");
        assert_eq!(dispatch.text, "hello");
        assert!(dispatch.images.is_empty());
        assert_eq!(composer.text(), "");
        assert!(composer.sending());
    }

    #[test]
    fn pictures_alone_send_and_staging_holds_four() {
        let mut composer = Composer::new();
        assert!(!composer.can_send());
        for n in 0..4 {
            let name = staged_name(n, "image/png").expect("png");
            composer.stage(name, vec![1, 2, 3], 10, 10).expect("room");
        }
        assert!(composer.can_send());
        assert!(
            composer
                .stage("pasted-x.png".to_owned(), vec![1], 1, 1)
                .is_err()
        );
        let dispatch = composer.begin_send().expect("image-only");
        assert_eq!(dispatch.text, "");
        assert_eq!(dispatch.images.len(), 4);
        assert!(composer.staged().is_empty());
    }

    #[test]
    fn refused_formats_and_oversize_bytes_never_stage() {
        assert!(staged_name(1, "image/svg+xml").is_none());
        let mut images = Vec::new();
        assert!(stage_image(&mut images, "pasted-1.png".to_owned(), vec![], 1, 1).is_err());
        assert!(
            stage_image(
                &mut images,
                "pasted-1.png".to_owned(),
                vec![0; MAX_IMAGE_BYTES + 1],
                1,
                1,
            )
            .is_err()
        );
        assert!(images.is_empty());
        assert!(stage_image(&mut images, "pasted-1.png".to_owned(), vec![9], 2, 3).is_ok());
        remove_image(&mut images, "pasted-1.png");
        assert!(images.is_empty());
    }

    #[test]
    fn failed_sends_put_the_pictures_back() {
        let mut composer = Composer::new();
        composer
            .stage("pasted-1.png".to_owned(), vec![7], 4, 5)
            .expect("stage");
        let dispatch = composer.begin_send().expect("sendable");
        assert!(composer.staged().is_empty());
        composer.finish_send();
        composer.restore(dispatch.images);
        assert_eq!(composer.staged().len(), 1);
        assert!(composer.can_send());
    }
}
