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
        Some("attach_probe") => Duration::from_secs(60),
        Some("stream_probe") => Duration::from_secs(60),
        Some("send_images") => Duration::from_secs(90),
        Some("dictate") => Duration::from_secs(90),
        Some("link_preview") => Duration::from_secs(30),
        Some("activity") => Duration::from_secs(35),
        _ => Duration::from_secs(45),
    }
}

/// What the service heard in a dictation turn: the transcript when
/// it heard words, else the answered key list so the shape stays
/// describable. A noise-mode route may answer nothing at all; that
/// projects as heard nothing, never as a transport failure.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct DictationVerdict {
    #[serde(default)]
    pub transcript: Option<String>,
    #[serde(default)]
    pub keys: Vec<String>,
}

/// A link-preview card: whatever scalar fields the service returned
/// for a url, each optional because the response shape is not
/// recorded. A thin answer is a thin card, never a failed row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct LinkPreview {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub site_name: Option<String>,
    #[serde(default)]
    pub keys: Vec<String>,
}

/// The base64 the stdio frame needs for raw audio bytes. No new
/// dependency: the alphabet is fixed and the padding is arithmetic.
pub fn encode_b64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut word = chunk
            .iter()
            .fold(0u32, |word, byte| (word << 8) | *byte as u32);
        // Left-align: a short final chunk's bytes sit at the top, so
        // the sextet picks line up with the full-chunk positions.
        word <<= 8 * (3 - chunk.len());
        let padded = chunk.len();
        for index in 0..4 {
            if index <= padded {
                out.push(ALPHABET[((word >> (18 - 6 * index)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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

/// One live event, the allowlisted projection the bridge returns for both
/// the activity read and the stream replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamEvent {
    pub event: String,
    pub seq: Option<u64>,
    pub text: Option<String>,
    pub status: Option<String>,
    pub session_id: Option<String>,
}

/// Windowed transcript page, the web UI's history_window shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryWindow {
    pub messages: Vec<HistoryMessage>,
    #[serde(default)]
    pub has_older: bool,
    #[serde(default)]
    pub has_newer: bool,
    #[serde(default)]
    pub newest_seq: Option<u64>,
    #[serde(default)]
    pub transcript_revision: Option<String>,
}

/// Selected-model state, the web UI's model.get/model.set shape.
///
/// The service's `generation` wins over anything cached: a caller keeps
/// an answer only when its generation is newer than what it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    #[serde(default)]
    pub generation: Option<i64>,
    #[serde(default)]
    pub selected_model: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub available_models: Vec<String>,
    #[serde(default)]
    pub display_name: Option<String>,
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
    /// Unread messages in the thread, as the listing reports
    /// them; drives the sidebar badge. Absent reads as zero.
    #[serde(default)]
    pub unread_count: u64,
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The open target's kind ("file", "space", ...), when the
    /// listing gave one; a space row is recognised by this, not
    /// by its format word.
    #[serde(default)]
    pub target_kind: Option<String>,
    /// A space row's handle: the slug its open target names.
    /// Space actions address the space by this, never by the
    /// artifact reference.
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub updated_at_ms: Option<i64>,
    pub pinned: bool,
    pub can_open: bool,
    pub can_share: bool,
}
/// One page of the Library listing.
///
/// `has_more` is the service's pagination answer. `next_cursor`
/// is the page's own opaque continuation token: the client holds
/// it only to hand back verbatim on the next page request, and
/// never displays, logs or interprets it. `paging` is set on a
/// follow-up page: "continued" when the bridge proved the page
/// continues the list, "unavailable" when no candidate paging
/// parameter did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactList {
    pub total_count: u64,
    pub has_more: bool,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub paging: Option<String>,
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
    /// Feed only: the unit's service action label (the gateway's
    /// `action.label`, confirmed by the user's timing-log probe):
    /// actionable units render this button instead of Discuss.
    #[serde(default)]
    pub action_label: Option<String>,
    /// Feed only: the service action kind beside the label.
    #[serde(default)]
    pub action_kind: Option<String>,
    /// Feed only: why the service created this unit (the gateway's
    /// `why_did_i_see_this`, same probe): shown from the row's ⋯ menu.
    #[serde(default)]
    pub why: Option<String>,
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedPrompt {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

/// Whether a feed edition is being generated right now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedStatus {
    #[serde(default)]
    pub refresh_in_progress: bool,
    #[serde(default)]
    pub next_edition_kind: Option<String>,
}
/// One remembered passage, as `memory.search` returned it (live, Nov 14, 2026).
///
/// `memory_uri` is the service's own address and the only thing `memory.get`
/// accepts, so it is kept verbatim and never rebuilt from `path`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryHit {
    pub memory_uri: String,
    pub text: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub line_start: Option<u64>,
    #[serde(default)]
    pub line_end: Option<u64>,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

/// One memory search, with the rows this build could not read counted
/// rather than silently dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemorySearch {
    pub recognized: u64,
    pub skipped: u64,
    #[serde(default)]
    pub hits: Vec<MemoryHit>,
}

/// One resolved memory entry.
///
/// `found: false` is the service's own answer about an address it no longer
/// holds, not a failed read, so it is data rather than an error.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub found: bool,
    #[serde(default)]
    pub memory_uri: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub line_start: Option<u64>,
    #[serde(default)]
    pub line_end: Option<u64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchReceipt {
    pub sent: bool,
    pub confirmed: bool,
    pub message_id: Option<String>,
    /// Image sends only: the landed row's attachment did not
    /// match what was sent (byte count differs), so the picture
    /// the reader pasted is not the picture the service stored.
    /// Always false for plain sends and older bridges.
    #[serde(default)]
    pub stored_mismatch: bool,
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
/// One issued grant, the web UI's grants.list item shape.
///
/// Scalar identity and lifecycle fields only; the paging cursor travels
/// opaque beside the page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantItem {
    pub grant_id: String,
    pub subject_kind: String,
    pub subject_id: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub revision: Option<u64>,
    #[serde(default)]
    pub not_before_ms: Option<i64>,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    #[serde(default)]
    pub revoked_at_ms: Option<i64>,
    #[serde(default)]
    pub provenance: Option<String>,
    #[serde(default)]
    pub created_at_ms: Option<i64>,
}
/// One page of the issued-access inventory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantsPage {
    #[serde(default)]
    pub grants: Vec<GrantItem>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub has_more: bool,
}
/// One action group's allow/ask/deny surface inside a connector scope.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedGroup {
    pub key: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub methods: Vec<String>,
}
/// One connector scope's permission map, the read half of the scoped
/// grant model the web UI's Policy page renders.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedPermissions {
    pub scope_type: String,
    pub scope_id: String,
    #[serde(default)]
    pub groups: Vec<ScopedGroup>,
}
/// One scoped mode change, the write half of the scoped grant model.
///
/// Carried as one struct so the call stays a single revision-guarded write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedUpdate {
    pub scope_type: String,
    pub scope_id: String,
    pub expected_revision: u64,
    pub connector: String,
    pub method: String,
    pub mode: String,
    pub display_name: String,
}
/// What the service said about a run it accepted.
///
/// `accepted` means the request landed; a run showing up is a fact only a
/// fresh read of tasks.runs holds, so the caller re-reads rather than
/// trusting this answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunAnswer {
    pub job_id: String,
    pub accepted: bool,
}
/// What the service said about a scoped mode change it accepted.
///
/// The change itself is proven by re-reading the scope, never by this
/// answer alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedUpdateAnswer {
    pub scope_type: String,
    pub scope_id: String,
    pub method: String,
    pub mode: String,
    pub accepted: bool,
}
/// What the service said about a message/turn deletion it accepted.
///
/// Acceptance is not absence: the caller re-reads history to prove the
/// row is gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDeleteAnswer {
    pub target: String,
    pub message_id: String,
    pub accepted: bool,
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
/// One durable policy setting, as the service reports it: its
/// own dotted name and the scalar value it holds, projected by
/// the bridge from the settings body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyEntry {
    pub label: String,
    pub value: String,
}

