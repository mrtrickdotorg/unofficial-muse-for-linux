//! Opt-in account integration via a separately installed community Python bridge.
//! Constructors are inert; methods block and belong on a worker thread.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fmt,
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_REQUEST: usize = 1024 * 1024;
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
/// How long one account operation may take before the client gives up on
/// it, by bridge op. These are the outer bound: the account host enforces
/// a tighter per-op budget of its own and recycles a stuck connection.
/// They replace a flat 180s that let one wedged call freeze the app for
/// minutes (Nov 10: the service answered a test in ~3s while the app
/// waited on a held gateway connection whose peer had vanished without
/// closing it -- the transport only checks its timeout between frames,
/// so no error ever surfaced).
fn deadline_for(op: Option<&str>) -> Duration {
    match op {
        Some("watch") => Duration::from_secs(100),
        Some("send") => Duration::from_secs(50),
        Some("activity") => Duration::from_secs(35),
        _ => Duration::from_secs(45),
    }
}

#[cfg(test)]
mod process_tests;

#[derive(Clone, Debug)]
pub struct BridgeConfig {
    pub python: PathBuf,
    pub script: PathBuf,
    pub config_dir: PathBuf,
}
#[derive(Clone, Debug)]
pub struct AccountClient {
    config: BridgeConfig,
    active: Option<Arc<AtomicBool>>,
    /// Overrides the derived host socket (tests and embedding).
    host: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub identity_name: String,
    pub vm_id: String,
    pub threads: Vec<Thread>,
}
/// Identity only: `AccountSnapshot` also carries every conversation, and the
/// service can spend seconds answering that list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountIdentity {
    pub identity_name: String,
    pub vm_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub session_id: String,
    pub title: String,
    pub archived: bool,
    /// The listing's own pin flag; older answers without it read
    /// as unpinned rather than failing the whole snapshot.
    #[serde(default)]
    pub pinned: bool,
    /// Whether the session is a conversation thread at all. The
    /// service seeds non-thread sessions (the "Learn about ..."
    /// walkthrough) into the same listing; the web sidebar shows
    /// threads only. Absent reads as a thread, so an older answer
    /// can never empty the sidebar.
    #[serde(default = "default_thread")]
    pub is_thread: bool,
    pub updated_at: Option<String>,
}

fn default_thread() -> bool {
    true
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub message_id: String,
    pub seq: u64,
    pub role: String,
    pub text: String,
    pub timestamp: Option<String>,
    pub status: Option<String>,
    /// Choice buttons the message's widget published, projected by the
    /// bridge from the confirmed `payload.widgets[].data.options`
    /// shape (probe, Nov 12, 2026). A press sends the option's text as
    /// the reader's next message.
    #[serde(default)]
    pub options: Vec<String>,
    /// The option already chosen on this widget when the service
    /// recorded one (`state.data.selectedText`): the buttons then
    /// render answered and inert, the chosen one marked.
    #[serde(default)]
    pub selected_option: Option<String>,
    /// Files attached to the message, projected by the bridge from
    /// `payload.metadata.attachments` (probe harvest, Nov 13, 2026).
    /// Display fields only: the service-side path and hash never
    /// cross into the client.
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
}

/// One attached file as the transcript shows it: a card with the
/// name, type, size, and -- for images -- the pixel dimensions,
/// caption and the service's transcription of the image.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAttachment {
    pub filename: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub bytes_len: Option<u64>,
    #[serde(default)]
    pub width: Option<u64>,
    #[serde(default)]
    pub height: Option<u64>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub transcription: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksSnapshot {
    pub schedules: Vec<Schedule>,
    pub runs: Vec<TaskRun>,
    /// Agent (subagent) activity for the side panel's Today list:
    /// active runs first, then recent ones, projected by the bridge
    /// from the confirmed `subagents.list` shape (probe, Nov 12, 2026).
    #[serde(default)]
    pub agents: Vec<AgentActivity>,
}

