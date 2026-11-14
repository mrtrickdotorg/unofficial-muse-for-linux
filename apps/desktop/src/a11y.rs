//! Accessible names for the controls whose visible content does
//! not name them.
//!
//! GTK names most controls from their contents on its own: a
//! button built with text is announced by that text. What it
//! cannot name is a control whose content is an icon or a glyph --
//! the rail's section buttons, the "+" over the side-chat list,
//! the arrow send button, the panel toggles, the thread menu, the
//! Feed's heart. A sighted user gets those from the tooltip; a
//! screen reader gets nothing unless the accessible Label
//! property says it, in the same words the tooltip already uses.
//! This pass (Nov 14, 2026) names exactly that set, plus the text
//! entries whose only description is a placeholder, which GTK
//! does not promote to a name. A live AT-SPI client read remains
//! the proof the names are heard as intended; nothing here
//! claims it.

use gtk::prelude::*;
use gtk4 as gtk;

/// Set the name a screen reader announces for `widget`.
pub fn name<W>(widget: &W, label: &str)
where
    W: IsA<gtk::Accessible>,
{
    widget.update_property(&[gtk::accessible::Property::Label(label)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a display; run under Xvfb like the other GTK tests"]
    fn glyph_controls_and_entries_take_names_without_losing_content() {
        gtk::init().expect("gtk init");
        let glyph = gtk::Button::with_label("+");
        name(&glyph, "Start a new side chat");
        assert_eq!(glyph.label().as_deref(), Some("+"));
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("Search your conversations"));
        name(&entry, "Search your conversations");
        assert_eq!(
            entry.placeholder_text().as_deref(),
            Some("Search your conversations")
        );
    }
}
