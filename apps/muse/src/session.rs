//! Account session for the rewrite: saved-connection persistence with the
//! same file contract as the shipped client, plus a single-flight ticket
//! discipline for service reads. Transport itself stays in `muse-account`;
//! this module owns sequencing, never protocol shapes.
//!
//! Milestone 3 wires this into the chat surface; until then the live app
//! does not reference it yet.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Which service read currently owns the session's one in-flight slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Connect,
    Identity,
    History,
    Send,
    /// Shared by every secondary page: only the visible page reads,
    /// so one slot covers them all.
    Page,
    /// The model picker read and write. One slot: a set never races
    /// a listing.
    Model,
}

/// A held in-flight slot. Only the completion carrying a matching ticket
/// may apply its answer -- a late reply from a superseded read owns
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    op: Op,
    serial: u64,
}

/// The saved link: interpreter, session folder, and the standing grant
/// from an explicitly consented connect. Same JSON contract as the
/// shipped client (`account-connection.json`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedConnection {
    pub python: PathBuf,
    pub config_dir: PathBuf,
    pub consent: bool,
}

fn valid_saved_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.contains('\0')
        && Path::new(value).is_absolute()
}

fn saved_connection_text(python: &str, config_dir: &str, consent: bool) -> String {
    serde_json::json!({
        "python": python,
        "config_dir": config_dir,
        "consent": consent,
    })
    .to_string()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedConnectionFile {
    python: String,
    config_dir: String,
    #[serde(default)]
    consent: bool,
}

fn saved_connection_from_text(text: &str) -> Option<SavedConnection> {
    let saved: SavedConnectionFile = serde_json::from_str(text).ok()?;
    if !valid_saved_path(&saved.python) || !valid_saved_path(&saved.config_dir) {
        return None;
    }
    Some(SavedConnection {
        python: PathBuf::from(saved.python),
        config_dir: PathBuf::from(saved.config_dir),
        consent: saved.consent,
    })
}

/// Read the saved link from `dir`/`account-connection.json`. The file must
/// be a uid-owned regular file with no group/other permission bits, exactly
/// like the shipped client demands -- a lax file reads as absent, never as
/// a link.
#[cfg(unix)]
pub fn load_saved_connection_in(dir: &Path) -> Option<SavedConnection> {
    use std::os::unix::fs::MetadataExt;
    let path = dir.join("account-connection.json");
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.uid() != libc_geteuid() || metadata.mode() & 0o077 != 0 {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    if bytes.len() > 16 * 1024 {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    saved_connection_from_text(&text)
}

#[cfg(unix)]
fn libc_geteuid() -> u32 {
    // SAFETY: geteuid has no preconditions; it only reads the credential.
    unsafe { libc::geteuid() }
}

/// Persist the link atomically (temp file + rename, mode 0600) into `dir`,
/// creating the directory private (0700) when missing.
pub fn save_saved_connection_in(dir: &Path, connection: &SavedConnection) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|_| "could not create config directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "could not lock down config directory")?;
    }
    let text = saved_connection_text(
        &connection.python.display().to_string(),
        &connection.config_dir.display().to_string(),
        connection.consent,
    );
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = dir.join(format!(".account-connection.{nonce}.tmp"));
    std::fs::write(&temporary, text.as_bytes())
        .map_err(|_| "could not write persistent config file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "could not lock down persistent config file")?;
    }
    std::fs::rename(&temporary, dir.join("account-connection.json"))
        .map_err(|_| "could not install persistent config file")?;
    Ok(())
}

/// Live account state: the linked identity plus the newest transcript
/// read. One in-flight service read at a time; a second press waits
/// instead of stacking a duplicate dispatch.
pub struct Session {
    client: Option<muse_account::AccountClient>,
    config: Option<muse_account::BridgeConfig>,
    busy: Option<Ticket>,
    serial: u64,
    pub account: Option<muse_account::AccountSnapshot>,
    pub history: Vec<muse_account::HistoryMessage>,
    /// The latest usage-limit sentence the service sent, verbatim. Feeds
    /// the notice line so every at-limit send visibly returns the
    /// service's own reset message even before the next history lands.
    pub latest_quota: Option<String>,
    /// Model picker state: the selection plus the generation it came
    /// with. A listing only lands when its generation is newer than
    /// what is held, so a late answer can never move the picker
    /// backwards.
    pub model_generation: Option<i64>,
    pub selected_model: Option<String>,
    pub available_models: Vec<String>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            client: None,
            config: None,
            busy: None,
            serial: 0,
            account: None,
            history: Vec::new(),
            latest_quota: None,
            model_generation: None,
            selected_model: None,
            available_models: Vec::new(),
        }
    }

    /// True while any read holds the slot.
    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    /// Take the in-flight slot for one read. A second press while the
    /// slot is held gets nothing -- it waits instead of dispatching
    /// twice.
    pub fn try_begin(&mut self, op: Op) -> Option<Ticket> {
        if self.busy.is_some() {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        let ticket = Ticket {
            op,
            serial: self.serial,
        };
        self.busy = Some(ticket);
        Some(ticket)
    }

    /// Release the slot for a completion carrying its own ticket. A
    /// stale or foreign ticket owns nothing and releases nothing.
    pub fn accept(&mut self, ticket: &Ticket) -> bool {
        if self.busy.as_ref() == Some(ticket) {
            self.busy = None;
            true
        } else {
            false
        }
    }

    /// Attach a linked client. Called once the bridge proves the saved
    /// session belongs to the expected account.
    pub fn attach(
        &mut self,
        client: muse_account::AccountClient,
        config: muse_account::BridgeConfig,
    ) {
        self.client = Some(client);
        self.config = Some(config);
    }

    /// Record a finished connect: the account lands only on success.
    pub fn finish_connect(
        &mut self,
        ticket: &Ticket,
        result: Result<muse_account::AccountSnapshot, muse_account::AccountError>,
    ) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        match result {
            Ok(account) => {
                self.account = Some(account);
                true
            }
            Err(_) => false,
        }
    }

    /// Prepend an older window ahead of the loaded rows, dropping rows
    /// already held. Used by Load-older; the ticket discipline is the
    /// caller's (the window read holds the History slot).
    pub fn prepend_history(&mut self, rows: Vec<muse_account::HistoryMessage>) {
        use std::collections::HashSet;
        let known: HashSet<&str> = self
            .history
            .iter()
            .map(|row| row.message_id.as_str())
            .collect();
        let fresh: Vec<_> = rows
            .into_iter()
            .filter(|row| !known.contains(row.message_id.as_str()))
            .collect();
        let mut combined = Vec::with_capacity(fresh.len() + self.history.len());
        combined.extend(fresh);
        combined.append(&mut self.history);
        self.history = combined;
    }

    /// Remember the service's usage-limit sentence verbatim. Only the
    /// recognized sentence lands here, never reader text.
    pub fn note_quota(&mut self, text: String) {
        self.latest_quota = Some(text);
    }

    /// Record a model listing when its generation is newer than what
    /// is held. Stale answers own nothing.
    pub fn note_model(&mut self, info: &muse_account::ModelInfo) {
        let newer = match (self.model_generation, info.generation) {
            (_, None) => true,
            (None, Some(_)) => true,
            (Some(held), Some(seen)) => seen > held,
        };
        if newer {
            self.model_generation = info.generation;
            self.selected_model = info
                .selected_model
                .clone()
                .or_else(|| info.default_model.clone());
            self.available_models = info.available_models.clone();
        }
    }

    /// A cheap fingerprint of the held history: row count plus the
    /// newest row's id. The transcript skips its full widget rebuild
    /// when this is unchanged -- rebuilding hundreds of labels every
    /// poll is what made long threads stutter.
    pub fn history_fingerprint(&self) -> (usize, Option<String>) {
        let last = self.history.last().map(|row| row.message_id.clone());
        (self.history.len(), last)
    }

    /// Record a finished history read: the rows land only on success.
    pub fn finish_history(
        &mut self,
        ticket: &Ticket,
        result: Result<Vec<muse_account::HistoryMessage>, muse_account::AccountError>,
    ) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        match result {
            Ok(rows) => {
                self.history = rows;
                true
            }
            Err(_) => false,
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// The account folder both clients share: `$XDG_CONFIG_HOME` (or
/// `~/.config`) plus `unofficial-muse-for-linux/account`. The new app
/// reads the same saved link, so signing in once links both.
pub fn account_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        })?;
    Some(base.join("unofficial-muse-for-linux/account"))
}

