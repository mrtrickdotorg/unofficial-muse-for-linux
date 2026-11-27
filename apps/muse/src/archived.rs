//! The sessions this seat archived, remembered locally so they can be
//! unarchived again.
//!
//! Archiving works on the service: `session.archive` answers
//! `{"archived": true, "updated": true}` and `session.unarchive` puts the
//! session back. What does not work is finding the archived row afterwards: the
//! moment a session is archived, `sessions.list` stops returning it, and
//! `include_archived` / `archived` bodies answer with the same shortened list.
//! `sessions.get` still resolves the id -- but nothing lists it, so no row can
//! offer Unarchive.
//!
//! So the flag stays the service's and the reachability is ours: this module
//! remembers what the app archived (id and title) beside the account link, and
//! the Threads page paints those rows in an Archived section. Unarchiving
//! forgets the row and re-reads the listing, which lists it again.
//!
//! The memory prunes itself only on fresh evidence: a listing that was just
//! read and still names an id means the id is archived no longer, so it leaves
//! the ledger ([`sweep`]). Painting never prunes ([`merge`]), because the
//! listing a page paints from can lag the write it is painting: right after an
//! archive the app still holds the pre-archive listing, which names the row the
//! memory has just taken on. Pruning there would throw the memory away in the
//! same breath that wrote it, and the row would vanish from the window with
//! nothing left to unarchive.

use std::path::{Path, PathBuf};

use muse_account::Thread;

/// How many archived sessions are remembered. Archiving is a deliberate press,
/// so this is history, not a working set; the oldest falls off first.
const MAX: usize = 200;

/// The group the Threads page paints archived rows under.
pub(crate) const GROUP: &str = "Archived";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Remembered {
    id: String,
    title: String,
}

/// The ledger's own file, beside the account link, so a test home scopes it
/// exactly as it scopes the link. Under test there is no ledger at all: the
/// rules below take their path from the tests, so no test can read or spend the
/// machine's real memory.
#[cfg(not(test))]
fn ledger_path() -> Option<PathBuf> {
    crate::session::account_dir()
        .and_then(|account| account.parent().map(Path::to_path_buf))
        .map(|dir| dir.join("archived-sessions.json"))
}

#[cfg(test)]
fn ledger_path() -> Option<PathBuf> {
    None
}

/// The ledger as the file carries it. A file that is not a list of rows is no
/// memory at all, never an error the page has to handle.
fn load_at(path: &Path) -> Vec<Remembered> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Atomic, like the window's own writes: a temporary file beside it, then a
/// rename, so a reader never sees half a ledger. Best effort -- losing it costs
/// at most a row the reader archived earlier, never a session.
fn save_at(path: &Path, rows: &[Remembered]) {
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(text) = serde_json::to_string(rows) else {
        return;
    };
    let temporary = path.with_extension("json.part");
    if std::fs::write(&temporary, text).is_err() {
        return;
    }
    let _ = std::fs::rename(&temporary, path);
}

/// Drop every remembered id `named` names, writing only when something left.
fn drop_named_at(path: &Path, named: impl Fn(&str) -> bool) {
    let rows = load_at(path);
    let kept: Vec<Remembered> = rows.iter().filter(|row| !named(&row.id)).cloned().collect();
    if kept.len() != rows.len() {
        save_at(path, &kept);
    }
}

/// Remember one archived session: newest first, one row per id, bounded.
fn remember_at(path: &Path, id: &str, title: &str) {
    if id.is_empty() {
        return;
    }
    let title = title.trim();
    let title = if title.is_empty() {
        "(untitled)"
    } else {
        title
    };
    let mut rows = load_at(path);
    rows.retain(|row| row.id != id);
    rows.insert(
        0,
        Remembered {
            id: id.to_owned(),
            title: title.to_owned(),
        },
    );
    rows.truncate(MAX);
    save_at(path, &rows);
}

/// Forget one session: it was unarchived, deleted, or listed again.
fn forget_at(path: &Path, id: &str) {
    drop_named_at(path, |kept| kept == id);
}

/// [`remember_at`] against the ledger on disk.
pub(crate) fn remember(id: &str, title: &str) {
    if let Some(path) = ledger_path() {
        remember_at(&path, id, title);
    }
}

/// [`forget_at`] against the ledger on disk.
pub(crate) fn forget(id: &str) {
    if let Some(path) = ledger_path() {
        forget_at(&path, id);
    }
}

/// The listing as the Threads page paints it: the service's own rows plus the
/// rows this seat archived. A row the listing still names while the memory
/// holds it is painted archived -- that listing can be the one that predates
/// the archive the page is about to show -- and a remembered row the listing no
/// longer names is appended under Archived. Nothing is forgotten here: painting
/// must not spend the memory it is painting from.
fn merge(listing: &[Thread], remembered: &[Remembered]) -> Vec<Thread> {
    let held: std::collections::HashSet<&str> =
        remembered.iter().map(|row| row.id.as_str()).collect();
    let mut rows: Vec<Thread> = listing
        .iter()
        .map(|thread| {
            // The service never lists an archived row, so a remembered id here
            // is a listing that predates the archive being painted.
            if held.contains(thread.session_id.as_str()) {
                Thread {
                    archived: true,
                    unread_count: 0,
                    ..thread.clone()
                }
            } else {
                thread.clone()
            }
        })
        .collect();
    let named: std::collections::HashSet<&str> = listing
        .iter()
        .map(|thread| thread.session_id.as_str())
        .collect();
    for row in remembered
        .iter()
        .filter(|row| !named.contains(row.id.as_str()))
    {
        rows.push(Thread {
            session_id: row.id.clone(),
            title: row.title.clone(),
            archived: true,
            pinned: false,
            is_thread: true,
            unread_count: 0,
            snippet: None,
            is_primary: true,
            updated_at: None,
        });
    }
    rows
}

