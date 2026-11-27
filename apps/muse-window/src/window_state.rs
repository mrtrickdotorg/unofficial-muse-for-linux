//! This window's geometry, kept in the app's own config folder.
//!
//! The app and its window are two processes but one application, so there is
//! one saved geometry: `$XDG_CONFIG_HOME/unofficial-muse-for-linux/
//! window-state.json` (`$HOME/.config/...` when the variable is unset). The
//! app's GTK fallback window reads and writes the same file, so whichever
//! window a launch opens remembers the reader's size, position and maximized
//! state -- and an XDG config home scopes it completely, so a test home never
//! inherits the seat's geometry and nothing lands in a shared place.
//!
//! A launch with nothing saved opens large and maximized: this is a whole app
//! in one window, not a utility panel, so its first size covers the primary
//! display.

use std::path::PathBuf;

/// What a launch remembers.
///
/// `x`/`y` are optional because only this window can place itself: a GTK 4
/// fallback window may not choose its own position on Wayland, so its write
/// keeps whatever position the file already had (see the app's own copy of
/// this module) rather than inventing one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub maximized: bool,
}

/// The smallest window the shell still reads in: the page list keeps all of its
/// rows and History scrolls below them, which is the layout the drawer was
/// built for. Smaller than this and the app is a panel, not the app.
pub const MIN_WIDTH: f64 = 900.0;
pub const MIN_HEIGHT: f64 = 600.0;

/// What a first launch asks for before maximizing: a large usable window on any
/// display, and the whole surface when the window manager takes the maximize.
pub const FIRST_WIDTH: f64 = 1400.0;
pub const FIRST_HEIGHT: f64 = 900.0;

/// The file this window and the app's fallback window share.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".config"))
        })?;
    Some(base.join("unofficial-muse-for-linux").join("window-state.json"))
}

/// The file's own text as geometry. A size below the window's minimum is not
/// geometry -- a stale or hand-edited file must not open a window the drawer
/// cannot live in -- and a position needs both coordinates to be a position.
pub fn decode(text: &str) -> Option<Geometry> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let width = u32::try_from(value.get("width")?.as_u64()?).ok()?;
    let height = u32::try_from(value.get("height")?.as_u64()?).ok()?;
    if f64::from(width) < MIN_WIDTH || f64::from(height) < MIN_HEIGHT {
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

/// The file's text for a geometry: one small object, with the position left out
/// when this window could not name one.
pub fn encode(geometry: &Geometry) -> String {
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

pub fn load() -> Option<Geometry> {
    let path = path()?;
    decode(&std::fs::read_to_string(path).ok()?)
}

/// Write the geometry atomically (a temporary file beside it, then a rename), so
/// a launch that starts mid-write reads the old geometry rather than half of a
/// new one. Best effort: a window that cannot save still opens.
pub fn save(geometry: &Geometry) {
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
        let raised = Geometry {
            maximized: true,
            ..geometry
        };
        assert_eq!(decode(&encode(&raised)), Some(raised));
    }

    /// A window that cannot name its position still remembers its size: the
    /// file simply has no position, and one coordinate alone is not one.
    #[test]
    fn a_geometry_without_a_position_saves_without_one() {
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
        let half = r#"{"width":1200,"height":800,"x":12,"maximized":false}"#;
        assert_eq!(decode(half).and_then(|g| g.x), None);
    }

    /// Nothing else is geometry: a missing file, junk, or a size the drawer
    /// cannot be read in leaves the launch to its own large default.
    #[test]
    fn a_geometry_below_the_minimum_is_not_restored() {
        assert_eq!(decode(""), None);
        assert_eq!(decode("not json"), None);
        assert_eq!(decode("{}"), None);
        let tiny = format!(
            r#"{{"width":{},"height":{},"maximized":false}}"#,
            MIN_WIDTH as u32 - 1,
            MIN_HEIGHT as u32 - 1
        );
        assert_eq!(decode(&tiny), None);
        let smallest = format!(
            r#"{{"width":{},"height":{},"maximized":false}}"#,
            MIN_WIDTH as u32, MIN_HEIGHT as u32
        );
        assert!(decode(&smallest).is_some());
    }
}