/// The saved link, when one exists and passes the permission checks.
pub fn load_saved_link() -> Option<SavedConnection> {
    load_saved_connection_in(&account_dir()?)
}

/// Locate a bridge script the way the shipped client does: packaged
/// install first, source checkout second. Same search order, so the
/// new app runs wherever the old one does.
pub fn bridge_script(name: &str) -> std::path::PathBuf {
    let mut roots = Vec::new();
    if let Some(appdir) = std::env::var_os("APPDIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        roots.push(appdir);
    }
    if let Ok(executable) = std::env::current_exe() {
        roots.extend(
            executable
                .ancestors()
                .skip(1)
                .map(std::path::Path::to_path_buf),
        );
    }
    roots.push(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    for root in &roots {
        for path in [
            root.join("usr/share/unofficial-muse-for-linux/account")
                .join(name),
            root.join("scripts").join(name),
        ] {
            if path.is_file() {
                return path;
            }
        }
    }
    std::path::PathBuf::from("/usr/share/unofficial-muse-for-linux/account").join(name)
}

/// Roots under which a session folder is fragile: a cache cleaner or
/// a temp sweep can delete the sign-in the app depends on. Ported from
/// the shipped client (found Nov 10, 2026): a saved preference pointing
/// there was a fallback, never a real choice.
pub fn fragile_roots() -> Vec<std::path::PathBuf> {
    let mut roots = vec![
        std::env::temp_dir(),
        std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|home| home.join(".cache"))
                    .unwrap_or_default()
            }),
    ];
    if let Some(tmpdir) = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        roots.push(tmpdir);
    }
    roots
}