/// The service's durable policy settings, for display.
///
/// `connectors` are the names the settings' own connectors
/// section published -- the only names a reset may address.
/// `withheld` counts values the projection could not show as
/// pairs (anything deeper than a scalar leaf), so a page can say
/// the list is partial instead of implying it is the whole.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySettings {
    #[serde(default)]
    pub entries: Vec<PolicyEntry>,
    #[serde(default)]
    pub connectors: Vec<String>,
    #[serde(default)]
    pub withheld: u64,
}

/// What the service said about a connector reset it took.
///
/// Acceptance only: whether the connector now reads as reset is
/// proven by re-reading the settings, never by this answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorResetAnswer {
    pub connector: String,
    pub accepted: bool,
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

/// What the service said about a space unsharing it took.
///
/// Acceptance only: whether the space now reads as unshared is
/// proven by re-reading the Library, never by this answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceUnshareAnswer {
    pub slug: String,
    pub accepted: bool,
}

/// What the service's HTML export of one space came back as.
///
/// `html` is present only when the export decoded as a page and
/// fit the frame it travels in; `size` always reports the
/// export's true byte count, and `truncated` says the page was
/// withheld for size rather than lost.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceExportAnswer {
    pub slug: String,
    pub format: String,
    pub size: u64,
    pub truncated: bool,
    pub html: Option<String>,
}

/// What the service said about a mark-as-read it took.
///
/// Acceptance only: whether the thread now reads as seen is
/// proven by re-reading the sessions listing, never by this
/// answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkSeenAnswer {
    pub thread_id: String,
    pub accepted: bool,
}

/// The attach probe's verdicts, step by step. Each field is what
/// that step itself proved: whether the service took the file
/// write ("accepted" or "refused"), whether the send dispatched,
/// and whether the thread's own history then showed the message
/// -- and the file stored as its attachment. A false is a
/// finding, not a failure: the probe exists to learn which step
/// the recorded-shape candidate does not survive.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachProbeAnswer {
    pub thread: String,
    pub fs_write: String,
    pub sent: bool,
    pub message_seen: bool,
    pub attachment_seen: bool,
    /// The binary leg's per-candidate verdicts; absent from
    /// older bridges.
    #[serde(default)]
    pub binary: Vec<BinaryVerdict>,
}

/// What one stream probe learned: whether the service accepted
/// a per-thread event subscription at all, and how many events
/// it carried in the listen window. The event shapes themselves
/// are the bridge's timing log, never this answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamProbeAnswer {
    pub thread: String,
    pub accepted: bool,
    pub events: u64,
}

/// One candidate fs.write encoding's verdict from the attach
/// probe's binary leg, read back from the stored attachment's
/// own projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryVerdict {
    pub candidate: u32,
    pub verdict: String,
    pub bytes_len: Option<u64>,
}