/// [`merge`] against the ledger on disk. The Threads page and the web slice
/// both paint through this; painting never writes the ledger.
pub(crate) fn with_archived(listing: &[Thread]) -> Vec<Thread> {
    let remembered = ledger_path().map(|path| load_at(&path)).unwrap_or_default();
    merge(listing, &remembered)
}

/// The memory without every id a freshly read listing names again: the service
/// lists it, so it is archived no longer (unarchived by this app, by another
/// client, or on the site).
fn sweep_at(path: &Path, listing: &[Thread]) {
    let named: std::collections::HashSet<&str> = listing
        .iter()
        .map(|thread| thread.session_id.as_str())
        .collect();
    drop_named_at(path, |id| named.contains(id));
}

/// [`sweep_at`] against the ledger on disk. Called where a listing was just
/// read from the service -- never while painting.
pub(crate) fn sweep(listing: &[Thread]) {
    if let Some(path) = ledger_path() {
        sweep_at(&path, listing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str) -> Thread {
        Thread {
            session_id: id.to_owned(),
            title: format!("{id} title"),
            archived: false,
            pinned: false,
            is_thread: true,
            unread_count: 0,
            snippet: None,
            is_primary: true,
            updated_at: None,
        }
    }

    /// A ledger path of this test's own, so parallel tests cannot collide.
    fn ledger(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("muse-archived-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("archived-sessions.json")
    }

    fn ids(rows: &[Remembered]) -> Vec<&str> {
        rows.iter().map(|row| row.id.as_str()).collect()
    }

    /// A session the service stopped listing is still a row, marked archived:
    /// that is what makes its Unarchive reachable.
    #[test]
    fn an_archived_session_the_listing_dropped_is_painted_again() {
        let listing = vec![thread("kept")];
        let memory = vec![Remembered {
            id: "gone".into(),
            title: "gone title".into(),
        }];
        let rows = merge(&listing, &memory);
        assert_eq!(rows.len(), 2);
        let restored = rows.iter().find(|row| row.session_id == "gone").unwrap();
        assert!(restored.archived, "the row is painted as archived");
        assert_eq!(restored.title, "gone title");
    }

    /// The listing the page paints from can be the one that predates the
    /// archive: it still names the row the memory has just taken on, and that
    /// row must paint archived rather than as a live one -- and painting must
    /// not spend the memory, or the row would vanish on the next fill.
    #[test]
    fn a_listed_row_the_memory_holds_is_painted_archived() {
        let listing = vec![thread("just"), thread("other")];
        let memory = vec![Remembered {
            id: "just".into(),
            title: "just title".into(),
        }];
        let rows = merge(&listing, &memory);
        assert_eq!(rows.len(), 2, "no second row for the same id");
        let row = rows.iter().find(|row| row.session_id == "just").unwrap();
        assert!(row.archived, "the stale listing still paints it archived");
        assert!(
            !rows
                .iter()
                .any(|row| row.session_id == "other" && row.archived)
        );
        assert_eq!(memory.len(), 1, "painting keeps the memory");
    }

    /// Only a freshly read listing sweeps: an id it names is archived no
    /// longer, and it leaves the ledger instead of becoming a ghost row.
    #[test]
    fn a_fresh_listing_sweeps_what_it_names() {
        let path = ledger("sweep");
        remember_at(&path, "back", "back title");
        remember_at(&path, "gone", "gone title");
        sweep_at(&path, &[thread("back")]);
        assert_eq!(ids(&load_at(&path)), vec!["gone"]);
    }

    /// Remembering is newest-first, one row per id and bounded; forgetting
    /// drops one id and writes nothing when there is nothing to drop.
    #[test]
    fn remembering_is_newest_first_bounded_and_forgetting_removes_one_id() {
        let path = ledger("remember");
        assert!(load_at(&path).is_empty(), "no file is no memory");
        remember_at(&path, "a", "A title");
        remember_at(&path, "b", "  ");
        remember_at(&path, "a", "A again");
        let rows = load_at(&path);
        assert_eq!(ids(&rows), vec!["a", "b"], "one row per id, newest first");
        assert_eq!(rows[0].title, "A again");
        assert_eq!(rows[1].title, "(untitled)", "an empty title still reads");

        for i in 0..MAX + 5 {
            remember_at(&path, &format!("s{i}"), "spare");
        }
        let rows = load_at(&path);
        assert_eq!(rows.len(), MAX, "the oldest fall off");
        assert_eq!(ids(&rows)[0], format!("s{}", MAX + 4));

        forget_at(&path, "s10");
        assert_eq!(load_at(&path).len(), MAX - 1);
        forget_at(&path, "s10");
        assert_eq!(load_at(&path).len(), MAX - 1, "nothing to drop, no write");
    }

    /// The ledger round-trips through its file, and a file that is not a list
    /// of rows is no memory rather than an error.
    #[test]
    fn the_ledger_round_trips_and_a_bad_file_is_no_memory() {
        let path = ledger("round-trip");
        let rows = vec![
            Remembered {
                id: "a".into(),
                title: "a".into(),
            },
            Remembered {
                id: "b".into(),
                title: "b".into(),
            },
        ];
        save_at(&path, &rows);
        assert_eq!(load_at(&path), rows);
        std::fs::write(&path, "{\"not\":\"a list\"}").unwrap();
        assert!(load_at(&path).is_empty());
    }
}