/// One subagent run as the side panel's Today list shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivity {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub final_response: Option<String>,
}
/// What the service answered to a cancel request.
///
/// `cancelled` is the service's word, not ours: a press with nothing running
/// comes back false, and the caller must say so instead of reporting a stop
/// that did not happen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelAnswer {
    pub cancelled: bool,
    #[serde(default)]
    pub message_id: Option<String>,
}
/// Bounded live listen: what the agent is doing right now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveStatus {
    pub events: Vec<LiveEvent>,
}
/// One live event, allowlisted by the bridge. Threaded replies never arrive
/// here, so `text` is a progress fragment, never the answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveEvent {
    pub event: String,
    #[serde(default)]
    pub seq: Option<u64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub title: String,
    pub enabled: bool,
    pub timezone: Option<String>,
    pub next_run_at_utc: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRun {
    pub run_id: String,
    pub job_id: String,
    pub status: String,
    pub result_summary: Option<String>,
    pub error_text: Option<String>,
    pub scheduled_for_utc: Option<i64>,
}
/// One Library artifact, as the bridge projected it.
///
/// `artifact_ref` is the service's own reference (what a later open, share or
/// delete would quote) and `path` is the workspace path it resolves to.
/// `can_open`/`can_share` stay the service's answers: this client never decides
/// on its own what may be shared or removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSummary {
    pub artifact_ref: String,
    pub title: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub placement: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub updated_at_ms: Option<i64>,
    pub pinned: bool,
    pub can_open: bool,
    pub can_share: bool,
}
/// One page of the Library listing.
///
/// `has_more` is the service's pagination answer; the cursor itself stays in the
/// bridge, so holding a page cannot leak a token into the window or the log.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactList {
    pub total_count: u64,
    pub has_more: bool,
    #[serde(default)]
    pub artifacts: Vec<ArtifactSummary>,
}
/// One row of an Ideas / Goals / Feed listing, as the bridge projected it.
///
/// These surfaces have recorded routes but no recorded payload shape, so
/// the bridge normalizes tolerantly and the window shows exactly these
/// fields: a title that always exists (the bridge derives one from the
/// body when the service gave none), and optional body, kind and stamp.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListingItem {
    #[serde(default)]
    pub id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub stamp: Option<String>,
    /// Feed only: the unit's one-line framing, when the bridge projected
    /// it, shown as a quiet meta line beside the category.
    #[serde(default)]
    pub kicker: Option<String>,
    /// Feed only: the unit's category, when projected.
    #[serde(default)]
    pub category: Option<String>,
    /// Feed only: the unit's emoji avatar (the gateway's `emoji` field,
    /// confirmed by the feed probe on Nov 12, 2026).
    #[serde(default)]
    pub icon: Option<String>,
    /// Feed only: the unit's hero image URL (the gateway's
    /// `attachment.hero_image_url`, same probe); the card's image
    /// plumbing renders it whenever it lands.
    #[serde(default)]
    pub image_url: Option<String>,
    /// Feed only: the readable text of the unit's widget attachment
    /// (the gateway's `attachment.widget_html`, extracted line-wise
    /// by the bridge). This is the fallback view, shown when the
    /// live frame below cannot render.
    #[serde(default)]
    pub widget_text: Option<String>,
    /// Feed only: the unit's widget attachment markup itself (the
    /// gateway's `attachment.widget_html`, capped by the bridge),
    /// rendered live in the card's navigation-locked WebView frame.
    #[serde(default)]
    pub widget_html: Option<String>,
}
/// One read of an Ideas / Goals / Feed listing.
///
/// `skipped` counts rows the service sent that this build could not
/// recognize; it is reported, never folded into the total, so an
/// unrecognized service shape cannot masquerade as an empty list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceListing {
    pub total: u64,
    pub recognized: u64,
    pub skipped: u64,
    #[serde(default)]
    pub items: Vec<ListingItem>,
    /// Feed only: the prompt that generates the feed, when the bridge
    /// could read it on the same pass (feed.prompt.get).
    #[serde(default)]
    pub feed_prompt: Option<FeedPrompt>,
    /// Feed only: generation status for the prompt card (feed.status).
    #[serde(default)]
    pub feed_status: Option<FeedStatus>,
}

/// The feed's generating prompt, for the display-only prompt card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedPrompt {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

/// Whether a feed edition is being generated right now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedStatus {
    #[serde(default)]
    pub refresh_in_progress: bool,
    #[serde(default)]
    pub next_edition_kind: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchReceipt {
    pub sent: bool,
    pub confirmed: bool,
    pub message_id: Option<String>,
}
/// One label/value pair the service attached to an approval request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDetail {
    pub label: String,
    pub value: String,
}
/// A pending approval, as the sentinel service describes it.
///
/// `decision`, `always_scope` and `attention_mode` keep the service's own words:
/// the window shows them rather than inventing a friendlier category, and
/// `details` is the request's own evidence for what the agent wants to do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub approval_id: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub always_scope: Option<String>,
    #[serde(default)]
    pub attention_mode: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub details: Vec<ApprovalDetail>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApprovals {
    #[serde(default)]
    pub pending: Vec<Approval>,
}
/// The five outcomes the service publishes. Nothing else is ever sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    AllowTask,
    AllowSite,
    AllowAlways,
    Deny,
}
impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowOnce => "allow_once",
            Self::AllowTask => "allow_task",
            Self::AllowSite => "allow_site",
            Self::AllowAlways => "allow_always",
            Self::Deny => "deny",
        }
    }
    /// Every outcome, for a window that must offer the service's own set and
    /// not a subset it chose to guess at.
    pub fn all() -> [Self; 5] {
        [
            Self::AllowOnce,
            Self::AllowTask,
            Self::AllowSite,
            Self::AllowAlways,
            Self::Deny,
        ]
    }
}
/// What the service said about a decision it took.
///
/// `accepted` means the service accepted the decision; it does not mean the
/// approved action then succeeded, so callers must not report an effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionAnswer {
    pub approval_id: String,
    pub requested: String,
    pub accepted: bool,
    #[serde(default)]
    pub decision: Option<String>,
}
/// One grant the service says is still in force.
///
/// The fields are the service's own words, projected by the bridge: the id a
/// revocation must name, what the grant covers, when it ends, and whether the
/// service itself marks it revocable. A window shows these; it does not
/// reinterpret them into categories of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveGrant {
    pub permission_id: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub lifetime: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub policy_subject: Option<String>,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    pub revocable: bool,
    #[serde(default)]
    pub title: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveGrants {
    #[serde(default)]
    pub grants: Vec<ActiveGrant>,
}
/// What the service said about a revocation it took.
///
/// `accepted` means the service accepted the request; it does not mean the
/// grant is gone. Only a fresh read of the active list proves that, so
/// callers re-read rather than trusting this answer, and `revoked` is the
/// service's own word when it supplied one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeAnswer {
    pub permission_id: String,
    pub accepted: bool,
    #[serde(default)]
    pub revoked: Option<bool>,
}
/// What the service said about a schedule deletion it took.
///
/// `accepted` means the service accepted the request; it does not mean the
/// schedule is gone. Only a fresh read of the listing proves that, so
/// callers re-read rather than trusting this answer, and `deleted` is the
/// service's own word when it supplied one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteAnswer {
    pub job_id: String,
    pub accepted: bool,
    #[serde(default)]
    pub deleted: Option<bool>,
}

/// A side chat the service just started, named by its session id.
///
/// The id is the whole answer: the bridge proves it against the
/// session listing, and the chat's title is the service's to give
/// once the conversation has a first message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartedSession {
    pub session_id: String,
}

/// What the service said about a session mutation it took.
///
/// `accepted` means the service accepted the request; it does not
/// mean the listing already shows the new state. Only a fresh read
/// of the listing proves that, so callers re-read rather than
/// trusting this answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOpAnswer {
    pub session_id: String,
    pub kind: String,
    pub accepted: bool,
}