/// One pasted image on its way to the bridge: a staged-file
/// name (the bridge reads the bytes from the staging directory
/// both sides share) and the bytes themselves, which only the
/// client touches -- it writes the staging file and removes it
/// once the send resolves, whichever way it resolves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedImage {
    pub name: String,
    pub bytes: Vec<u8>,
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

/// What the service said about a feed-unit deletion it took.
///
/// Acceptance only, with the service's own `deleted` word when it
/// supplied one; whether the unit is actually gone is proven by
/// re-reading the feed. The route is the same evidence class as
/// the goal deletion: a path-only DELETE from the recorded table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedUnitDeleteAnswer {
    pub unit_id: String,
    pub accepted: bool,
    #[serde(default)]
    pub deleted: Option<bool>,
}

/// What the service said about a Library pin-state change it took.
///
/// The body is the live-probed shape: the artifact reference with a
/// real boolean. Acceptance only: whether the row now shows the
/// state is proven by re-reading the Library, never by this answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPinAnswer {
    pub artifact_ref: String,
    pub accepted: bool,
    pub pinned: bool,
}

/// What the service said about a Library file deletion it took.
///
/// The body is the live-probed shape: the service's own relative
/// path with `recursive: false`. Acceptance only: whether the row
/// is gone is proven by re-reading the Library, never by this
/// answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDeleteAnswer {
    pub path: String,
    pub accepted: bool,
}

/// What the service said about an idea engagement it took.
///
/// The body is the live-probed shape: `{event_type:
/// feedback_up}`, the engagement enum's own positive literal (the
/// web's "More like this"). Acceptance only: ranking is the
/// service's business, never this answer's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaEngagementAnswer {
    pub id: String,
    pub accepted: bool,
}

/// What the service said about idea feedback it took.
///
/// The body is the live-probed shape: `{feedback: feedback_down}`,
/// the feedback enum's own negative literal (the web's "Not
/// interested", which dismisses the card). Acceptance only:
/// whether the card is gone is proven by re-reading the ideas
/// listing, never by this answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaFeedbackAnswer {
    pub id: String,
    pub accepted: bool,
}

/// What the service said about a goal rename it took.
///
/// The body is the live-probed shape: PATCH `/goals/{id}`
/// `{title}`. Acceptance only: whether the row shows the new
/// title is proven by re-reading the goals listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalRenameAnswer {
    pub goal_id: String,
    pub accepted: bool,
    pub title: String,
}

/// What the service said about a goal status flip it took.
///
/// The body is the live-probed shape: PATCH `/goals/{id}`
/// `{status: completed|active}`. Acceptance only: whether the row
/// shows the state is proven by re-reading the goals listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalSetCompleteAnswer {
    pub goal_id: String,
    pub accepted: bool,
    pub completed: bool,
}

/// What the service said about a feed prompt replacement it took.
///
/// The body is the live-probed shape: PATCH `/api/feed/prompt`
/// `{prompt}`. Acceptance only: whether the card shows the new
/// words is proven by re-reading the feed, never by this answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedPromptUpdateAnswer {
    pub accepted: bool,
}

/// One goal as the detail view needs it: the display fields of a
/// `goals.get` answer's `goal`, length-capped by the bridge like
/// the listing rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalDetailGoal {
    #[serde(default)]
    pub goal_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub momentum: Option<String>,
    #[serde(default)]
    pub momentum_status: Option<String>,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub parent_goal_id: Option<String>,
}

/// One entry of a `goals.get` answer's `updates`: title, summary,
/// description and date.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalDetailUpdate {
    #[serde(default)]
    pub update_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub effective_at: Option<String>,
}

/// One entry of a `goals.get` answer's `associations`: the link
/// type and the linked row's own title.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalDetailAssociation {
    #[serde(default)]
    pub association_type: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// What a `goals.get` read answered: the shaped goal, its
/// updates, and its associations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalGetAnswer {
    #[serde(default)]
    pub goal: GoalDetailGoal,
    #[serde(default)]
    pub updates: Vec<GoalDetailUpdate>,
    #[serde(default)]
    pub associations: Vec<GoalDetailAssociation>,
}

