//! Session-scoped, explicitly enabled local host for the linked account.
//!
//! Ported from the retired client's controller: the lifecycle,
//! consent gates and user-facing copy are unchanged, only the UI
//! layer (next slice) is new. Nothing here touches GTK: the page
//! owns a `Controller` and renders snapshots from it.
//!
//! The host registers this machine with the service (one persisted
//! node id per installation), advertises the broker's file-command
//! manifest, and pumps invocations: reads and search answer
//! immediately, mutations become approval cards the person presses,
//! and every reply rides back over the same link. Stop revokes
//! everything: transport first, then file capabilities.

use muse_account::BridgeConfig;
use muse_local_link::{HostConfig, HostEvent, HostSession};
use muse_local_tools::{ApprovalRequest, Decision, Invocation, ToolBroker, ToolReply};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// One pending approval card: the broker's request plus the folder
/// context the card names. The destination folder rides along for a
/// cross-folder move; it is `None` for every other card.
#[derive(Clone)]
pub(crate) struct Card {
    pub request: ApprovalRequest,
    pub deadline: Instant,
    pub deciding: bool,
}

enum Completed {
    Grant(Result<(), &'static str>),
    /// A page's filename search: the whole merged answer, already worded.
    Search(FileSearch),
    /// A page's content search: the merged matching lines.
    Content(ContentSearch),
    // Boxed: an approval card carries a full proposal, and the grant variant must
    // not carry that weight through every channel send.
    Operation(Box<Decision>, Instant, String),
}

/// Session activity log capacity: oldest entries are dropped first.
const MAX_LOG: usize = 64;
/// UTC clock label for log entries (HH:MM:SS); std-only, no new deps.
fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02} UTC",
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60
    )
}
/// One device identity per installation, not one per start. A fresh random id
/// on every start registers a *new* device with the account, so the agent can
/// never attribute work to this machine and a pairing never survives a restart.
const NODE_ID_FILE: &str = "local-host-node-id";
/// Registered device label. The identity is stable, so the label must not
/// imply a per-session device.
const DEVICE_LABEL: &str = "Linux";
/// Local-host copy. The identity is one persisted id per installation; this
/// text previously claimed a random per-session device and a screenshot showed
/// it contradicting the registration notice, so it is named and guarded.
const IDENTITY_COPY: &str = "The device identity is created once per installation, stored owner-only beside your account config, and reused after restart, so Muse attributes chats to this same machine. Chats are attributed to this Linux host only while it is registered. Stop, account disconnect/error/change, or window close revokes all grants and approvals. No automatic restart or persistent broad grants; existing Muse settings are never changed.";
/// Shown after the page's own "Registered with Muse" prefix, so it must not
/// repeat that phrase; doing so rendered "Registered with Muse · Registered
/// with Muse. …" on screen.
const REGISTERED_NOTICE: &str =
    "This device keeps one identity across restarts. Stop revokes folders and pending approvals.";

pub(crate) fn identity_copy() -> &'static str {
    IDENTITY_COPY
}

fn node_id_is_valid(value: &str) -> bool {
    value.len() == 38
        && value.starts_with("linux-")
        && value[6..]
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
/// Saved "allow writes here without a card" decisions, beside the device
/// identity. Grant ids are minted per session, so the choice is stored by
/// the folder's own path and re-applied to a later grant of that folder.
const POLICY_FILE: &str = "local-host-policy.json";
const POLICY_SAVE_FAILED: &str = "Could not save the folder permission.";

fn policy_path(directory: &std::path::Path) -> std::path::PathBuf {
    directory.join(POLICY_FILE)
}

/// A missing or unreadable file means no saved permissions, never a guess:
/// the person can only gain a permission by setting it again.
fn load_folder_policies(directory: &std::path::Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(policy_path(directory)) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<String>>(&text)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| !entry.is_empty() && !entry.contains('\0'))
        .map(PathBuf::from)
        .collect()
}

/// Save the set atomically, owner-only, so an interrupted write cannot leave a
/// half permission that a later start would read as something else.
fn save_folder_policies(
    directory: &std::path::Path,
    roots: &[PathBuf],
) -> Result<(), &'static str> {
    let entries: Vec<String> = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    let body = serde_json::to_vec(&entries).map_err(|_| POLICY_SAVE_FAILED)?;
    let staged = directory.join(format!(".{POLICY_FILE}.{}", std::process::id()));
    let written = {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&staged)
            .and_then(|mut file| file.write_all(&body))
    };
    if written.is_err() || std::fs::rename(&staged, policy_path(directory)).is_err() {
        let _ = std::fs::remove_file(&staged);
        return Err(POLICY_SAVE_FAILED);
    }
    Ok(())
}

fn random_node_id() -> Option<String> {
    let mut nonce = [0u8; 16];
    // SAFETY: the writable buffer is valid for its supplied length.
    if unsafe { libc::getrandom(nonce.as_mut_ptr().cast(), nonce.len(), 0) } != nonce.len() as isize
    {
        return None;
    }
    Some(format!(
        "linux-{}",
        nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}
/// Reads the installation's node id, minting and persisting one on first use.
/// A corrupt leftover is replaced rather than handed to the service as an id.
fn persisted_node_id(directory: &std::path::Path) -> Result<String, &'static str> {
    let path = directory.join(NODE_ID_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim();
        if node_id_is_valid(existing) {
            return Ok(existing.to_owned());
        }
    }
    let node_id = random_node_id().ok_or("Could not generate a secure device identity.")?;
    // Stage then rename: an interrupted start must not leave a truncated id that
    // a later start would treat as corrupt and silently replace with a new device.
    let staged = directory.join(format!(".{NODE_ID_FILE}.{}", std::process::id()));
    let written = {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&staged)
            .and_then(|mut file| file.write_all(node_id.as_bytes()))
    };
    if written.is_err() || std::fs::rename(&staged, &path).is_err() {
        let _ = std::fs::remove_file(&staged);
        return Err("Could not save the local host identity.");
    }
    Ok(node_id)
}
/// Remote-controlled command names are allow-listed so a hostile command
/// string (paths, content) can never reach the log.
fn safe_command(raw: &str) -> &str {
    match raw {
        "files.search"
        | "files.search_content"
        | "files.read_text"
        | "files.create_text"
        | "files.edit"
        | "files.rename"
        | "files.delete"
        | "computer.probe" => raw,
        _ => "unknown-command",
    }
}
/// Broker outcome codes are fixed strings; still filtered so only
/// code-shaped text is ever logged (never content or paths).
fn safe_code(raw: &str) -> String {
    let code: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(32)
        .collect();
    if code.is_empty() { "err".into() } else { code }
}
/// Relative paths are already broker-validated (strict, no leading slash,
/// no `.`/`..`); truncated and de-slashed defensively.
fn safe_path(raw: &str) -> String {
    let path: String = raw.trim_start_matches('/').chars().take(256).collect();
    if path.is_empty() {
        "(invalid-path)".into()
    } else {
        path
    }
}