/// What the service said about an idea-card execution it took.
///
/// `accepted` means the service took the request; the work itself
/// happens on the Muse side, and the session it runs in travels
/// only when the execution answer itself carried one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaExecuteAnswer {
    pub idea_id: String,
    pub accepted: bool,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// What the service said about a goal deletion it took.
///
/// Acceptance only, with the service's own `deleted` word when it
/// supplied one; whether the goal is actually gone is proven by
/// re-reading the goals listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalDeleteAnswer {
    pub goal_id: String,
    pub accepted: bool,
    #[serde(default)]
    pub deleted: Option<bool>,
}

/// What the service said about a feed reaction it took.
///
/// Acceptance only: whether the unit now shows the reaction is
/// proven by re-reading the feed, never by this answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedReactAnswer {
    pub unit_id: String,
    pub accepted: bool,
    pub reaction: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    AuthRequired,
    AccountChanged,
    VmUnavailable,
    Timeout,
    InvalidRequest,
    InsecureConfig,
    BridgeUnavailable,
    BridgeFailure,
    ProtocolError,
    ResponseTooLarge,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountError {
    pub code: ErrorCode,
    pub uncertain: bool,
    /// Local-only context, such as the interpreter that could not be started.
    /// The bridge never sends this, so a returned `bridge_unavailable` keeps it
    /// empty while a failed spawn fills it: the two causes stay distinguishable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
impl AccountError {
    fn new(code: ErrorCode, uncertain: bool) -> Self {
        Self {
            code,
            uncertain,
            detail: None,
        }
    }
    fn with_detail(code: ErrorCode, uncertain: bool, detail: impl Into<String>) -> Self {
        Self {
            code,
            uncertain,
            detail: Some(detail.into()),
        }
    }
}
impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.code {
            ErrorCode::AuthRequired => "Muse authentication is missing or expired; sign in again.",
            ErrorCode::AccountChanged => {
                "The saved Muse session now belongs to a different account; reconnect to continue."
            }
            ErrorCode::VmUnavailable => "The Muse account VM is unavailable.",
            ErrorCode::Timeout => "The account bridge exceeded its time limit.",
            ErrorCode::InvalidRequest => "The account request or bridge configuration is invalid.",
            ErrorCode::InsecureConfig => "Account configuration permissions are unsafe.",
            ErrorCode::BridgeUnavailable => {
                "The selected community account bridge is not installed or executable."
            }
            ErrorCode::BridgeFailure => "The community account gateway request failed.",
            ErrorCode::ProtocolError => "The account bridge returned an invalid response.",
            ErrorCode::ResponseTooLarge => "The account bridge response exceeded its size limit.",
        })?;
        if self.uncertain {
            f.write_str(" Delivery is uncertain; do not automatically resend.")?;
        }
        if let Some(detail) = &self.detail {
            f.write_str(&format!(" ({detail})"))?;
        }
        Ok(())
    }
}
impl std::error::Error for AccountError {}

#[derive(Deserialize)]
struct Envelope<T> {
    ok: bool,
    data: Option<T>,
    error: Option<AccountError>,
}