/// What the service said about a subgoal creation it took.
///
/// The body is the live-probed shape: POST `/goals` with `{title,
/// summary, description, emoji, parent_goal_id}`. Acceptance only:
/// whether the child exists is proven by re-reading the goals
/// listing, never by this answer alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalAddSubgoalAnswer {
    pub goal_id: String,
    pub parent_goal_id: String,
    pub accepted: bool,
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
    UnsupportedMethod,
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
            ErrorCode::UnsupportedMethod => "The Muse service does not support that request.",
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
    /// Windowed transcript paging the way the web UI reads it.
    ///
    /// An anchor seq plus a bounded window on each side, instead of a full
    /// re-read of a long conversation. The answer carries the rows plus the
    /// paging flags the web UI renders from.
    pub fn history_window(
        &self,
        thread: Option<&str>,
        anchor_seq: u64,
        before_limit: usize,
        after_limit: usize,
    ) -> Result<HistoryWindow, AccountError> {
        validate_thread(thread)?;
        if !(1..=200).contains(&before_limit) || !(1..=200).contains(&after_limit) {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({
                "op": "history_window",
                "thread": thread,
                "anchor_seq": anchor_seq,
                "before_limit": before_limit,
                "after_limit": after_limit,
            }),
            false,
        )
    }
    /// Replay a turn's stream events from either anchor.
    ///
    /// Exactly one of `message_id` / `reply_to_message_id`, the web UI's
    /// `chat.stream_events` shape with the 'all' lane. Read-only.
    pub fn stream_events(
        &self,
        message_id: Option<&str>,
        reply_to_message_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StreamEvent>, AccountError> {
        let anchors = [message_id, reply_to_message_id];
        if anchors
            .iter()
            .filter(|a| matches!(a, Some(s) if !s.is_empty() && s.len() <= 4096))
            .count()
            != 1
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        if !(1..=200).contains(&limit) {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let answer: serde_json::Value = self.request(
            serde_json::json!({
                "op": "stream_events",
                "message_id": message_id,
                "reply_to_message_id": reply_to_message_id,
                "limit": limit,
            }),
            false,
        )?;
        serde_json::from_value(
            answer
                .get("events")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|_| AccountError::new(ErrorCode::ProtocolError, false))
    }
    /// One bounded listen on a per-area subscription.
    ///
    /// The shipped 12.1 client holds one subscribe per area (activity,
    /// fs, identity, ...): the method names one of those, the body is a
    /// small JSON object of scalar fields. Read-only.
    pub fn subscribe(
        &self,
        method: &str,
        body: serde_json::Value,
        limit: usize,
    ) -> Result<Vec<StreamEvent>, AccountError> {
        if method.is_empty() || method.len() > 64 || method.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        if !(1..=200).contains(&limit) {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        if !body.is_null() && !body.is_object() {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let answer: serde_json::Value = self.request(
            serde_json::json!({
                "op": "subscribe",
                "method": method,
                "body": body,
                "limit": limit,
            }),
            false,
        )?;
        serde_json::from_value(
            answer
                .get("events")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|_| AccountError::new(ErrorCode::ProtocolError, false))
    }
    /// Read the selected-model state the web UI loads on start.
    ///
    /// The answer carries the service's `generation` plus the scalar
    /// selection; the caller keeps it only when the generation is newer
    /// than what it already holds.
    pub fn model_get(&self) -> Result<ModelInfo, AccountError> {
        let answer: serde_json::Value =
            self.request(serde_json::json!({"op": "model_get"}), false)?;
        serde_json::from_value(answer)
            .map_err(|_| AccountError::new(ErrorCode::ProtocolError, false))
    }
    /// Select one model by its service id.
    ///
    /// The id comes from a `model_get` listing or the picker, never a
    /// guess; the answer reports the generation the service settled on.
    pub fn model_set(&self, model: &str) -> Result<ModelInfo, AccountError> {
        if model.is_empty() || model.len() > 256 || model.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let answer: serde_json::Value = self.request(
            serde_json::json!({
                "op": "model_set",
                "model": model,
            }),
            false,
        )?;
        serde_json::from_value(answer)
            .map_err(|_| AccountError::new(ErrorCode::ProtocolError, false))
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
    /// and the bridge projects one page into narrow rows so no raw
    /// node payload reaches the window.
    pub fn artifacts(&self) -> Result<ArtifactList, AccountError> {
        self.request(serde_json::json!({"op": "artifacts"}), false)
    }
    /// Fetch the Library page after a cursor a previous page
    /// issued, with the references already shown.
    ///
    /// The cursor is opaque: it is handed back verbatim, never
    /// interpreted. The seen references are the proof material --
    /// the bridge accepts a candidate page only when its rows
    /// demonstrably continue the list, so a paging parameter the
    /// service ignores cannot pass the first page off as the
    /// second. A page whose `paging` reads "unavailable" carries
    /// no rows: the service did not page.
    pub fn artifacts_page(
        &self,
        cursor: &str,
        seen_refs: &[String],
    ) -> Result<ArtifactList, AccountError> {
        if cursor.is_empty() || cursor.len() > 2048 || cursor.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        if seen_refs.len() > 500 {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "artifacts", "cursor": cursor, "seen_refs": seen_refs}),
            false,
        )
    }
    /// Stop sharing exactly one space, once.
    ///
    /// The slug must be one the Library listing's open target
    /// published, and the bridge makes a single path-only DELETE,
    /// so one confirmed press maps to one write. Whether the
    /// space then reads as unshared is proven by re-reading the
    /// Library, never by this answer.
    pub fn space_unshare(&self, slug: &str) -> Result<SpaceUnshareAnswer, AccountError> {
        if slug.is_empty() || slug.len() > 256 || slug.contains('\0') || slug.contains('/') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "space_unshare", "slug": slug}),
            false,
        )
    }
    /// Read one space's HTML export for an in-app preview.
    ///
    /// The slug must be one the Library listing's open target
    /// published; the bridge makes a single raw GET on the
    /// recorded binary route, so the export itself is the only
    /// thing that travels. An oversized export arrives as its
    /// size and a truncation flag, never as a broken frame.
    pub fn space_export(&self, slug: &str) -> Result<SpaceExportAnswer, AccountError> {
        if slug.is_empty() || slug.len() > 256 || slug.contains('\0') || slug.contains('/') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "space_export", "slug": slug}),
            false,
        )
    }
    /// Search the agent's memory for the reader's own words, read-only.
    ///
    /// `memory.search` takes its query in a JSON body — a live probe
    /// (Nov 14, 2026) refused `q`, query parameters, an empty body and an
    /// empty query — so exactly that field is sent. Nothing here writes:
    /// the route table records no memory delete or edit.
    pub fn memory_search(&self, query: &str) -> Result<MemorySearch, AccountError> {
        if query.is_empty() || query.len() > 512 || query.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "memory_search", "query": query}),
            false,
        )
    }

    /// Resolve one remembered passage by the `memory_uri` a hit published.
    ///
    /// The address is never assembled from a path: only the service's own
    /// URI is accepted here, and the bridge refuses anything that is not one.
    pub fn memory_get(&self, memory_uri: &str) -> Result<MemoryEntry, AccountError> {
        if !memory_uri.starts_with("memory://")
            || memory_uri.len() > 2048
            || memory_uri.contains('\0')
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "memory_get", "memory_uri": memory_uri}),
            false,
        )
    }

    /// Search the account's conversations for the reader's own
    /// words, read-only.
    ///
    /// The route is recorded; its query parameter's name is not,
    /// so the bridge probes the conventional names and uses the
    /// first the service answers (see the bridge's search
    /// dispatch). Results carry the conversation they belong to,
    /// so a result can be opened.
    pub fn search(&self, query: &str) -> Result<ServiceListing, AccountError> {
        if query.is_empty() || query.len() > 512 || query.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(serde_json::json!({"op": "search", "query": query}), false)
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
    /// One page of the issued-access inventory, the web UI's grants.list.
    ///
    /// The cursor travels opaque; `None` reads the first page.
    pub fn grants(
        &self,
        limit: usize,
        cursor: Option<&str>,
        include_revoked: bool,
    ) -> Result<GrantsPage, AccountError> {
        if !(1..=200).contains(&limit) {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        if let Some(cursor) = cursor
            && (cursor.is_empty() || cursor.len() > 4096)
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({
                "op": "grants",
                "limit": limit,
                "cursor": cursor,
                "include_revoked": include_revoked,
            }),
            false,
        )
    }
    /// One connector scope's allow/ask/deny map. Read-only.
    pub fn scoped_permissions(
        &self,
        scope_type: &str,
        scope_id: &str,
        connectors: &[String],
    ) -> Result<ScopedPermissions, AccountError> {
        if scope_type.is_empty()
            || scope_type.len() > 256
            || scope_id.is_empty()
            || scope_id.len() > 256
            || connectors.len() > 32
            || connectors.iter().any(|c| c.is_empty() || c.len() > 128)
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let answer: serde_json::Value = self.request(
            serde_json::json!({
                "op": "scoped_permissions",
                "scope_type": scope_type,
                "scope_id": scope_id,
                "connectors": connectors,
            }),
            false,
        )?;
        let mut answer = answer;
        answer["scope_type"] = serde_json::Value::String(scope_type.to_owned());
        answer["scope_id"] = serde_json::Value::String(scope_id.to_owned());
        serde_json::from_value(answer)
            .map_err(|_| AccountError::new(ErrorCode::ProtocolError, false))
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
    /// Read the service's durable policy settings. Read-only.
    pub fn policy_settings(&self) -> Result<PolicySettings, AccountError> {
        self.request(serde_json::json!({"op": "policy_settings"}), false)
    }
    /// Reset exactly one connector's grants, once.
    ///
    /// The name must be one the policy settings' own connectors
    /// section published, and the bridge makes a single path-only
    /// DELETE, so one confirmed press maps to one reset. Whether
    /// the connector then reads as reset is proven by re-reading
    /// the settings, never by this answer.
    pub fn connector_reset(&self, connector: &str) -> Result<ConnectorResetAnswer, AccountError> {
        let valid = !connector.is_empty()
            && connector.len() <= 128
            && connector
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphanumeric())
            && connector
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !valid {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "connector_reset", "connector": connector}),
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
    /// Run one listed schedule, once. Acceptance is not a run: the caller
    /// proves the run by re-reading tasks.runs.
    pub fn tasks_run(&self, job_id: &str) -> Result<RunAnswer, AccountError> {
        if job_id.is_empty() || job_id.len() > 256 || job_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "tasks_run", "job_id": job_id}),
            false,
        )
    }
    /// Change one connector+method mode, guarded by the revision the read
    /// published. The caller proves the change by re-reading the scope.
    pub fn scoped_update(&self, update: &ScopedUpdate) -> Result<ScopedUpdateAnswer, AccountError> {
        if update.scope_type.is_empty()
            || update.scope_type.len() > 256
            || update.scope_id.is_empty()
            || update.scope_id.len() > 256
            || update.connector.is_empty()
            || update.connector.len() > 256
            || update.method.is_empty()
            || update.method.len() > 256
            || !matches!(update.mode.as_str(), "allow" | "ask" | "deny")
            || update.display_name.is_empty()
            || update.display_name.len() > 256
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({
                "op": "scoped_update",
                "scope_type": update.scope_type,
                "scope_id": update.scope_id,
                "expected_revision": update.expected_revision,
                "connector": update.connector,
                "method": update.method,
                "mode": update.mode,
                "display_name": update.display_name,
            }),
            false,
        )
    }
    /// Delete one message or one turn. Acceptance is not absence: the
    /// caller re-reads history to prove the row is gone.
    pub fn chat_delete(
        &self,
        target: &str,
        message_id: &str,
    ) -> Result<ChatDeleteAnswer, AccountError> {
        if !matches!(target, "message" | "turn") || message_id.is_empty() || message_id.len() > 4096
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({
                "op": "chat_delete",
                "target": target,
                "message_id": message_id,
            }),
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
    /// Mark exactly one conversation read, once.
    ///
    /// The id must be one the sessions listing published, and the
    /// bridge makes a single request in the recorded consumer's
    /// shape (the id as a path param, no body), so opening a chat
    /// maps to one write. Whether the thread then reads as seen
    /// is proven by re-reading the listing, never by this answer.
    pub fn mark_seen(&self, thread_id: &str) -> Result<MarkSeenAnswer, AccountError> {
        if thread_id.is_empty() || thread_id.len() > 256 || thread_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "mark_seen", "thread_id": thread_id}),
            false,
        )
    }
    /// Run the attach probe in one named thread, once: the
    /// bridge writes one small file service-side, sends one
    /// message referencing it in that thread, and verifies the
    /// outcome from the thread's history. A designated scratch
    /// chat is the intended target; the bridge refuses to run
    /// without a named thread, so the probe can never land in
    /// whichever conversation is merely open.
    pub fn attach_probe(&self, thread: &str) -> Result<AttachProbeAnswer, AccountError> {
        if thread.is_empty() || thread.len() > 256 || thread.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "attach_probe", "thread": thread}),
            false,
        )
    }

    /// One bounded listen on a thread's event stream. The probe
    /// is how the fill-lag question gets answered with evidence:
    /// whether `chat.stream_events` carries a side chat's reply
    /// deltas, and under what request shape. One call, one
    /// listen; the bridge tries its candidate bodies in order
    /// and reports only acceptance and an event count.
    pub fn stream_probe(&self, thread: &str) -> Result<StreamProbeAnswer, AccountError> {
        if thread.is_empty() || thread.len() > 256 || thread.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "stream_probe", "thread": thread}),
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
    /// Delete exactly one feed unit, once.
    ///
    /// The id must be one the feed listing published, and the
    /// bridge makes a single request, so one confirmed press maps
    /// to one deletion. Whether the unit is actually gone
    /// afterwards is proven by re-reading the feed, never by this
    /// answer alone.
    pub fn feed_unit_delete(&self, unit_id: &str) -> Result<FeedUnitDeleteAnswer, AccountError> {
        if unit_id.is_empty() || unit_id.len() > 256 || unit_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "feed_unit_delete", "unit_id": unit_id}),
            false,
        )
    }
    /// Change one Library artifact's pin state, once.
    ///
    /// The reference must be one the Library listing published, and
    /// the flag a real boolean. The bridge makes a single request in
    /// the live-probed shape, so one press maps to one change.
    /// Whether the row now shows the state is proven by re-reading
    /// the Library, never by this answer alone.
    pub fn artifact_pin(
        &self,
        artifact_ref: &str,
        pinned: bool,
    ) -> Result<ArtifactPinAnswer, AccountError> {
        if artifact_ref.is_empty() || artifact_ref.len() > 256 || artifact_ref.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "artifact_pin", "artifact_ref": artifact_ref, "pinned": pinned}),
            false,
        )
    }
    /// Delete exactly one Library file, once.
    ///
    /// The path must be one the Library listing published (relative;
    /// absolute paths are refused), and the bridge makes a single
    /// request in the live-probed shape, so one confirmed press maps
    /// to one deletion. Whether the row is actually gone is proven
    /// by re-reading the Library, never by this answer alone.
    pub fn file_delete(&self, path: &str) -> Result<FileDeleteAnswer, AccountError> {
        if path.is_empty() || path.len() > 1024 || path.contains('\0') || path.starts_with('/') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "file_delete", "path": path}),
            false,
        )
    }
    /// Record one "More like this" on an idea, once.
    ///
    /// The id must be one the ideas listing published, and the
    /// bridge makes a single request in the live-probed shape, so
    /// one press maps to one engagement. Ranking is the service's
    /// business; this answer claims acceptance only.
    pub fn idea_engagement(&self, id: &str) -> Result<IdeaEngagementAnswer, AccountError> {
        if id.is_empty() || id.len() > 256 || id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "idea_engagement", "id": id}),
            false,
        )
    }
    /// Record one "Not interested" on an idea, once.
    ///
    /// The id must be one the ideas listing published, and the
    /// bridge makes a single request in the live-probed shape, so
    /// one press maps to one dismissal. Whether the card is gone
    /// is proven by re-reading the ideas listing, never by this
    /// answer alone.
    pub fn idea_feedback(&self, id: &str) -> Result<IdeaFeedbackAnswer, AccountError> {
        if id.is_empty() || id.len() > 256 || id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(serde_json::json!({"op": "idea_feedback", "id": id}), false)
    }
    /// Rename exactly one goal, once.
    ///
    /// The id must be one the goals listing published and the
    /// title the reader's own non-blank words; the bridge makes a
    /// single PATCH in the live-probed shape, so one confirmed
    /// press maps to one rename. Whether the row shows it is
    /// proven by re-reading the goals listing.
    pub fn goal_rename(
        &self,
        goal_id: &str,
        title: &str,
    ) -> Result<GoalRenameAnswer, AccountError> {
        if goal_id.is_empty() || goal_id.len() > 256 || goal_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        if title.trim().is_empty() || title.len() > 256 || title.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "goal_rename", "goal_id": goal_id, "title": title}),
            false,
        )
    }
    /// Mark one goal complete, or reopen it, once.
    ///
    /// The id must be one the goals listing published, and the
    /// bridge makes a single PATCH in the live-probed shape, so
    /// one press maps to one status flip. Whether the row shows
    /// the state is proven by re-reading the goals listing.
    pub fn goal_set_complete(
        &self,
        goal_id: &str,
        completed: bool,
    ) -> Result<GoalSetCompleteAnswer, AccountError> {
        if goal_id.is_empty() || goal_id.len() > 256 || goal_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "goal_set_complete", "goal_id": goal_id, "completed": completed}),
            false,
        )
    }
    /// Replace the feed prompt's words, once.
    ///
    /// The words are the reader's own non-blank text; the bridge
    /// makes a single PATCH in the live-probed shape, so one
    /// confirmed press maps to one replacement. Whether the card
    /// shows the new words is proven by re-reading the feed.
    pub fn feed_prompt_update(&self, prompt: &str) -> Result<FeedPromptUpdateAnswer, AccountError> {
        if prompt.trim().is_empty() || prompt.len() > 4000 || prompt.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "feed_prompt_update", "prompt": prompt}),
            false,
        )
    }
    /// Read one goal's detail: the goal, its updates, its links.
    ///
    /// The id must be one the goals listing published. Read-only;
    /// the answer is the bridge-shaped detail, never raw payload.
    pub fn goal_get(&self, goal_id: &str) -> Result<GoalGetAnswer, AccountError> {
        if goal_id.is_empty() || goal_id.len() > 256 || goal_id.contains('\0') {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "goal_get", "goal_id": goal_id}),
            false,
        )
    }
    /// Create one subgoal under a goal, once.
    ///
    /// The parent id must be one the goals listing published and
    /// the words the reader's own; the bridge makes a single POST
    /// in the live-probed shape, so one confirmed press maps to
    /// one child goal. Whether the child exists is proven by
    /// re-reading the goals listing, never by this answer alone.
    pub fn goal_add_subgoal(
        &self,
        parent_goal_id: &str,
        title: &str,
        summary: &str,
        description: &str,
        emoji: &str,
    ) -> Result<GoalAddSubgoalAnswer, AccountError> {
        if parent_goal_id.is_empty() || parent_goal_id.len() > 256 || parent_goal_id.contains('\0')
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        for (value, limit) in [
            (title, 256),
            (summary, 500),
            (description, 4000),
            (emoji, 16),
        ] {
            if value.trim().is_empty() || value.len() > limit || value.contains('\0') {
                return Err(AccountError::new(ErrorCode::InvalidRequest, false));
            }
        }
        self.request(
            serde_json::json!({"op": "goal_add_subgoal", "parent_goal_id": parent_goal_id,
                               "title": title, "summary": summary,
                               "description": description, "emoji": emoji}),
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

    /// Voice dictation: the recorded bytes go out raw with the rate in
    /// the query, per the route template. Only the rates the client
    /// records; empty audio and oversize clips are refused locally.
    pub fn dictate(
        &self,
        pcm: &[u8],
        sample_rate_hz: u32,
    ) -> Result<DictationVerdict, AccountError> {
        if pcm.is_empty()
            || pcm.len() > 8 * 1024 * 1024
            || (sample_rate_hz != 8000 && sample_rate_hz != 16000)
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let verdict: DictationVerdict = self.request(
            serde_json::json!({
                "op": "dictate",
                "pcm_b64": encode_b64(pcm),
                "sample_rate_hz": sample_rate_hz,
            }),
            true,
        )?;
        Ok(verdict)
    }

    /// A link-preview card for one url: whatever scalar fields the
    /// service returned. Only http(s) urls travel; anything else is a
    /// local refusal, never a service round trip.
    pub fn preview_link(&self, url: &str) -> Result<LinkPreview, AccountError> {
        if url.len() > 2048 || !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let preview: LinkPreview = self.request(
            serde_json::json!({
                "op": "link_preview",
                "url": url,
            }),
            true,
        )?;
        Ok(preview)
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
    /// Send one message carrying pasted images. The bytes never
    /// travel the request: each image is written to the staging
    /// directory under the account config dir, the bridge reads
    /// it from there, writes it service-side and sends the file
    /// items, and the staging files are removed again whatever
    /// the outcome. The receipt is the send receipt: `confirmed`
    /// means the landed row carries every image with the byte
    /// count that was sent; `stored_mismatch` says it landed but
    /// stored differently.
    pub fn send_images_verified(
        &self,
        thread: Option<&str>,
        message: &str,
        images: &[StagedImage],
        vm_id: &str,
        identity_name: &str,
    ) -> Result<DispatchReceipt, AccountError> {
        validate_thread(thread)?;
        if message.len() > 512 * 1024 || images.is_empty() || images.len() > 4 {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        for image in images {
            let name = &image.name;
            let stem_ok = name.starts_with("pasted-")
                && name.len() <= 128
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
            let ext_ok = [".png", ".jpg", ".jpeg", ".gif", ".webp"]
                .iter()
                .any(|ext| name.ends_with(ext));
            if !stem_ok || !ext_ok || image.bytes.is_empty() || image.bytes.len() > 8 * 1024 * 1024
            {
                return Err(AccountError::new(ErrorCode::InvalidRequest, false));
            }
        }
        if vm_id.is_empty()
            || vm_id.len() > 256
            || identity_name.is_empty()
            || identity_name.len() > 256
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let staging = self.config.config_dir.join("paste-staging");
        let staged_paths: Vec<std::path::PathBuf> = images
            .iter()
            .map(|image| staging.join(&image.name))
            .collect();
        let outcome = (|| {
            std::fs::create_dir_all(&staging)
                .map_err(|_| AccountError::new(ErrorCode::ProtocolError, false))?;
            for (image, path) in images.iter().zip(&staged_paths) {
                std::fs::write(path, &image.bytes)
                    .map_err(|_| AccountError::new(ErrorCode::ProtocolError, false))?;
            }
            let request = serde_json::json!({
                "op": "send_images",
                "thread": thread,
                "message": message,
                "images": images.iter().map(|image| image.name.clone())
                    .collect::<Vec<_>>(),
                "expect_vm_id": vm_id,
                "expect_identity_name": identity_name,
            });
            let receipt: DispatchReceipt = self.request(request, true)?;
            if receipt.confirmed
                && (!receipt.sent || receipt.message_id.as_deref().is_none_or(str::is_empty))
            {
                return Err(AccountError::new(ErrorCode::ProtocolError, true));
            }
            Ok(receipt)
        })();
        for path in &staged_paths {
            let _ = std::fs::remove_file(path);
        }
        outcome
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

    #[test]
    fn subscribe_rejects_a_wild_method_or_body_before_touching_the_service() {
        // Validation fires before any subprocess spawns, so these paths
        // are offline-safe: no request leaves this process.
        use super::{AccountClient, BridgeConfig, ErrorCode};
        use std::path::PathBuf;
        let client = AccountClient::new(BridgeConfig {
            python: PathBuf::from("/unused/python"),
            script: PathBuf::from("/unused/bridge.py"),
            config_dir: PathBuf::from("/unused"),
        });
        for (method, body, limit) in [
            ("", serde_json::json!({}), 50),
            ("activity.subscribe", serde_json::json!([1, 2]), 50),
            ("activity.subscribe", serde_json::json!({}), 0),
        ] {
            let error = client
                .subscribe(method, body, limit)
                .expect_err("validation must fire before any subprocess");
            assert_eq!(error.code, ErrorCode::InvalidRequest);
        }
    }

    #[test]
    fn model_set_rejects_a_wild_model_id_before_touching_the_service() {
        // Same offline-safe shape as subscribe: validation fires before
        // any subprocess spawns.
        use super::{AccountClient, BridgeConfig, ErrorCode};
        use std::path::PathBuf;
        let client = AccountClient::new(BridgeConfig {
            python: PathBuf::from("/unused/python"),
            script: PathBuf::from("/unused/bridge.py"),
            config_dir: PathBuf::from("/unused"),
        });
        for model in ["".to_string(), "m\x000".to_string(), "m".repeat(257)] {
            let error = client
                .model_set(&model)
                .expect_err("validation must fire before any subprocess");
            assert_eq!(error.code, ErrorCode::InvalidRequest);
        }
        let info: super::ModelInfo = serde_json::from_value(serde_json::json!({"generation": 7}))
            .expect("sparse model answers keep parsing");
        assert_eq!(info.generation, Some(7));
        assert!(info.available_models.is_empty());
    }

    #[test]
    fn base64_vectors_round_trip_without_a_dependency() {
        assert_eq!(super::encode_b64(b""), "");
        assert_eq!(super::encode_b64(b"f"), "Zg==");
        assert_eq!(super::encode_b64(b"fo"), "Zm8=");
        assert_eq!(super::encode_b64(b"foo"), "Zm9v");
        assert_eq!(super::encode_b64(b"foobar"), "Zm9vYmFy");
        assert_eq!(super::encode_b64(&[0xff, 0x00, 0x80]), "/wCA");
    }

    #[test]
    fn dictate_validation_fires_before_any_subprocess() {
        use std::path::PathBuf;
        let client = super::AccountClient::new(super::BridgeConfig {
            python: PathBuf::from("/unused/python"),
            script: PathBuf::from("/unused/bridge.py"),
            config_dir: PathBuf::from("/unused"),
        });
        for (pcm, rate) in [
            (vec![], 16000),
            (vec![0u8; 8 * 1024 * 1024 + 1], 16000),
            (vec![1u8], 44100),
        ] {
            let error = client
                .dictate(&pcm, rate)
                .expect_err("validation must fire before any subprocess");
            assert_eq!(error.code, super::ErrorCode::InvalidRequest);
        }
        let verdict: super::DictationVerdict =
            serde_json::from_value(serde_json::json!({"transcript": "hi"}))
                .expect("sparse verdicts keep parsing");
        assert_eq!(verdict.transcript.as_deref(), Some("hi"));
        assert!(verdict.keys.is_empty());
    }

    #[test]
    fn preview_link_validation_fires_before_any_subprocess() {
        use std::path::PathBuf;
        let client = super::AccountClient::new(super::BridgeConfig {
            python: PathBuf::from("/unused/python"),
            script: PathBuf::from("/unused/bridge.py"),
            config_dir: PathBuf::from("/unused"),
        });
        for url in ["", "ftp://example.com/x", &"x".repeat(2049)] {
            let error = client
                .preview_link(url)
                .expect_err("validation must fire before any subprocess");
            assert_eq!(error.code, super::ErrorCode::InvalidRequest);
        }
        let preview: super::LinkPreview = serde_json::from_value(serde_json::json!({"title": "T"}))
            .expect("sparse previews keep parsing");
        assert_eq!(preview.title.as_deref(), Some("T"));
        assert!(preview.image_url.is_none());
    }

    #[test]
    fn unsupported_method_errors_parse_and_name_themselves() {
        // The bridge emits this code for well-formed but unheld methods;
        // the app must read it back instead of failing the envelope.
        let error: super::AccountError = serde_json::from_value(serde_json::json!({
            "code": "unsupported_method", "uncertain": false
        }))
        .expect("new bridge codes keep parsing");
        assert_eq!(error.code, super::ErrorCode::UnsupportedMethod);
        assert!(format!("{error}").contains("does not support"));
    }
}