/// True when `path` sits under a cache or temp root.
pub fn is_fragile_session_dir(path: &std::path::Path, roots: &[std::path::PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Copy the session files from `old` into `new` (created 0700, files
/// 0600). When `new` already holds a cookies file at least as fresh as
/// the old one, the destination is the live session and nothing is
/// copied. Fails only when the old folder holds a cookies file the copy
/// could not reproduce: relocation must never strand a working sign-in
/// in an empty folder.
pub fn copy_session_dir(old: &std::path::Path, new: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(new).map_err(|_| "could not create the app session folder")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(new, std::fs::Permissions::from_mode(0o700));
    }
    let modified = |base: &std::path::Path| {
        std::fs::metadata(base.join("cookies.txt"))
            .and_then(|meta| meta.modified())
            .ok()
    };
    if let (Some(source), Some(dest)) = (modified(old), modified(new))
        && dest >= source
    {
        return Ok(());
    }
    let entries = match std::fs::read_dir(old) {
        Ok(entries) => entries,
        Err(_) => return Ok(()), // No old folder: nothing to carry over.
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let dest = new.join(entry.file_name());
        if std::fs::copy(&path, &dest).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    if old.join("cookies.txt").is_file() && !new.join("cookies.txt").is_file() {
        return Err("could not copy the saved session into the app folder".into());
    }
    Ok(())
}

/// The session folder the app should actually use for a saved link. A
/// fragile (cache/temp) location relocates to the app's own folder,
/// carrying the session with it, and the link is re-saved so the move
/// happens once. Any failure leaves the saved path untouched: a working
/// sign-in in a bad place beats a broken sign-in in a good one.
pub fn effective_config_dir(
    python: &std::path::Path,
    saved: &std::path::Path,
    consent: bool,
) -> std::path::PathBuf {
    if !is_fragile_session_dir(saved, &fragile_roots()) {
        return saved.to_owned();
    }
    let Some(app_dir) = account_dir() else {
        return saved.to_owned();
    };
    if app_dir == saved {
        return saved.to_owned();
    }
    match copy_session_dir(saved, &app_dir) {
        Ok(()) => {
            let link = SavedConnection {
                python: python.to_owned(),
                config_dir: app_dir.clone(),
                consent,
            };
            let _ = save_saved_connection_in(&app_dir, &link);
            app_dir
        }
        Err(_) => saved.to_owned(),
    }
}

/// The bridge venv's interpreter, when the setup script has run:
/// `<data-home>/unofficial-muse-for-linux/account-bridge/.venv/bin/python`.
/// This one runs both the bridge and the login helper.
pub fn bridge_venv_python() -> Option<std::path::PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute());
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute());
    bridge_venv_python_in(data_home, home)
}

