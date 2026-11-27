//! The app window's geometry, in the same file the Tauri window writes.
//!
//! The shell window is its own process (`apps/muse-window`), so this module and
//! that crate's `window_state` share one contract rather than one code path:
//! `$XDG_CONFIG_HOME/unofficial-muse-for-linux/window-state.json`. Whichever
//! window a launch opens, the reader's size, maximized state and -- where the
//! platform allows it -- position come back, and an XDG config home scopes the
//! state completely, so a test home starts from its own geometry.
//!
//! A launch with nothing saved opens maximized. This window is a whole app, not
//! a utility panel, so its first size is the primary display; a GTK 4 window
//! may not place itself on Wayland, which is why a saved position is carried
//! over from the file rather than set here.

use gtk4::prelude::*;

use std::path::PathBuf;

/// The smallest window the shell still reads in: every page stays listed and
/// History scrolls below them.
pub(crate) const MIN_WIDTH: i32 = 900;
pub(crate) const MIN_HEIGHT: i32 = 600;

/// What the file remembers. `x`/`y` are optional: this window cannot place
/// itself, so it keeps whatever position the Tauri window last wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Geometry {
    pub width: i32,
    pub height: i32,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub maximized: bool,
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".config"))
        })?;
    Some(
        base.join("unofficial-muse-for-linux")
            .join("window-state.json"),
    )
}

/// The file's own text as geometry: a size below the minimum is not geometry,
/// and a position needs both coordinates.
pub(crate) fn decode(text: &str) -> Option<Geometry> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let width = i32::try_from(value.get("width")?.as_u64()?).ok()?;
    let height = i32::try_from(value.get("height")?.as_u64()?).ok()?;
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return None;
    }
    let x = value
        .get("x")
        .and_then(serde_json::Value::as_i64)
        .and_then(|x| i32::try_from(x).ok());
    let y = value
        .get("y")
        .and_then(serde_json::Value::as_i64)
        .and_then(|y| i32::try_from(y).ok());
    let (x, y) = match (x, y) {
        (Some(x), Some(y)) => (Some(x), Some(y)),
        _ => (None, None),
    };
    Some(Geometry {
        width,
        height,
        x,
        y,
        maximized: value
            .get("maximized")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

pub(crate) fn encode(geometry: &Geometry) -> String {
    let mut value = serde_json::json!({
        "width": geometry.width,
        "height": geometry.height,
        "maximized": geometry.maximized,
    });
    if let (Some(x), Some(y)) = (geometry.x, geometry.y) {
        value["x"] = serde_json::json!(x);
        value["y"] = serde_json::json!(y);
    }
    value.to_string()
}

fn load() -> Option<Geometry> {
    let path = path()?;
    decode(&std::fs::read_to_string(path).ok()?)
}

/// Atomic, like the window's own write: a temporary file beside it, then a
/// rename, so a launch never reads half a geometry. Best effort.
fn save(geometry: &Geometry) {
    let Some(path) = path() else {
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let temporary = path.with_extension("json.part");
    if std::fs::write(&temporary, encode(geometry)).is_err() {
        return;
    }
    let _ = std::fs::rename(&temporary, &path);
}

/// Put the reader's geometry on the window before it is shown: the size and
/// maximized state from the file, or the whole display on a first launch.
pub(crate) fn apply(window: &gtk4::ApplicationWindow) {
    window.set_size_request(MIN_WIDTH, MIN_HEIGHT);
    match load() {
        Some(saved) => {
            window.set_default_size(saved.width, saved.height);
            if saved.maximized {
                window.maximize();
            }
        }
        // A whole app, so a whole display: the window manager owns the rest.
        None => window.maximize(),
    }
}

/// Remember the geometry when the window closes and when it is maximized or
/// restored, so the file is current even if the app is quit right after.
pub(crate) fn remember(window: &gtk4::ApplicationWindow) {
    window.connect_close_request(|window| {
        save_window(window);
        gtk4::glib::Propagation::Proceed
    });
    window.connect_maximized_notify(save_window);
}

fn save_window(window: &gtk4::ApplicationWindow) {
    let (width, height) = logical_size(window);
    // Before the window has a surface it has no size to remember: maximizing it
    // for a first launch must not write the floor in as if it were the
    // reader's window.
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return;
    }
    let previous = load();
    save(&Geometry {
        width,
        height,
        x: previous.and_then(|geometry| geometry.x),
        y: previous.and_then(|geometry| geometry.y),
        maximized: window.is_maximized(),
    });
}

/// The window's size in logical pixels: its surface while it has one, and the
/// size it asked for before that.
fn logical_size(window: &gtk4::ApplicationWindow) -> (i32, i32) {
    if let Some(surface) = window.surface() {
        let width = surface.width();
        let height = surface.height();
        if width > 0 && height > 0 {
            return (width, height);
        }
    }
    window.default_size()
}

#[cfg(test)]
mod tests {
    use super::{Geometry, MIN_HEIGHT, MIN_WIDTH, decode, encode};

    #[test]
    fn a_geometry_round_trips_through_its_own_text() {
        let geometry = Geometry {
            width: 1600,
            height: 1000,
            x: Some(220),
            y: Some(140),
            maximized: false,
        };
        assert_eq!(decode(&encode(&geometry)), Some(geometry));
    }

    /// This window cannot place itself, so it writes the size and the maximized
    /// flag, keeps the position the other window wrote, and never invents one.
    #[test]
    fn a_geometry_without_a_position_still_round_trips() {
        let geometry = Geometry {
            width: 1200,
            height: 800,
            x: None,
            y: None,
            maximized: true,
        };
        let text = encode(&geometry);
        assert!(!text.contains("\"x\""), "no position in {text}");
        assert_eq!(decode(&text), Some(geometry));
    }

    #[test]
    fn a_geometry_below_the_minimum_is_not_restored() {
        assert_eq!(decode(""), None);
        assert_eq!(decode(r#"{"width":1200}"#), None);
        let tiny = format!(
            r#"{{"width":{},"height":{},"maximized":false}}"#,
            MIN_WIDTH - 1,
            MIN_HEIGHT - 1
        );
        assert_eq!(decode(&tiny), None);
        let smallest = format!(
            r#"{{"width":{},"height":{},"maximized":false}}"#,
            MIN_WIDTH, MIN_HEIGHT
        );
        assert!(decode(&smallest).is_some());
    }
}