impl AccountClient {
    pub fn new(config: BridgeConfig) -> Self {
        Self {
            config,
            active: None,
            host: None,
        }
    }
    /// Bound this clone's subprocesses to a session lease. False cancels locally;
    /// an already dispatched remote action cannot be recalled and is uncertain.
    pub fn cancellable(&self, active: Arc<AtomicBool>) -> Self {
        Self {
            config: self.config.clone(),
            active: Some(active),
            host: self.host.clone(),
        }
    }
    /// Point this client at a specific host socket instead of the derived one.
    #[doc(hidden)]
    pub fn use_host_socket(&self, path: PathBuf) -> Self {
        Self {
            config: self.config.clone(),
            active: self.active.clone(),
            host: Some(path),
        }
    }
    pub fn connect(&self) -> Result<AccountSnapshot, AccountError> {
        self.request(serde_json::json!({"op": "connect"}), false)
    }
    /// Cheap account verification, for a preflight which must not pay for the
    /// conversation list. `connect` remains the way to load that list.
    pub fn identity(&self) -> Result<AccountIdentity, AccountError> {
        self.request(serde_json::json!({"op": "identity"}), false)
    }
    pub fn history(
        &self,
        thread: Option<&str>,
        limit: usize,
    ) -> Result<Vec<HistoryMessage>, AccountError> {
        validate_thread(thread)?;
        if !(1..=200).contains(&limit) {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "history", "thread": thread, "limit": limit}),
            false,
        )
    }
    /// Wait until the newest message changes, then return the window.
    ///
    /// The bridge holds one connection for the wait, so a recheck costs ~0.1s
    /// instead of a fresh Noise handshake per tick.
    pub fn watch(
        &self,
        thread: Option<&str>,
        limit: usize,
        timeout: Duration,
    ) -> Result<Vec<HistoryMessage>, AccountError> {
        self.watch_after(thread, limit, None, timeout)
    }
    /// Wait for anything newer than `after`, then return the window.
    ///
    /// A caller that already knows the newest message it has seen passes it
    /// here: the bridge then answers on its first read instead of waiting for
    /// the *next* change, so nothing that arrived while the request was being
    /// set up is missed.
    pub fn watch_after(
        &self,
        thread: Option<&str>,
        limit: usize,
        after: Option<&str>,
        timeout: Duration,
    ) -> Result<Vec<HistoryMessage>, AccountError> {
        validate_thread(thread)?;
        if !(1..=200).contains(&limit) {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let millis = u64::try_from(timeout.as_millis())
            .ok()
            .filter(|millis| (1..=60_000).contains(millis))
            .ok_or_else(|| AccountError::new(ErrorCode::InvalidRequest, false))?;
        let mut request = serde_json::json!({
            "op": "watch",
            "thread": thread,
            "limit": limit,
            "timeout": millis,
        });
        if let Some(after) = after {
            if after.is_empty() || after.len() > 256 {
                return Err(AccountError::new(ErrorCode::InvalidRequest, false));
            }
            request["after"] = after.into();
        }
        self.request(request, false)
    }
    pub fn tasks(&self) -> Result<TasksSnapshot, AccountError> {
        self.request(serde_json::json!({"op": "tasks"}), false)
    }
    /// List the account's Library, read-only.
    ///
    /// Listing is never consent: nothing here opens, shares, renames or deletes,
    /// and the bridge projects one page into narrow rows so no cursor or raw
    /// node payload reaches the window.
    pub fn artifacts(&self) -> Result<ArtifactList, AccountError> {
        self.request(serde_json::json!({"op": "artifacts"}), false)
    }
    /// List the account's idea cards, read-only.
    ///
    /// Executing or rating a card is a write; this client only reads.
    pub fn ideas(&self) -> Result<ServiceListing, AccountError> {
        self.request(serde_json::json!({"op": "ideas"}), false)
    }
    /// List the account's goals, read-only.
    pub fn goals(&self) -> Result<ServiceListing, AccountError> {
        self.request(serde_json::json!({"op": "goals"}), false)
    }
    /// List the account's feed units, read-only.
    ///
    /// Reactions, reordering and regeneration are writes and stay on the
    /// Muse side.
    pub fn feed(&self) -> Result<ServiceListing, AccountError> {
        self.request(serde_json::json!({"op": "feed"}), false)
    }
    /// Listen briefly for live agent/task status.
    ///
    /// Not a reply read: the pinned client records that threaded replies never
    /// arrive as live events, so the transcript keeps polling `history` and this
    /// feeds only the status line. Bounded to 30 s so a status read cannot hold
    /// the shared control connection for a whole minute.
    pub fn activity(&self, timeout: Duration) -> Result<LiveStatus, AccountError> {
        let millis = u64::try_from(timeout.as_millis())
            .ok()
            .filter(|millis| (1..=30_000).contains(millis))
            .ok_or_else(|| AccountError::new(ErrorCode::InvalidRequest, false))?;
        self.request(
            serde_json::json!({"op": "activity", "timeout": millis}),
            false,
        )
    }
    /// Ask the service to stop the run it currently has in flight.
    ///
    /// The answer says whether anything was stopped; it is not an intent
    /// record, so UI must not treat the call itself as a successful stop.
    pub fn cancel(&self) -> Result<CancelAnswer, AccountError> {
        self.request(serde_json::json!({"op": "cancel"}), false)
    }
    /// List what the agent is asking to do. Read-only: listing never decides.
    pub fn approvals(&self) -> Result<PendingApprovals, AccountError> {
        self.request(serde_json::json!({"op": "approvals"}), false)
    }
    /// Decide exactly one approval, once.
    ///
    /// The bridge accepts only the five published outcomes and makes a single
    /// request, so one press maps to one write. Nothing retries here: a decision
    /// whose answer was lost is re-read, never repeated.
    pub fn decide(
        &self,
        approval_id: &str,
        decision: Decision,
    ) -> Result<DecisionAnswer, AccountError> {
        if approval_id.is_empty() || approval_id.len() > 256 || approval_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({
                "op": "decide",
                "approval_id": approval_id,
                "decision": decision.as_str(),
            }),
            false,
        )
    }
    /// List the grants still in force. Read-only: listing never revokes.
    pub fn permissions(&self) -> Result<ActiveGrants, AccountError> {
        self.request(serde_json::json!({"op": "permissions"}), false)
    }
    /// Revoke exactly one grant, once.
    ///
    /// The id must be one the active listing published, and the bridge makes
    /// a single request, so one press maps to one write. Whether the grant is
    /// actually gone afterwards is proven by re-reading the list, never by
    /// this answer alone.
    pub fn revoke(&self, permission_id: &str) -> Result<RevokeAnswer, AccountError> {
        if permission_id.is_empty() || permission_id.len() > 256 || permission_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "revoke", "permission_id": permission_id}),
            false,
        )
    }
    /// Delete exactly one scheduled task, once.
    ///
    /// The id must be one the schedule listing published, and the bridge
    /// makes a single request, so one confirmed press maps to one write.
    /// Whether the schedule is actually gone afterwards is proven by
    /// re-reading the list, never by this answer alone.
    pub fn delete_task(&self, job_id: &str) -> Result<DeleteAnswer, AccountError> {
        if job_id.is_empty() || job_id.len() > 256 || job_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "delete_task", "job_id": job_id}),
            false,
        )
    }
    /// Start exactly one new side chat, once.
    ///
    /// One press maps to one `session.start` on the service, never
    /// retried: a retry could start a second chat the reader never
    /// asked for. The returned id is proven by the bridge against
    /// the session listing, not merely echoed from the start call.
    pub fn start_session(&self) -> Result<StartedSession, AccountError> {
        self.request(serde_json::json!({"op": "start_session"}), false)
    }
    /// Apply exactly one session mutation, once: pin, unpin,
    /// archive, unarchive, delete, or rename (with its title).
    ///
    /// The id must be one the session listing published and the
    /// bridge makes a single request, so one confirmed press maps
    /// to one write. Whether the listing then shows the new state
    /// is proven by re-reading it, never by this answer alone.
    pub fn session_op(
        &self,
        session_id: &str,
        kind: &str,
        title: Option<&str>,
    ) -> Result<SessionOpAnswer, AccountError> {
        if session_id.is_empty() || session_id.len() > 256 || session_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let mut body = serde_json::json!({
            "op": "session_op",
            "session_id": session_id,
            "kind": kind,
        });
        if let Some(title) = title {
            body["title"] = title.into();
        }
        self.request(body, false)
    }
    /// Delete exactly one goal, once.
    ///
    /// The id must be one the goals listing published, and the
    /// bridge makes a single request, so one confirmed press maps
    /// to one write. Whether the goal is actually gone afterwards
    /// is proven by re-reading the list, never by this answer.
    pub fn goal_delete(&self, goal_id: &str) -> Result<GoalDeleteAnswer, AccountError> {
        if goal_id.is_empty() || goal_id.len() > 256 || goal_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "goal_delete", "goal_id": goal_id}),
            false,
        )
    }
    /// Execute exactly one idea card, once (its "Try it").
    ///
    /// The id must be one the idea-cards listing published, and the
    /// bridge makes a single request, so one press maps to one
    /// execution: a retry could start the same work twice.
    pub fn idea_execute(&self, idea_id: &str) -> Result<IdeaExecuteAnswer, AccountError> {
        if idea_id.is_empty() || idea_id.len() > 256 || idea_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "idea_execute", "idea_id": idea_id}),
            false,
        )
    }
    /// React to exactly one feed unit, once.
    ///
    /// The id must be one the feed listing published and the bridge
    /// makes a single request, so one press maps to one write.
    /// Whether the unit then shows the reaction is proven by
    /// re-reading the feed, never by this answer alone.
    pub fn feed_react(
        &self,
        unit_id: &str,
        reaction: &str,
    ) -> Result<FeedReactAnswer, AccountError> {
        if unit_id.is_empty() || unit_id.len() > 256 || unit_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        if reaction.is_empty() || reaction.len() > 16 {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "feed_react", "unit_id": unit_id, "reaction": reaction}),
            false,
        )
    }
    /// Dispatch once; an unconfirmed receipt or uncertain error must not be
    /// automatically retried. Confirmation proves only prompt persistence.
    pub fn send(
        &self,
        thread: Option<&str>,
        message: &str,
    ) -> Result<DispatchReceipt, AccountError> {
        self.send_checked(thread, message, None, None)
    }
    /// Dispatch only after proving, in the same request, that the saved session
    /// is still `vm_id` for `identity_name`. One account check instead of a
    /// whole `connect`, which also lists every conversation.
    pub fn send_verified(
        &self,
        thread: Option<&str>,
        message: &str,
        vm_id: &str,
        identity_name: &str,
    ) -> Result<DispatchReceipt, AccountError> {
        self.send_checked(thread, message, None, Some((vm_id, identity_name)))
    }
    pub fn send_from_device_verified(
        &self,
        thread: Option<&str>,
        message: &str,
        device_id: &str,
        vm_id: &str,
        identity_name: &str,
    ) -> Result<DispatchReceipt, AccountError> {
        self.send_checked(
            thread,
            message,
            Some(device_id),
            Some((vm_id, identity_name)),
        )
    }
    /// Explicit registration attribution; never changes saved account settings.
    pub fn send_from_device(
        &self,
        thread: Option<&str>,
        message: &str,
        device_id: &str,
    ) -> Result<DispatchReceipt, AccountError> {
        if device_id.is_empty()
            || device_id.len() > 128
            || !device_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.send_checked(thread, message, Some(device_id), None)
    }
    fn send_checked(
        &self,
        thread: Option<&str>,
        message: &str,
        device_id: Option<&str>,
        expect: Option<(&str, &str)>,
    ) -> Result<DispatchReceipt, AccountError> {
        validate_thread(thread)?;
        if message.trim().is_empty() || message.len() > 512 * 1024 {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        if let Some(device_id) = device_id
            && (device_id.is_empty()
                || device_id.len() > 128
                || !device_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let mut request = serde_json::json!({"op": "send", "thread": thread, "message": message});
        if let Some(device_id) = device_id {
            request["device_id"] = device_id.into();
        }
        if let Some((vm_id, identity_name)) = expect {
            if vm_id.is_empty()
                || vm_id.len() > 256
                || identity_name.is_empty()
                || identity_name.len() > 256
            {
                return Err(AccountError::new(ErrorCode::InvalidRequest, false));
            }
            request["expect_vm_id"] = vm_id.into();
            request["expect_identity_name"] = identity_name.into();
        }
        let receipt: DispatchReceipt = self.request(request, true)?;
        if receipt.confirmed
            && (!receipt.sent || receipt.message_id.as_deref().is_none_or(str::is_empty))
        {
            return Err(AccountError::new(ErrorCode::ProtocolError, true));
        }
        Ok(receipt)
    }
    fn request<T: DeserializeOwned>(
        &self,
        mut request: serde_json::Value,
        sending: bool,
    ) -> Result<T, AccountError> {
        if !self.config.python.is_absolute()
            || !self.config.script.is_absolute()
            || !self.config.config_dir.is_absolute()
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let config_dir = self
            .config
            .config_dir
            .to_str()
            .ok_or_else(|| AccountError::new(ErrorCode::InvalidRequest, false))?;
        let deadline = deadline_for(request.get("op").and_then(serde_json::Value::as_str));
        let op_name = request
            .get("op")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_owned();
        request["config_dir"] = serde_json::Value::String(config_dir.to_owned());
        let bytes = serde_json::to_vec(&request)
            .map_err(|_| AccountError::new(ErrorCode::InvalidRequest, false))?;
        if bytes.len() > MAX_REQUEST {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        // A live host answers in ~0.1s on a connection it already holds. When
        // there is none (or it cannot start), the one-shot bridge below is
        // unchanged: a missing host must never block an account operation.
        let started = Instant::now();
        let via;
        let output = match self.host_exchange(&bytes, sending, deadline) {
            Ok(Some(output)) => {
                via = "host";
                output
            }
            Ok(None) => {
                via = "oneshot";
                // Only the one-shot path needs the interpreter and script; a
                // host that is already running answers without either.
                if let Err(error) = self.require_bridge() {
                    self.log_timing(&op_name, started, via, false);
                    return Err(error);
                }
                match run_bridge_active(
                    &self.config,
                    &bytes,
                    deadline,
                    sending,
                    self.active.as_deref(),
                ) {
                    Ok(output) => output,
                    Err(error) => {
                        self.log_timing(&op_name, started, via, false);
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                self.log_timing(&op_name, started, "host", false);
                return Err(error);
            }
        };
        let result = (|| {
            let envelope: Envelope<T> = serde_json::from_slice(&output)
                .map_err(|_| AccountError::new(ErrorCode::ProtocolError, sending))?;
            if envelope.ok {
                if envelope.error.is_some() {
                    return Err(AccountError::new(ErrorCode::ProtocolError, sending));
                }
                envelope
                    .data
                    .ok_or_else(|| AccountError::new(ErrorCode::ProtocolError, sending))
            } else {
                Err(envelope
                    .error
                    .unwrap_or_else(|| AccountError::new(ErrorCode::ProtocolError, sending)))
            }
        })();
        self.log_timing(&op_name, started, via, result.is_ok());
        result
    }
    /// Append one content-free timing line (op, duration, route, outcome)
    /// to the same op-timing.log the host and bridge write: together the
    /// three layers show which one of them ate a round trip's time.
    fn log_timing(&self, op: &str, started: Instant, via: &str, ok: bool) {
        append_timing_line(
            &self.config.config_dir,
            &format!(
                "client op={} ms={} ok={} via={}",
                op,
                started.elapsed().as_millis(),
                u8::from(ok),
                via
            ),
        );
    }
    /// Append the desktop's transcript-rebuild time to the same log.
    /// Every fetch stage already had a number; the render was the one
    /// stage without, which is why a slow chat open could not be
    /// attributed from the log alone (Nov 12, 2026).
    pub fn log_render(&self, rows: usize, elapsed: Duration) {
        append_timing_line(
            &self.config.config_dir,
            &format!("client render rows={} ms={}", rows, elapsed.as_millis()),
        );
    }
    /// The whole render pass, every section (thread list, panel,
    /// tasks, transcript): the transcript line alone kept clearing
    /// the render while the switch still crawled (Nov 13, 2026), so
    /// the pass as the user feels it gets a number too.
    pub fn log_render_full(&self, elapsed: Duration) {
        append_timing_line(
            &self.config.config_dir,
            &format!("client render-full ms={}", elapsed.as_millis()),
        );
    }
    /// Which build wrote the lines that follow: sessions from
    /// different builds share one log, and more than one diagnosis
    /// has chased a symptom the running build had already fixed.
    pub fn log_start(&self, build: &str) {
        append_timing_line(
            &self.config.config_dir,
            &format!("client start build={build}"),
        );
    }
    /// A chat-switch click, so the log brackets click -> render ->
    /// frame: which of the three legs eats the time is the whole
    /// question when a switch feels slow.
    pub fn log_switch(&self) {
        append_timing_line(&self.config.config_dir, "client switch");
    }
    /// A bottom-settle episode as it closes: how many geometry
    /// changes the transcript's adjustment reported and how many
    /// snaps the landing took. A quiet landing is 1-2 of each; a
    /// layout feedback loop is dozens.
    pub fn log_settle(&self, changes: u32, snaps: u32) {
        append_timing_line(
            &self.config.config_dir,
            &format!("client settle changes={changes} snaps={snaps}"),
        );
    }
    /// A named main-loop work unit (render, transcript, panel,
    /// threads, completion) that ran over 300ms: the unit reports
    /// itself, so a freeze inside the app's own code carries its
    /// name.
    pub fn log_work(&self, unit: &str, elapsed: Duration) {
        append_timing_line(
            &self.config.config_dir,
            &format!("client work unit={unit} ms={}", elapsed.as_millis()),
        );
    }
    /// The 250ms heartbeat timer fired this late: the main thread
    /// was blocked outside every named unit -- GTK's frame
    /// machinery, the compositor path, or the environment. An
    /// idle window does not delay a timer, so unlike the retired
    /// frame-tick watch this cannot be an idle artifact.
    pub fn log_heartbeat(&self, blocked: Duration) {
        append_timing_line(
            &self.config.config_dir,
            &format!("client heartbeat blocked_ms={}", blocked.as_millis()),
        );
    }
    /// One freeze episode, sampled by the desktop's watchdog
    /// thread while the heartbeat was stale: how stale the beat
    /// was, how much CPU the main thread burned across a 600ms
    /// window inside the freeze (near 600 = in-process work; near
    /// 0 = parked), and the syscall it sat in, with the peer path
    /// when it sat in a read on a unix socket.
    pub fn log_freeze(&self, blocked_ms: u64, main_cpu_ms: u64, syscall: &str) {
        append_timing_line(
            &self.config.config_dir,
            &format!(
                "client freeze blocked_ms={blocked_ms} main_cpu_ms={main_cpu_ms} syscall={syscall}"
            ),
        );
    }
    /// One request line to the host, one reply line back.
    ///
    /// `Ok(None)` means no host is available, so the caller keeps its original
    /// one-shot path. A failure after the request was written is an error, never
    /// a retry: the host may already have dispatched it.
    fn host_exchange(
        &self,
        request: &[u8],
        sending: bool,
        deadline: Duration,
    ) -> Result<Option<Vec<u8>>, AccountError> {
        let socket = self
            .host
            .clone()
            .unwrap_or_else(|| host_socket(&self.config.config_dir));
        if let Ok(stream) = UnixStream::connect(&socket) {
            match self.exchange_on(stream, request, sending, deadline) {
                Ok(line) => return Ok(Some(line)),
                Err(error) if matches!(error.code, ErrorCode::Timeout) => {
                    // The host answered nothing within the op's budget: one
                    // of its held connections is wedged. Terminate the host
                    // (SIGTERM is a clean shutdown for it) so the next
                    // exchange starts fresh instead of queueing behind the
                    // same wedge; a read may retry on the fresh host, a send
                    // may not be replayed.
                    self.kill_host(&socket);
                    if !sending {
                        return self.start_and_exchange(&socket, request, deadline);
                    }
                    return Err(error);
                }
                // A socket left behind by a host that has just exited: a read is
                // idempotent and can be retried on a fresh host, a send cannot
                // be replayed because it may already have been written.
                Err(_) if !sending => return self.start_and_exchange(&socket, request, deadline),
                Err(error) => return Err(error),
            }
        }
        self.start_and_exchange(&socket, request, deadline)
    }
    fn start_and_exchange(
        &self,
        socket: &Path,
        request: &[u8],
        deadline: Duration,
    ) -> Result<Option<Vec<u8>>, AccountError> {
        match self.start_host().and_then(|()| wait_for_socket(socket)) {
            Some(stream) => self.exchange_on(stream, request, false, deadline).map(Some),
            None => Ok(None),
        }
    }
    /// Terminate the account host that owns `socket`, via the pid file it
    /// publishes beside the socket. Best-effort: a missing or stale pid
    /// file just leaves the host as it was.
    fn kill_host(&self, socket: &Path) {
        let Ok(text) = std::fs::read_to_string(host_pid_path(socket)) else {
            return;
        };
        let Ok(pid) = text.trim().parse::<libc::pid_t>() else {
            return;
        };
        if pid > 0 {
            // SAFETY: kill() with a pid parsed from the host's own pid
            // file; SIGTERM is the host's documented clean shutdown.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
    fn exchange_on(
        &self,
        stream: UnixStream,
        request: &[u8],
        sending: bool,
        deadline: Duration,
    ) -> Result<Vec<u8>, AccountError> {
        let fail = |code: ErrorCode| AccountError::new(code, sending);
        let started = Instant::now();
        stream
            .set_write_timeout(Some(deadline))
            .map_err(|_| fail(ErrorCode::BridgeFailure))?;
        // Short reads so a cancelled lease is noticed during a long wait.
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|_| fail(ErrorCode::BridgeFailure))?;
        let mut writer = stream
            .try_clone()
            .map_err(|_| fail(ErrorCode::BridgeFailure))?;
        if self
            .active
            .as_deref()
            .is_some_and(|active| !active.load(Ordering::SeqCst))
        {
            return Err(fail(ErrorCode::BridgeFailure));
        }
        writer
            .write_all(request)
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush())
            .map_err(|error| {
                AccountError::with_detail(
                    ErrorCode::BridgeFailure,
                    sending,
                    format!("host write: {error}"),
                )
            })?;
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        loop {
            if self
                .active
                .as_deref()
                .is_some_and(|active| !active.load(Ordering::SeqCst))
            {
                return Err(fail(ErrorCode::BridgeFailure));
            }
            if started.elapsed() >= deadline {
                return Err(fail(ErrorCode::Timeout));
            }
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => {
                    return Err(AccountError::with_detail(
                        ErrorCode::BridgeFailure,
                        sending,
                        "host closed the connection".to_owned(),
                    ));
                }
                Ok(_) if line.last() == Some(&b'\n') => {
                    line.pop();
                    return Ok(line);
                }
                Ok(_) => {
                    if line.len() > MAX_RESPONSE {
                        return Err(fail(ErrorCode::ResponseTooLarge));
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => {
                    return Err(AccountError::with_detail(
                        ErrorCode::BridgeFailure,
                        sending,
                        format!("host read: {error}"),
                    ));
                }
            }
        }
    }
    fn require_bridge(&self) -> Result<(), AccountError> {
        if !self.config.python.is_file() {
            return Err(AccountError::with_detail(
                ErrorCode::BridgeUnavailable,
                false,
                format!("interpreter not found: {}", self.config.python.display()),
            ));
        }
        if !self.config.script.is_file() {
            return Err(AccountError::with_detail(
                ErrorCode::BridgeUnavailable,
                false,
                format!("bridge script not found: {}", self.config.script.display()),
            ));
        }
        Ok(())
    }
    fn start_host(&self) -> Option<()> {
        // The host lives beside the bridge script; a stale install has none and
        // falls back instead of failing.
        let script = self.config.script.with_file_name("muse_account_host.py");
        if !script.is_file() {
            return None;
        }
        let mut command = Command::new(&self.config.python);
        bridge_environment(&mut command);
        // Deliberately NO PR_SET_PDEATHSIG: it fires when the creating
        // *thread* dies, and account jobs run on short-lived worker
        // threads -- so every host was SIGTERM'd the moment the job that
        // happened to spawn it finished (Patrick's op-timing.log, Nov 10:
        // four host serve-starts in twelve seconds, each serving one to
        // three ops; a watch that started as its host died never landed,
        // which presented as replies taking minutes while the service
        // answered in seconds). The host's lifetime contract is its own:
        // it exits when its parent *process* changes (a ppid watch) or
        // after five idle minutes, both of which key on the app, not on
        // whichever job thread arrived first.
        command
            .arg("-I")
            .arg(&script)
            .arg("--config-dir")
            .arg(&self.config.config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .ok()?;
        // Dropped, never killed: the host exits when it is orphaned or idle.
        Some(())
    }
}

/// FNV-1a 64. The Python host derives its socket name the same way, so this
/// function is a cross-language contract rather than an internal detail.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut value: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        value ^= u64::from(*byte);
        value = value.wrapping_mul(0x0000_0100_0000_01b3);
    }
    value
}

/// Append one line to <config_dir>/op-timing.log (epoch-ms prefixed, the
/// same file the Python host and bridge write). Best-effort: timing must
/// never fail an account operation.
pub fn append_timing_line(config_dir: &Path, line: &str) {
    use std::io::Write;
    let epoch_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(config_dir.join("op-timing.log"))
    {
        let _ = writeln!(file, "{epoch_ms} {line}");
    }
}

/// The pid file the account host publishes beside its socket; the Python
/// host derives it the same way (`socket_path + ".pid"`), so this is a
/// cross-language contract rather than an internal detail.
pub fn host_pid_path(socket: &Path) -> PathBuf {
    let mut name = socket.as_os_str().to_owned();
    name.push(".pid");
    PathBuf::from(name)
}

pub fn host_socket(config_dir: &Path) -> PathBuf {
    let name = format!(
        "account-{:016x}.sock",
        fnv1a64(config_dir.as_os_str().as_encoded_bytes())
    );
    let root = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => match std::env::var_os("XDG_CACHE_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"),
        },
    };
    root.join("unofficial-muse-for-linux").join(name)
}

fn bridge_environment(command: &mut Command) {
    const KEYS: [&str; 21] = [
        "HOME",
        "PATH",
        "LANG",
        "LC_ALL",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ];
    command.env_clear();
    for key in KEYS {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("MUSE_MCP_ENABLE_RAW", "0")
        .env("PYTHONDONTWRITEBYTECODE", "1");
}

fn wait_for_socket(path: &Path) -> Option<UnixStream> {
    let started = Instant::now();
    // First launch imports the bridge and opens two connections; give the
    // process a moment to bind before deciding it is absent.
    while started.elapsed() < Duration::from_secs(5) {
        if let Ok(stream) = UnixStream::connect(path) {
            return Some(stream);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

fn validate_thread(thread: Option<&str>) -> Result<(), AccountError> {
    if thread.is_some_and(|s| s.trim().is_empty() || s.len() > 4096 || s.contains('\0')) {
        return Err(AccountError::new(ErrorCode::InvalidRequest, false));
    }
    Ok(())
}

/// Kill the dedicated process group before reaping, including a descendant which
/// might otherwise keep stdout open. No wait/read can outlive the deadline.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // SAFETY: the child was started as leader of its own process group.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn nonblocking(fd: i32) -> std::io::Result<()> {
    // SAFETY: fcntl accesses a valid pipe descriptor owned by this call.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(test)]
fn run_bridge(
    config: &BridgeConfig,
    request: &[u8],
    timeout: Duration,
    sending: bool,
) -> Result<Vec<u8>, AccountError> {
    run_bridge_active(config, request, timeout, sending, None)
}
fn run_bridge_active(
    config: &BridgeConfig,
    request: &[u8],
    timeout: Duration,
    sending: bool,
    active: Option<&AtomicBool>,
) -> Result<Vec<u8>, AccountError> {
    if active.is_some_and(|a| !a.load(Ordering::SeqCst)) {
        return Err(AccountError::new(ErrorCode::InvalidRequest, false));
    }
    let mut command = Command::new(&config.python);
    bridge_environment(&mut command);
    let child = command
        .arg("-I")
        .arg(&config.script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|error| {
            AccountError::with_detail(
                ErrorCode::BridgeUnavailable,
                false,
                format!("could not start {}: {error}", config.python.display()),
            )
        })?;
    let mut guard = ChildGuard(child);
    let io_error = || AccountError::new(ErrorCode::BridgeFailure, sending);
    let stdin = guard.0.stdin.take().ok_or_else(io_error)?;
    let mut stdout = guard.0.stdout.take().ok_or_else(io_error)?;
    nonblocking(stdin.as_raw_fd()).map_err(|_| io_error())?;
    nonblocking(stdout.as_raw_fd()).map_err(|_| io_error())?;
    let started = Instant::now();
    let mut input = Some(stdin);
    let mut written = 0;
    let mut output = Vec::new();
    let mut eof = false;
    loop {
        if active.is_some_and(|a| !a.load(Ordering::SeqCst)) {
            return Err(AccountError::new(ErrorCode::BridgeFailure, sending));
        }
        if started.elapsed() >= timeout {
            return Err(AccountError::new(ErrorCode::Timeout, sending));
        }
        if let Some(pipe) = input.as_mut() {
            match pipe.write(&request[written..]) {
                Ok(0) => {
                    input = None;
                }
                Ok(n) => {
                    written += n;
                    if written == request.len() {
                        input = None;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                    input = None;
                }
                Err(_) => return Err(io_error()),
            }
        }
        // Read only bounded chunks and return to timeout checks even for a flood.
        for _ in 0..16 {
            let mut chunk = [0; 8192];
            match stdout.read(&mut chunk) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    if output.len() + n > MAX_RESPONSE {
                        return Err(AccountError::new(ErrorCode::ResponseTooLarge, sending));
                    }
                    output.extend_from_slice(&chunk[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(io_error()),
            }
        }
        if let Some(status) = guard.0.try_wait().map_err(|_| io_error())?
            && eof
        {
            if !status.success() {
                return Err(io_error());
            }
            return Ok(output);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::{HistoryMessage, ServiceListing, TasksSnapshot, deadline_for};
    use std::time::Duration;

    #[test]
    fn op_deadlines_bound_a_wedged_call_well_under_the_old_flat_180s() {
        // Regression shape for the Nov 10 lockup: every op's outer bound
        // must land in tens of seconds, the watch must still cover its
        // 45s window plus the full-window read, and the activity listen
        // must cover its ~4s subscribe with room for a cold start.
        for op in [
            None,
            Some("history"),
            Some("send"),
            Some("watch"),
            Some("activity"),
        ] {
            assert!(deadline_for(op) < Duration::from_secs(180), "{op:?}");
        }
        assert!(deadline_for(Some("watch")) >= Duration::from_secs(60));
        assert!(deadline_for(Some("activity")) >= Duration::from_secs(15));
        assert!(deadline_for(Some("watch")) > deadline_for(Some("send")));
        assert!(deadline_for(Some("send")) > deadline_for(Some("activity")));
    }

    #[test]
    fn history_messages_without_widget_fields_still_parse() {
        let message: HistoryMessage = serde_json::from_value(serde_json::json!({
            "message_id": "m", "seq": 1, "role": "assistant", "text": "hi",
            "timestamp": null, "status": null
        }))
        .expect("pre-widget bridge payloads keep parsing");
        assert!(message.options.is_empty());
        assert!(message.selected_option.is_none());
    }

    #[test]
    fn tasks_snapshots_without_agents_still_parse() {
        let snapshot: TasksSnapshot = serde_json::from_value(serde_json::json!({
            "schedules": [], "runs": []
        }))
        .expect("pre-activity bridge payloads keep parsing");
        assert!(snapshot.agents.is_empty());
    }

    #[test]
    fn listings_without_feed_card_fields_still_parse() {
        let page: ServiceListing = serde_json::from_value(serde_json::json!({
            "total": 1, "recognized": 1, "skipped": 0, "items": []
        }))
        .expect("non-feed listings keep parsing");
        assert!(page.feed_prompt.is_none());
        assert!(page.feed_status.is_none());
    }
}