/// The venv lookup with explicit roots, so tests never touch the
/// process environment.
pub fn bridge_venv_python_in(
    data_home: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    let base = data_home.or_else(|| home.map(|home| home.join(".local/share")))?;
    let python = base.join("unofficial-muse-for-linux/account-bridge/.venv/bin/python");
    python.is_file().then_some(python)
}

/// First `python3` found in the given directories. The login helper is
/// stdlib-only, so any working python3 signs in; the bridge needs the
/// venv above.
pub fn find_python_in(dirs: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    dirs.iter()
        .map(|dir| dir.join("python3"))
        .find(|path| path.is_file())
}

/// System pythons in the usual places, plus whatever `PATH` offers.
pub fn system_pythons() -> Vec<std::path::PathBuf> {
    let mut roots = vec![
        std::path::PathBuf::from("/usr/bin"),
        std::path::PathBuf::from("/usr/local/bin"),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        roots.extend(std::env::split_paths(&path));
    }
    let mut seen = std::collections::HashSet::new();
    roots
        .into_iter()
        .filter(|root| seen.insert(root.clone()))
        .collect()
}

/// The interpreter to offer: the bridge venv when set up, else the
/// first system python3. Nobody should have to know these paths.
pub fn detect_python() -> Option<std::path::PathBuf> {
    bridge_venv_python().or_else(|| find_python_in(&system_pythons()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_venv_python_is_found_under_a_temp_data_home() {
        let root = unique_root("detect-venv");
        let venv = root.join("data/unofficial-muse-for-linux/account-bridge/.venv/bin");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("python"), "fake").unwrap();
        assert_eq!(
            bridge_venv_python_in(Some(root.join("data")), None),
            Some(venv.join("python"))
        );
        assert_eq!(
            bridge_venv_python_in(None, Some(root.clone())),
            None,
            "no venv seeded under this home"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn python_search_takes_the_first_hit_in_order() {
        let root = unique_root("detect-path");
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("python3"), "fake").unwrap();
        assert_eq!(
            find_python_in(&[first.clone(), second]),
            Some(first.join("python3"))
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn saved_connection_text_round_trips_python_config_dir_and_consent() {
        // Ported contract: the new app reads links the shipped client
        // saved, and vice versa. Non-secret paths only, by construction.
        let text = saved_connection_text("/usr/bin/python3", "/home/test/.config/muse", true);
        let back = saved_connection_from_text(&text).expect("round trip");
        assert_eq!(back.python, PathBuf::from("/usr/bin/python3"));
        assert_eq!(back.config_dir, PathBuf::from("/home/test/.config/muse"));
        assert!(back.consent);
    }

    #[test]
    fn saved_connection_text_rejects_garbage_and_wild_paths() {
        assert!(saved_connection_from_text("not json").is_none());
        assert!(saved_connection_from_text("{}").is_none());
        let unknown = serde_json::json!({
            "python": "/usr/bin/python3",
            "config_dir": "/tmp/x",
            "consent": true,
            "extra": "nope",
        })
        .to_string();
        assert!(saved_connection_from_text(&unknown).is_none());
        for bad in ["", "relative/path", "x\0y", &"x".repeat(4097)] {
            let text = serde_json::json!({
                "python": bad,
                "config_dir": "/tmp/x",
                "consent": false,
            })
            .to_string();
            assert!(saved_connection_from_text(&text).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn save_then_load_round_trips_through_a_private_dir() {
        let root = std::env::temp_dir().join(format!(
            "muse-session-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let connection = SavedConnection {
            python: PathBuf::from("/usr/bin/python3"),
            config_dir: PathBuf::from("/tmp/muse-session"),
            consent: true,
        };
        save_saved_connection_in(&root, &connection).expect("save");
        let back = load_saved_connection_in(&root).expect("load");
        assert_eq!(back, connection);
        std::fs::remove_dir_all(&root).ok();
    }

    fn unique_root(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "muse-session-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn cache_and_temp_session_dirs_count_as_fragile() {
        let roots = fragile_roots();
        // Whatever the environment calls temp, a session under it is fragile.
        let fragile = std::env::temp_dir().join("muse-session");
        assert!(is_fragile_session_dir(&fragile, &roots));
        assert!(!is_fragile_session_dir(
            std::path::Path::new("/home/test/.config/muse"),
            &roots
        ));
    }

    #[test]
    fn session_copy_carries_cookies_and_leaves_the_live_one_alone() {
        let old = unique_root("copy-old");
        let new = unique_root("copy-new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("cookies.txt"), "live-session").unwrap();
        copy_session_dir(&old, &new).expect("copy");
        assert_eq!(
            std::fs::read_to_string(new.join("cookies.txt")).unwrap(),
            "live-session"
        );
        // A destination already holding a live session is not clobbered.
        copy_session_dir(&new, &new).expect("no-op");
        assert_eq!(
            std::fs::read_to_string(new.join("cookies.txt")).unwrap(),
            "live-session"
        );
        std::fs::remove_dir_all(&old).ok();
        std::fs::remove_dir_all(&new).ok();
    }

    fn history_row(id: &str, seq: u64) -> muse_account::HistoryMessage {
        muse_account::HistoryMessage {
            message_id: id.to_owned(),
            seq,
            role: "user".to_owned(),
            text: id.to_owned(),
            timestamp: None,
            status: None,
            options: Vec::new(),
            selected_option: None,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn older_windows_prepend_without_duplicating_held_rows() {
        let mut session = Session::new();
        session.history = vec![history_row("b", 2), history_row("c", 3)];
        session.prepend_history(vec![history_row("a", 1), history_row("b", 2)]);
        let ids: Vec<&str> = session
            .history
            .iter()
            .map(|row| row.message_id.as_str())
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn model_listings_land_only_on_newer_generations() {
        use muse_account::ModelInfo;
        let mut session = Session::new();
        session.note_model(&ModelInfo {
            generation: Some(3),
            selected_model: Some("m3".to_owned()),
            default_model: None,
            available_models: Vec::new(),
            display_name: None,
        });
        assert_eq!(session.selected_model.as_deref(), Some("m3"));
        session.note_model(&ModelInfo {
            generation: Some(2),
            selected_model: Some("m2".to_owned()),
            default_model: None,
            available_models: Vec::new(),
            display_name: None,
        });
        assert_eq!(session.selected_model.as_deref(), Some("m3"));
        session.note_model(&ModelInfo {
            generation: None,
            selected_model: Some("m0".to_owned()),
            default_model: None,
            available_models: Vec::new(),
            display_name: None,
        });
        assert_eq!(session.selected_model.as_deref(), Some("m0"));
    }

    #[test]
    fn history_fingerprint_moves_with_new_rows_only() {
        let mut session = Session::new();
        let empty = session.history_fingerprint();
        session.history = vec![history_row("b", 2), history_row("c", 3)];
        let loaded = session.history_fingerprint();
        assert_ne!(empty, loaded);
        session.prepend_history(vec![history_row("a", 1)]);
        let older = session.history_fingerprint();
        assert_ne!(loaded, older);
        assert_eq!(older.0, 3);
        assert_eq!(older.1.as_deref(), Some("c"));
    }

    #[test]
    fn quota_cache_holds_the_services_own_sentence() {
        let mut session = Session::new();
        assert!(session.latest_quota.is_none());
        session.note_quota("limit reached".to_owned());
        assert_eq!(session.latest_quota.as_deref(), Some("limit reached"));
    }

    #[test]
    fn lax_saved_connection_files_read_as_absent() {
        let root = std::env::temp_dir().join(format!(
            "muse-session-lax-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("account-connection.json");
        std::fs::write(
            &path,
            saved_connection_text("/usr/bin/python3", "/tmp/x", false),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(load_saved_connection_in(&root).is_none());
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn second_press_while_busy_waits_and_stale_tickets_release_nothing() {
        let mut session = Session::new();
        let first = session.try_begin(Op::History).expect("slot");
        assert!(session.try_begin(Op::History).is_none());
        assert!(session.try_begin(Op::Connect).is_none());
        // A foreign ticket releases nothing...
        let foreign = Ticket {
            op: Op::History,
            serial: first.serial.wrapping_add(1),
        };
        assert!(!session.accept(&foreign));
        assert!(session.try_begin(Op::History).is_none());
        // ...while the real completion frees the slot exactly once.
        assert!(session.accept(&first));
        assert!(!session.accept(&first));
        assert!(session.try_begin(Op::Connect).is_some());
    }
}