struct Completion {
    epoch: u64,
    value: Completed,
}
const MAX_WORK: usize = 8;
struct WorkGuard(Arc<AtomicUsize>);
impl Drop for WorkGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
pub(crate) struct DeviceLease {
    pub node_id: String,
    pub active: Arc<AtomicBool>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Disconnected,
    Connecting,
    Registered,
}

/// One granted folder, for the page's grant list. The broker mints the
/// id per session; the label is display text, never logged raw.
#[derive(Clone)]
pub(crate) struct GrantedFolder {
    pub label: String,
    pub root: PathBuf,
    pub writable: bool,
}

/// One filename hit: the folder it was found in (the page's label for the
/// grant, never its capability id) and its path inside that folder.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct FileHit {
    pub folder: String,
    pub path: String,
}

/// A folder the search could not walk, by label and the broker's own code.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct FileRefusal {
    pub folder: String,
    pub code: String,
}

/// The last filename search the page ran. The query travels with the hits, so
/// a surface shows what was searched, not just what came back.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct FileSearch {
    pub query: String,
    pub hits: Vec<FileHit>,
    pub refused: Vec<FileRefusal>,
    pub scanned: u64,
    /// The walk stopped at its own bound, so more names may match.
    pub truncated: bool,
}

/// One matching line from a content search: the path inside its granted
/// folder, the 1-based line number and the line's own text (the broker's own
/// 200-character excerpt).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ContentHit {
    pub folder: String,
    pub path: String,
    pub line: u64,
    pub text: String,
}

/// The last content search the page ran: the text it looked for, the matching
/// lines, and the same per-folder refusals a filename search reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ContentSearch {
    pub query: String,
    pub hits: Vec<ContentHit>,
    pub refused: Vec<FileRefusal>,
    /// Names the broker looked at across every folder it walked.
    pub scanned: u64,
    /// A bound was reached, so more lines may match.
    pub truncated: bool,
}

/// A file's text as a row's preview shows it: the whole file the broker
/// answered, because it refuses one over its 64 KiB read rather than cutting it.
pub(crate) struct FilePreview {
    pub folder: String,
    pub path: String,
    pub content: String,
}

/// How many hits one search may carry across all its folders.
const MAX_SEARCH_HITS: usize = 200;
/// How many matching lines one content search may carry across all its
/// folders: the broker caps 50 per folder, and a page that walks eight of them
/// must still answer in one list.
const MAX_CONTENT_HITS: usize = 200;
/// A content search reads whole files, so it gets more wall clock than a walk
/// over names; the broker's own deadline still ends it.
const CONTENT_TIMEOUT_MS: u64 = 15_000;
/// One folder's walk is bounded by the broker's own 10,000-entry cap; this is
/// the wall clock the page allows it before it answers `expired`.
const SEARCH_TIMEOUT_MS: u64 = 5_000;
/// The preview's own read bound, matching the broker's `files.read_text`.
const PREVIEW_TIMEOUT_MS: u64 = 5_000;

/// The page's read model: everything render() needs, nothing it can
/// mutate. Cards carry their approval ids; the press handlers call
/// back into the controller by id.
pub(crate) struct HostSnapshot {
    pub phase: Phase,
    pub notice: String,
    pub grants: Vec<GrantedFolder>,
    pub cards: Vec<Card>,
    pub log: Vec<String>,
    pub policies: Vec<PathBuf>,
    pub node_id: Option<String>,
    /// The last filename search the page ran, and whether one is running:
    /// both surfaces read the search from here, never from a second copy.
    pub search: Option<FileSearch>,
    pub searching: bool,
    /// The last content search (matching lines inside the granted folders),
    /// and whether one is running.
    pub content: Option<ContentSearch>,
    pub searching_content: bool,
}

