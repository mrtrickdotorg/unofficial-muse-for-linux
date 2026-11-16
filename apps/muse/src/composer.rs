//! Message composer for the rewrite: pure send-state plus the entry
//! row widgets. The entry holds the draft; Send dispatches exactly
//! once per press; Stop cancels outside the ticket slot (that is the
//! point of stop: it must fire while a send holds the slot).
//!
//! The chat surface wires these builders later in this milestone;
//! until then the live app does not reference them yet.
#![allow(dead_code)]

use adw::prelude::*;

/// Draft plus in-flight send flag. One press dispatches once; a second
/// press while the send holds the slot waits instead of duplicating.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composer {
    text: String,
    sending: bool,
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

    /// Send is armed only for a non-blank draft with no send in flight.
    pub fn can_send(&self) -> bool {
        !self.sending && !self.text.trim().is_empty()
    }

    /// Take the draft for dispatch, marking the send in flight. Returns
    /// `None` when there is nothing sendable -- the press is dropped,
    /// never queued.
    pub fn begin_send(&mut self) -> Option<String> {
        if !self.can_send() {
            return None;
        }
        self.sending = true;
        let draft = self.text.trim().to_owned();
        self.text.clear();
        Some(draft)
    }

    /// Release the send flag when the dispatch completes, whatever the
    /// outcome. The entry stays as the reader left it after the press.
    pub fn finish_send(&mut self) {
        self.sending = false;
    }
}

/// The composer row: text entry plus Send and Stop. Sensitivity follows
/// the composer state; call `refresh` after every state change.
pub struct ComposerRow {
    pub entry: gtk4::Entry,
    pub send: gtk4::Button,
    pub stop: gtk4::Button,
    pub delete: gtk4::Button,
    row: gtk4::Box,
}

impl ComposerRow {
    pub fn build() -> Self {
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
        Self {
            entry,
            send,
            stop,
            delete,
            row,
        }
    }

    pub fn widget(&self) -> &gtk4::Box {
        &self.row
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
        assert_eq!(composer.begin_send().as_deref(), Some("hello"));
        assert_eq!(composer.text(), "");
        assert!(composer.sending());
    }
}
