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
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