pub(crate) struct Controller {
    epoch: u64,
    phase: Phase,
    broker: ToolBroker,
    session: Option<HostSession>,
    identity: Option<DeviceLease>,
    cards: Vec<Card>,
    replies: VecDeque<ToolReply>,
    log: VecDeque<String>,
    cmds: HashMap<String, String>,
    workers: Arc<AtomicUsize>,
    tx: mpsc::SyncSender<Completion>,
    rx: mpsc::Receiver<Completion>,
    notice: String,
    /// The account config folder, known once the host has started: where the
    /// saved folder permissions live.
    config_dir: Option<PathBuf>,
    /// Folder permissions saved on disk. Kept beside the broker's copy because
    /// Stop replaces the broker, and the switch must still show what is saved.
    policies: Vec<PathBuf>,
    /// The page's filename search: its last answer and whether one is running.
    search: Option<FileSearch>,
    searching: bool,
    /// The page's content search: its last answer and whether one is running.
    content: Option<ContentSearch>,
    searching_content: bool,
    /// Invocation ids for the page's own reads: the broker refuses a repeated
    /// id, so every press mints a new one.
    page_seq: u64,
}
impl Controller {
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(16);
        Self {
            epoch: 0,
            phase: Phase::Disconnected,
            broker: ToolBroker::new(),
            session: None,
            identity: None,
            cards: Vec::new(),
            replies: VecDeque::new(),
            log: VecDeque::new(),
            cmds: HashMap::new(),
            workers: Arc::new(AtomicUsize::new(0)),
            tx,
            rx,
            notice: "Disconnected. No folders shared with Muse.".into(),
            config_dir: None,
            policies: Vec::new(),
            search: None,
            searching: false,
            content: None,
            searching_content: false,
            page_seq: 0,
        }
    }
    /// The page's read model. Render reads this; presses go through the
    /// controller methods below so every mutation keeps the invariants.
    pub(crate) fn snapshot(&self) -> HostSnapshot {
        HostSnapshot {
            phase: self.phase,
            notice: self.notice.clone(),
            grants: self
                .broker
                .grants()
                .into_iter()
                .map(|g| GrantedFolder {
                    label: g.label,
                    root: g.root,
                    writable: g.writable,
                })
                .collect(),
            cards: self.cards.clone(),
            log: self.log.iter().cloned().collect(),
            policies: self.policies.clone(),
            node_id: self.identity.as_ref().map(|lease| lease.node_id.clone()),
            // A search is only about folders granted now: a stopped host has
            // none, and its old answer must not outlive them.
            search: self
                .search
                .clone()
                .filter(|_| !self.broker.grants().is_empty()),
            searching: self.searching,
            content: self
                .content
                .clone()
                .filter(|_| !self.broker.grants().is_empty()),
            searching_content: self.searching_content,
        }
    }
    fn push_log(&mut self, entry: String) {
        if self.log.len() >= MAX_LOG {
            self.log.pop_front();
        }
        self.log.push_back(entry);
    }
    pub(crate) fn clear_log(&mut self) {
        // Display only: cards, replies, grants, epoch are untouched.
        self.log.clear();
    }
    fn grant_label(&self, grant_id: &str) -> String {
        // Labels are broker-minted ("Folder N"); the raw grant id (a
        // capability token) is never logged.
        self.broker
            .grants()
            .into_iter()
            .find(|g| g.id == grant_id)
            .map(|g| g.label)
            .unwrap_or_else(|| "ungranted".into())
    }
    fn outcome(reply: &ToolReply) -> String {
        if reply.ok {
            "ok".into()
        } else {
            format!("err {}", safe_code(reply.error.as_deref().unwrap_or("err")))
        }
    }
    fn logged_command(&self, invocation_id: &str) -> String {
        self.cmds
            .get(invocation_id)
            .map(|c| safe_command(c).to_owned())
            .unwrap_or_else(|| "unknown-command".into())
    }
    pub(crate) fn start(
        &mut self,
        config: Option<BridgeConfig>,
        consent: bool,
    ) -> Result<(), &'static str> {
        if !consent
            || self.phase != Phase::Disconnected
            || self.workers.load(Ordering::SeqCst) != 0
            || self.broker.grants().is_empty()
        {
            return Err("Connect the account, consent and grant a folder before starting.");
        }
        let config = config.ok_or("Connect your Muse account first.")?;
        // Saved permissions belong to the folders, not to a session's grant
        // ids, so they are re-applied to whatever this session grants.
        self.policies = load_folder_policies(&config.config_dir);
        for root in &self.policies {
            self.broker.set_folder_policy(root, true);
        }
        self.config_dir = Some(config.config_dir.clone());
        let node_id = persisted_node_id(&config.config_dir)?;
        let script = config.script.with_file_name("muse_local_host.py");
        let session = HostSession::spawn(HostConfig {
            python: config.python,
            script,
            config_dir: config.config_dir,
            node_id: node_id.clone(),
            display_name: DEVICE_LABEL.into(),
            commands: self.broker.manifest()["commands_v2"].clone(),
        })
        .map_err(|_| "Host configuration rejected.")?;
        self.epoch = self.epoch.wrapping_add(1);
        self.identity = Some(DeviceLease {
            node_id,
            active: Arc::new(AtomicBool::new(false)),
        });
        self.session = Some(session);
        self.phase = Phase::Connecting;
        self.notice = "Waiting for the actual registration acknowledgement; not paired yet.".into();
        Ok(())
    }
    fn work(&self, work: impl FnOnce() -> Completed + Send + 'static) -> Result<(), &'static str> {
        self.workers
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < MAX_WORK).then_some(n + 1)
            })
            .map_err(|_| "Local host is busy; no additional work was queued.")?;
        let guard = WorkGuard(Arc::clone(&self.workers));
        let tx = self.tx.clone();
        let epoch = self.epoch;
        std::thread::spawn(move || {
            let value = work();
            let _ = tx.try_send(Completion { epoch, value });
            drop(guard);
        });
        Ok(())
    }
    pub(crate) fn grant(
        &self,
        path: PathBuf,
        consent: bool,
        writable: bool,
    ) -> Result<(), &'static str> {
        if !consent || self.phase != Phase::Disconnected || self.workers.load(Ordering::SeqCst) != 0
        {
            return Err("Stop the host and explicitly consent before granting a folder.");
        }
        if self.broker.grants().len() >= 8 {
            return Err("At most eight folders may be granted.");
        }
        let broker = self.broker.clone();
        self.work(move || {
            Completed::Grant(
                (if writable {
                    broker.grant(&path)
                } else {
                    broker.grant_read_only(&path)
                })
                .map(|_| ())
                .map_err(|_| "Folder access was denied."),
            )
        })
    }
    /// The page's filename search: one bounded walk per granted folder, on a
    /// worker, so a large folder never freezes the window. The answer lands in
    /// `poll` and both surfaces read it from the snapshot, never from a second
    /// copy.
    pub(crate) fn search(&mut self, query: &str) -> Result<(), &'static str> {
        let query = query.trim();
        if query.is_empty() {
            return Err("Type part of a filename to search for.");
        }
        if query.len() > 256 {
            return Err("A filename search is at most 256 characters.");
        }
        if query.contains(['/', '\0']) {
            return Err("A filename search is one name fragment, not a path.");
        }
        let grants: Vec<(String, String)> = self
            .broker
            .grants()
            .into_iter()
            .map(|grant| (grant.id, grant.label))
            .collect();
        if grants.is_empty() {
            return Err("Grant a folder before searching it.");
        }
        self.page_seq = self.page_seq.wrapping_add(1);
        let seq = self.page_seq;
        let query = query.to_owned();
        // The page shows this query as running from the press, not from the
        // answer: a walk may take seconds and the entry must not look inert.
        self.search = Some(FileSearch {
            query: query.clone(),
            ..FileSearch::default()
        });
        self.searching = true;
        let broker = self.broker.clone();
        if let Err(busy) =
            self.work(move || Completed::Search(search_files(&broker, &grants, &query, seq)))
        {
            self.searching = false;
            return Err(busy);
        }
        Ok(())
    }

    /// The page's content search: the broker's own matching lines per granted
    /// folder, merged on a worker so a folder of code never freezes the
    /// window. The broker reads only UTF-8 files and caps files, matches and
    /// line length; the page reports its answer, not a second implementation.
    pub(crate) fn search_content(&mut self, query: &str) -> Result<(), &'static str> {
        let query = query.trim();
        if query.is_empty() {
            return Err("Type text to look for inside your files.");
        }
        if query.len() > 256 {
            return Err("A content search is at most 256 characters.");
        }
        if query.contains('\0') {
            return Err("A content search cannot carry a NUL byte.");
        }
        let grants: Vec<(String, String)> = self
            .broker
            .grants()
            .into_iter()
            .map(|grant| (grant.id, grant.label))
            .collect();
        if grants.is_empty() {
            return Err("Grant a folder before searching inside it.");
        }
        self.page_seq = self.page_seq.wrapping_add(1);
        let seq = self.page_seq;
        let query = query.to_owned();
        self.content = Some(ContentSearch {
            query: query.clone(),
            ..ContentSearch::default()
        });
        self.searching_content = true;
        let broker = self.broker.clone();
        if let Err(busy) =
            self.work(move || Completed::Content(search_contents(&broker, &grants, &query, seq)))
        {
            self.searching_content = false;
            return Err(busy);
        }
        Ok(())
    }

    /// One row's preview: the broker's own bounded text read. Run here rather
    /// than on a worker because it reads at most 64 KiB and refuses more.
    pub(crate) fn read_file(&mut self, folder: &str, path: &str) -> Result<FilePreview, String> {
        let Some(grant) = self
            .broker
            .grants()
            .into_iter()
            .find(|grant| grant.label == folder)
        else {
            return Err("That folder is no longer granted. Search again.".to_owned());
        };
        self.page_seq = self.page_seq.wrapping_add(1);
        let reply = match self.broker.invoke(Invocation {
            id: format!("page-read-{}", self.page_seq),
            command: "files.read_text".to_owned(),
            params: serde_json::json!({"grant_id": grant.id, "path": path}),
            timeout_ms: PREVIEW_TIMEOUT_MS,
        }) {
            Decision::Reply(reply) => reply,
            Decision::Approval(_) => {
                return Err("A read asked for an approval, so nothing was opened.".to_owned());
            }
        };
        if !reply.ok {
            return Err(read_refusal(reply.error.as_deref().unwrap_or("err")).to_owned());
        }
        let content = reply
            .output
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        Ok(FilePreview {
            folder: folder.to_owned(),
            path: path.to_owned(),
            content,
        })
    }

    fn invoke(&mut self, invocation: muse_local_link::HostInvocation) {
        if self.phase != Phase::Registered {
            self.stop();
            return;
        }
        // The folder label is log-only: the raw grant id (a
        // capability token) never travels past this line.
        let label = self.grant_label(
            invocation
                .params
                .get("grant_id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
        let deadline = Instant::now() + Duration::from_millis(invocation.timeout_ms.min(120000));
        let broker = self.broker.clone();
        let command = invocation.command.clone();
        self.push_log(format!(
            "{} received {} · {}",
            now_hms(),
            safe_command(&command),
            label
        ));
        if self
            .work(move || {
                let decision = run_invocation(&broker, invocation, deadline);
                Completed::Operation(Box::new(decision), deadline, command)
            })
            .is_err()
        {
            self.stop();
            self.notice = "Host stopped: local worker capacity reached.".into();
        }
    }
    /// Turn the saved write permission on (for every writable folder granted
    /// now) or off. Durable first, then enforced: a save that fails leaves the
    /// broker exactly as it was.
    pub(crate) fn set_write_policy(&mut self, allow: bool) -> Result<(), &'static str> {
        let directory = self
            .config_dir
            .clone()
            .ok_or("Start the local host once before saving this permission.")?;
        let roots = if allow {
            let mut roots = self.policies.clone();
            for root in self
                .broker
                .grants()
                .iter()
                .filter(|grant| grant.writable)
                .map(|grant| grant.root.clone())
            {
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
            if roots.is_empty() {
                return Err("Grant a read-write folder before saving this permission.");
            }
            roots
        } else {
            Vec::new()
        };
        save_folder_policies(&directory, &roots)?;
        self.policies = roots.clone();
        for root in &roots {
            self.broker.set_folder_policy(root, true);
        }
        Ok(())
    }
    pub(crate) fn decide(&mut self, id: &str, allow: bool) -> Result<(), &'static str> {
        self.poll(); // Closure/expiry wins over a click on an old rendered card.
        if self.phase != Phase::Registered {
            return Err("Host is disconnected.");
        }
        let index = self
            .cards
            .iter()
            .position(|c| c.request.approval_id == id && !c.deciding && Instant::now() < c.deadline)
            .ok_or("Approval expired or revoked.")?;
        let broker = self.broker.clone();
        let id = id.to_owned();
        let path = safe_path(&self.cards[index].request.path);
        self.push_log(format!(
            "{} approval {} · {}",
            now_hms(),
            if allow { "allowed once" } else { "denied" },
            path
        ));
        self.work(move || {
            Completed::Operation(
                Box::new(Decision::Reply(if allow {
                    broker.approve(&id)
                } else {
                    broker.deny(&id)
                })),
                Instant::now(),
                String::new(),
            )
        })?;
        self.cards[index].deciding = true;
        Ok(())
    }
    /// Each terminal code names a different problem and a different next step;
    /// one sentence for every closure sent the user to the wrong fix.
    fn closed_notice(code: &str) -> String {
        let text = match code {
            "auth_required" => {
                "Your Muse session expired during the local-host session. Sign in again, then grant folders and start the host."
            }
            "link_forbidden" => {
                "The service refused device linking for this account, so this Linux machine was not registered. Nothing was granted."
            }
            "link_rejected" => {
                "The service rejected the host registration. Nothing was paired; the account and folder grants are unchanged."
            }
            "transport_timeout" => {
                "The host connection timed out before registration. Check the connection, then start the host again."
            }
            "stopped" => "Local host stopped. Folder grants for this session ended.",
            "" => {
                "Host connection closed. Review your account, then grant folders and start again."
            }
            other => {
                return format!(
                    "Host connection closed ({other}). Review your account, then grant folders and start again."
                );
            }
        };
        text.to_string()
    }

    /// Drain transport events, worker completions and queued replies.
    /// Bounded drains keep the UI responsive even under a hostile
    /// event stream. The page calls this on a tick while it shows.
    pub(crate) fn poll(&mut self) {
        // Bounded drains keep GTK responsive even under a hostile event stream.
        for _ in 0..16 {
            let event = self.session.as_ref().map(|s| s.try_recv());
            match event {
                Some(Ok(HostEvent::Registered)) => self.registered(self.epoch),
                Some(Ok(HostEvent::Connecting)) => {}
                Some(Ok(HostEvent::Invoke(invocation))) => self.invoke(invocation),
                Some(Ok(HostEvent::Closed { code })) => {
                    let message = Self::closed_notice(&code);
                    self.stop();
                    // The code is the only explanation of a closure, so keep it
                    // in the activity log after `stop` has reset the session.
                    self.push_log(format!("{} host closed · {code}", now_hms()));
                    self.notice = message;
                    break;
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.stop();
                    self.notice =
                        "The host worker stopped without reporting a reason. Review your account, then grant folders and start again."
                            .into();
                    break;
                }
                _ => break,
            }
        }
        for _ in 0..16 {
            let Ok(completed) = self.rx.try_recv() else {
                break;
            };
            if completed.epoch != self.epoch {
                continue;
            }
            match completed.value {
                Completed::Grant(result) => {
                    self.notice = match result {
                        Ok(()) => "Folder granted for this local-host session only.".into(),
                        Err(e) => e.into(),
                    }
                }
                Completed::Content(result) => {
                    self.searching_content = false;
                    self.push_log(format!(
                        "{} content search · {} line(s) in {} name(s) scanned{}",
                        now_hms(),
                        result.hits.len(),
                        result.scanned,
                        if result.truncated {
                            " (cut at the bound)"
                        } else {
                            ""
                        }
                    ));
                    if !result.refused.is_empty() {
                        self.push_log(format!(
                            "{} content search · {} folder(s) refused",
                            now_hms(),
                            result.refused.len()
                        ));
                    }
                    self.content = Some(result);
                }
                Completed::Search(result) => {
                    self.searching = false;
                    // Counts, never paths: the log is the same surface a
                    // service-shaped command writes to.
                    self.push_log(format!(
                        "{} filename search · {} hit(s), {} name(s) scanned{}",
                        now_hms(),
                        result.hits.len(),
                        result.scanned,
                        if result.truncated {
                            " (cut at the bound)"
                        } else {
                            ""
                        }
                    ));
                    if !result.refused.is_empty() {
                        self.push_log(format!(
                            "{} filename search · {} folder(s) refused",
                            now_hms(),
                            result.refused.len()
                        ));
                    }
                    self.search = Some(result);
                }
                Completed::Operation(decision, deadline, command)
                    if self.phase == Phase::Registered =>
                {
                    match *decision {
                        Decision::Reply(reply) => {
                            if !command.is_empty() {
                                self.cmds.entry(reply.id.clone()).or_insert(command);
                            }
                            let what = self.logged_command(&reply.id);
                            let outcome = Self::outcome(&reply);
                            self.push_log(format!(
                                "{} auto-replied {} · {}",
                                now_hms(),
                                what,
                                outcome
                            ));
                            self.cards.retain(|c| c.request.invocation_id != reply.id);
                            self.replies.push_back(reply);
                        }
                        Decision::Approval(request) => {
                            if Instant::now() < deadline {
                                if !command.is_empty() {
                                    self.cmds
                                        .entry(request.invocation_id.clone())
                                        .or_insert(command);
                                }
                                let what = self.logged_command(&request.invocation_id);
                                self.push_log(format!(
                                    "{} approval requested {} · {}",
                                    now_hms(),
                                    what,
                                    safe_path(&request.path)
                                ));
                                self.cards.push(Card {
                                    request,
                                    deadline,
                                    deciding: false,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if self.phase == Phase::Registered {
            for reply in self.broker.expire() {
                if let Some(card) = self
                    .cards
                    .iter()
                    .find(|c| c.request.invocation_id == reply.id)
                {
                    self.push_log(format!(
                        "{} approval expired {} · {}",
                        now_hms(),
                        self.logged_command(&reply.id),
                        safe_path(&card.request.path)
                    ));
                }
                self.cards.retain(|c| c.request.invocation_id != reply.id);
                self.replies.push_back(reply);
            }
            if self.replies.len() > 16 {
                self.stop();
                return;
            }
            for _ in 0..16 {
                let Some(reply) = self.replies.front() else {
                    break;
                };
                let Some(session) = self.session.as_ref() else {
                    break;
                };
                let rid = reply.id.clone();
                let what = self.logged_command(&rid);
                let outcome =
                    if reply.output.get("allowed_by").and_then(|v| v.as_str()) == Some("policy") {
                        "ok · allowed by saved permission".to_string()
                    } else {
                        Self::outcome(reply)
                    };
                match session.send_result(
                    &reply.id,
                    reply.ok,
                    reply.output.clone(),
                    reply.error.clone(),
                ) {
                    Ok(()) => {
                        self.replies.pop_front();
                        self.cmds.remove(&rid);
                        self.push_log(format!("{} reply sent {} · {}", now_hms(), what, outcome));
                    }
                    Err(e) if matches!(e.code, "busy" | "queue_full") => break,
                    Err(e) if e.code == "request_too_large" => {
                        let reply = self.replies.front_mut().unwrap();
                        reply.ok = false;
                        reply.output = serde_json::Value::Null;
                        reply.error = Some("result_too_large".into());
                    }
                    Err(e) => {
                        self.replies.pop_front();
                        self.cmds.remove(&rid);
                        self.push_log(format!(
                            "{} reply dropped {} · {}",
                            now_hms(),
                            what,
                            safe_code(e.code)
                        ));
                    }
                }
            }
        }
    }
    fn registered(&mut self, epoch: u64) {
        if epoch == self.epoch && self.phase == Phase::Connecting {
            self.phase = Phase::Registered;
            self.notice = REGISTERED_NOTICE.into();
            if let Some(identity) = &self.identity {
                identity.active.store(true, Ordering::SeqCst);
            }
        }
    }
    pub(crate) fn stop(&mut self) {
        // Cancellation boundary: stop transport BEFORE revoking file capabilities.
        if let Some(session) = self.session.take() {
            session.stop();
        }
        if let Some(identity) = self.identity.take() {
            identity.active.store(false, Ordering::SeqCst);
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.phase = Phase::Disconnected;
        let revoked = self.cards.len();
        self.cards.clear();
        self.replies.clear();
        self.cmds.clear();
        // Session-only: prior entries reset with the cards/replies; only the
        // revoke/stop boundary marker survives, capped like the rest.
        self.log.clear();
        self.push_log(format!(
            "{} stopped · grants revoked · {} pending approval(s) revoked",
            now_hms(),
            revoked
        ));
        self.notice =
            "Disconnected. Grants revoked; new consent, folders and Start are required.".into();
        self.broker.revoke_all();
        self.broker = ToolBroker::new();
        // The searches were about those folders, so they go with them.
        self.search = None;
        self.searching = false;
        self.content = None;
        self.searching_content = false;
    }
}

/// Every granted folder's own bounded walk, merged into one answer. A folder
/// the broker refuses is named with its code instead of failing the search:
/// one unreadable folder must not hide the hits another one has.
fn search_files(
    broker: &ToolBroker,
    grants: &[(String, String)],
    query: &str,
    seq: u64,
) -> FileSearch {
    let mut out = FileSearch {
        query: query.to_owned(),
        ..FileSearch::default()
    };
    'folders: for (grant_id, label) in grants {
        let reply = match broker.invoke(Invocation {
            id: format!("page-search-{seq}-{label}"),
            command: "files.search".to_owned(),
            params: serde_json::json!({"grant_id": grant_id, "query": query}),
            timeout_ms: SEARCH_TIMEOUT_MS,
        }) {
            Decision::Reply(reply) => reply,
            Decision::Approval(_) => {
                out.refused.push(FileRefusal {
                    folder: label.clone(),
                    code: "approval".to_owned(),
                });
                continue;
            }
        };
        if !reply.ok {
            out.refused.push(FileRefusal {
                folder: label.clone(),
                code: reply.error.unwrap_or_else(|| "err".to_owned()),
            });
            continue;
        }
        out.scanned += reply
            .output
            .get("scanned")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        out.truncated |= reply
            .output
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        for path in reply
            .output
            .get("paths")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(path) = path.as_str() else {
                continue;
            };
            if out.hits.len() == MAX_SEARCH_HITS {
                out.truncated = true;
                break 'folders;
            }
            out.hits.push(FileHit {
                folder: label.clone(),
                path: path.to_owned(),
            });
        }
    }
    out.hits
        .sort_by(|a, b| a.folder.cmp(&b.folder).then_with(|| a.path.cmp(&b.path)));
    out
}

/// Every granted folder's own matching lines, merged into one answer. The
/// broker does the reading (UTF-8 only, 200 files and 50 matches per folder,
/// 200-character excerpts); this only merges and sorts, and a folder it
/// refused is named with its code instead of hiding another folder's hits.
fn search_contents(
    broker: &ToolBroker,
    grants: &[(String, String)],
    query: &str,
    seq: u64,
) -> ContentSearch {
    let mut out = ContentSearch {
        query: query.to_owned(),
        ..ContentSearch::default()
    };
    'folders: for (grant_id, label) in grants {
        let reply = match broker.invoke(Invocation {
            id: format!("page-content-{seq}-{label}"),
            command: "files.search_content".to_owned(),
            params: serde_json::json!({"grant_id": grant_id, "query": query}),
            timeout_ms: CONTENT_TIMEOUT_MS,
        }) {
            Decision::Reply(reply) => reply,
            Decision::Approval(_) => {
                out.refused.push(FileRefusal {
                    folder: label.clone(),
                    code: "approval".to_owned(),
                });
                continue;
            }
        };
        if !reply.ok {
            out.refused.push(FileRefusal {
                folder: label.clone(),
                code: reply.error.unwrap_or_else(|| "err".to_owned()),
            });
            continue;
        }
        out.scanned += reply
            .output
            .get("scanned")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        out.truncated |= reply
            .output
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        for hit in reply
            .output
            .get("matches")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(path), Some(line), Some(text)) = (
                hit.get("path").and_then(serde_json::Value::as_str),
                hit.get("line").and_then(serde_json::Value::as_u64),
                hit.get("text").and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            if out.hits.len() == MAX_CONTENT_HITS {
                out.truncated = true;
                break 'folders;
            }
            out.hits.push(ContentHit {
                folder: label.clone(),
                path: path.to_owned(),
                line,
                text: text.to_owned(),
            });
        }
    }
    out.hits.sort_by(|a, b| {
        a.folder
            .cmp(&b.folder)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    out
}

/// What a refused read means, in the one sentence a row can act on.
fn read_refusal(code: &str) -> &'static str {
    match code {
        "invalid_utf8" => "That file is not UTF-8 text, so it has no preview here.",
        "too_large" => "That file is larger than the 64 KiB a preview reads.",
        "special_file" => "That path is not a regular file.",
        "not_granted" | "revoked" => "That folder is no longer granted. Search again.",
        "expired" => "The read took too long and was abandoned.",
        _ => "That file could not be read.",
    }
}
fn run_invocation(
    broker: &ToolBroker,
    invocation: muse_local_link::HostInvocation,
    deadline: Instant,
) -> Decision {
    // Scheduling must consume, never restart, the remote invocation's budget.
    let timeout_ms = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(120000) as u64;
    broker.invoke(Invocation {
        id: invocation.id,
        command: invocation.command,
        params: invocation.params,
        timeout_ms,
    })
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    /// A search is one name fragment: an empty, overlong or path-shaped query
    /// is refused before any walk, and with no folder granted the page says
    /// what to do first rather than answering "no matches".
    #[test]
    fn a_search_query_is_one_name_fragment() {
        let mut host = Controller::new();
        assert_eq!(
            host.search(""),
            Err("Type part of a filename to search for.")
        );
        assert_eq!(
            host.search("   "),
            Err("Type part of a filename to search for.")
        );
        assert_eq!(
            host.search("dir/notes"),
            Err("A filename search is one name fragment, not a path.")
        );
        assert_eq!(
            host.search(&"x".repeat(257)),
            Err("A filename search is at most 256 characters.")
        );
        assert_eq!(
            host.search("notes"),
            Err("Grant a folder before searching it.")
        );
        // A row whose folder is gone is a refusal with the fix named, not a
        // silent empty preview.
        assert_eq!(
            host.read_file("Folder 1", "notes.txt").err().as_deref(),
            Some("That folder is no longer granted. Search again.")
        );
    }

    /// The walk is the broker's own: names are matched recursively inside the
    /// granted folder, one folder's hits merge with another's, and a query
    /// nothing matches is an empty answer rather than a refusal.
    #[test]
    fn a_search_merges_every_granted_folder() {
        let dir = Fixture::dir();
        std::fs::create_dir_all(dir.join("sub")).expect("a scratch subfolder");
        std::fs::write(dir.join("alpha-report.md"), "a").expect("a scratch file");
        std::fs::write(dir.join("sub/beta-report.md"), "b").expect("a nested scratch file");
        std::fs::write(dir.join("notes.txt"), "c").expect("a scratch file");
        let broker = ToolBroker::new();
        let grant = broker.grant_read_only(&dir).expect("a granted folder");
        let grants = vec![(grant.id.clone(), grant.label.clone())];

        let found = search_files(&broker, &grants, "report", 1);
        let paths: Vec<&str> = found.hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(paths, vec!["alpha-report.md", "sub/beta-report.md"]);
        assert!(found.hits.iter().all(|hit| hit.folder == grant.label));
        assert!(found.scanned >= 3, "the walk counts what it looked at");
        assert!(!found.truncated && found.refused.is_empty());

        let none = search_files(&broker, &grants, "nothing-like-this", 2);
        assert!(
            none.hits.is_empty(),
            "no matches is an answer, not a failure"
        );
        assert!(none.refused.is_empty());
    }

    /// A content query is text, not a path: a slash is legitimate, emptiness,
    /// an overlong query and a NUL are not, and with no grant the page says
    /// what to do first instead of answering "no matches".
    #[test]
    fn a_content_query_is_text_not_a_path() {
        let mut host = Controller::new();
        assert_eq!(
            host.search_content(""),
            Err("Type text to look for inside your files.")
        );
        assert_eq!(
            host.search_content("   "),
            Err("Type text to look for inside your files.")
        );
        assert_eq!(
            host.search_content(&"x".repeat(257)),
            Err("A content search is at most 256 characters.")
        );
        assert_eq!(
            host.search_content("a\0b"),
            Err("A content search cannot carry a NUL byte.")
        );
        assert_eq!(
            host.search_content("revenue"),
            Err("Grant a folder before searching inside it.")
        );
    }

    /// The matching lines are the broker's own: UTF-8 files only, with their
    /// 1-based line numbers and the line text, one folder's lines merged with
    /// another's, and a query nothing matches is an empty answer.
    #[test]
    fn a_content_search_reports_matching_lines_with_line_numbers() {
        let dir = Fixture::dir();
        std::fs::create_dir_all(dir.join("sub")).expect("a scratch subfolder");
        std::fs::write(
            dir.join("alpha-report.md"),
            "# Quarter report\nrevenue up\n",
        )
        .expect("a scratch file");
        std::fs::write(
            dir.join("sub/beta-report.md"),
            "no match here\nsecond revenue line\n",
        )
        .expect("a nested scratch file");
        std::fs::write(dir.join("notes.txt"), "shopping list\n").expect("a scratch file");
        let broker = ToolBroker::new();
        let grant = broker.grant_read_only(&dir).expect("a granted folder");
        let grants = vec![(grant.id.clone(), grant.label.clone())];

        let found = search_contents(&broker, &grants, "revenue", 1);
        assert_eq!(found.query, "revenue");
        let lines: Vec<(&str, u64, &str)> = found
            .hits
            .iter()
            .map(|hit| (hit.path.as_str(), hit.line, hit.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            vec![
                ("alpha-report.md", 2, "revenue up"),
                ("sub/beta-report.md", 2, "second revenue line"),
            ]
        );
        assert!(found.scanned >= 3, "the walk counts what it looked at");
        assert!(found.refused.is_empty() && !found.truncated);

        let none = search_contents(&broker, &grants, "no-such-text", 2);
        assert!(
            none.hits.is_empty(),
            "no matches is an answer, not a failure"
        );
        assert!(none.refused.is_empty());
    }

    /// A refused read says which refusal it was: each one names a different fix.
    #[test]
    fn a_refused_read_names_the_fix() {
        assert!(read_refusal("too_large").contains("64 KiB"));
        assert!(read_refusal("invalid_utf8").contains("UTF-8"));
        assert!(read_refusal("revoked").contains("no longer granted"));
        assert_eq!(read_refusal("weird"), "That file could not be read.");
    }

    static SCRATCH: AtomicU64 = AtomicU64::new(0);

    /// A unique scratch dir under the system temp dir, removed on drop.
    /// std-only: no tempdir dependency for a controller test.
    struct Fixture {
        dir: PathBuf,
    }
    impl Fixture {
        fn dir() -> PathBuf {
            let n = SCRATCH.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "muse-local-host-test-{}-{}",
                std::process::id(),
                n
            ));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }
        fn new() -> Self {
            Self { dir: Self::dir() }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn node_id_validation_accepts_only_stable_ids() {
        assert!(node_id_is_valid("linux-0123456789abcdef0123456789abcdef"));
        assert!(!node_id_is_valid(""));
        assert!(!node_id_is_valid("linux-short"));
        assert!(!node_id_is_valid("LINUX-0123456789abcdef0123456789abcdef"));
        assert!(!node_id_is_valid("linux-0123456789abcdef0123456789abcdeg"));
    }

    #[test]
    fn node_id_persists_per_installation_and_repairs_corruption() {
        let fixture = Fixture::new();
        let first = persisted_node_id(&fixture.dir).unwrap();
        assert!(node_id_is_valid(&first));
        let second = persisted_node_id(&fixture.dir).unwrap();
        assert_eq!(first, second);
        std::fs::write(fixture.dir.join(NODE_ID_FILE), "corrupt\n").unwrap();
        let third = persisted_node_id(&fixture.dir).unwrap();
        assert!(node_id_is_valid(&third));
        assert_ne!(third, first);
    }

    #[test]
    fn folder_policies_round_trip_owner_only() {
        let fixture = Fixture::new();
        assert!(load_folder_policies(&fixture.dir).is_empty());
        let roots = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        save_folder_policies(&fixture.dir, &roots).unwrap();
        assert_eq!(load_folder_policies(&fixture.dir), roots);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(policy_path(&fixture.dir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        save_folder_policies(&fixture.dir, &[]).unwrap();
        assert!(load_folder_policies(&fixture.dir).is_empty());
    }

    #[test]
    fn start_requires_consent_grants_and_a_stopped_host() {
        let mut host = Controller::new();
        // No grants, no consent, no config: every combination refuses.
        assert!(host.start(None, false).is_err());
        assert!(host.start(None, true).is_err());
        // A grant alone (no consent, no config) still refuses.
        let fixture = Fixture::new();
        host.broker.grant(&fixture.dir).unwrap();
        assert!(host.start(None, false).is_err());
        // Consent without an account config refuses before spawning.
        assert!(host.start(None, true).is_err());
    }

    #[test]
    fn grant_requires_consent_and_a_stopped_host() {
        let mut host = Controller::new();
        let dir = Fixture::dir();
        // The host is disconnected but nothing consented: refused.
        assert!(host.grant(dir.clone(), false, true).is_err());
        // Consent grants asynchronously; the completion lands on
        // poll. The worker runs beside the test, so wait for it.
        host.grant(dir.clone(), true, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while host.snapshot().grants.is_empty() && Instant::now() < deadline {
            host.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(host.snapshot().grants.len(), 1);
        assert!(host.snapshot().grants[0].writable);
        // Past eight grants the ninth refuses, consent or not.
        let capped_dir = Fixture::dir();
        let capped = Controller::new();
        let _held: Vec<Fixture> = (0..8)
            .map(|_| {
                let f = Fixture::new();
                capped.broker.grant(&f.dir).unwrap();
                f
            })
            .collect();
        assert!(capped.grant(capped_dir, true, true).is_err());
    }

    #[test]
    fn stop_is_idempotent_and_clears_session_state() {
        let mut host = Controller::new();
        host.stop();
        host.stop();
        let snap = host.snapshot();
        assert_eq!(snap.phase, Phase::Disconnected);
        assert!(snap.grants.is_empty());
        assert!(snap.cards.is_empty());
        assert!(snap.log.len() == 1);
        host.push_log("x".into());
        host.clear_log();
        assert!(host.snapshot().log.is_empty());
    }

    #[test]
    fn decide_without_a_registered_host_fails() {
        let mut host = Controller::new();
        assert!(host.decide("nope", true).is_err());
    }

    #[test]
    fn closed_codes_name_the_fix() {
        assert!(Controller::closed_notice("auth_required").contains("Sign in again"));
        // Unknown codes echo the code and the generic fix.
        let notice = Controller::closed_notice("weird-code");
        assert!(notice.contains("weird-code"));
    }

    #[test]
    fn safe_filters_never_leak_content_or_paths() {
        assert_eq!(safe_command("files.edit"), "files.edit");
        assert_eq!(safe_command("/etc/passwd"), "unknown-command");
        assert_eq!(safe_code("../../etc"), "etc");
        assert_eq!(safe_code("!!!"), "err");
        assert_eq!(safe_path("/a/b"), "a/b");
        assert_eq!(safe_path(""), "(invalid-path)");
    }
}
